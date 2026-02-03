use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "gsh")]
#[command(about = "CLI client for gsh - an agentic shell")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// LLM provider (anthropic, openai, moonshot, ollama)
    #[arg(long, short = 'p', global = true)]
    provider: Option<String>,

    /// Model to use (overrides provider default)
    #[arg(long, short = 'm', global = true)]
    model: Option<String>,

    /// Run a flow instead of single agent
    #[arg(long, short = 'f', global = true)]
    flow: Option<String>,

    /// One-shot prompt (shorthand for `gsh prompt "..."`)
    #[arg(trailing_var_arg = true)]
    query: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Send a one-shot prompt to the LLM
    Prompt {
        /// The prompt text
        query: Vec<String>,
        /// Don't stream output
        #[arg(long)]
        no_stream: bool,
    },
    /// Start an interactive chat session
    Chat {
        /// Resume an existing session
        #[arg(long)]
        session: Option<String>,
    },
    /// List running subagents
    Agents,
    /// Attach to a subagent's tmux session
    Attach {
        /// Agent ID or session name
        agent: String,
    },
    /// View logs from a subagent session
    Logs {
        /// Agent ID or session name
        agent: String,
        /// Include full scrollback history
        #[arg(long, short = 'a')]
        all: bool,
        /// Follow output (like tail -f)
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Kill a subagent
    Kill {
        /// Agent ID or session name
        agent: String,
    },
    /// Check daemon status
    Status,
    /// Stop the daemon
    Stop,
}

// Protocol messages (simplified versions from daemon)
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ShellMessage {
    Prompt {
        query: String,
        cwd: String,
        session_id: Option<String>,
        stream: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
    },
    ChatStart {
        cwd: String,
        session_id: Option<String>,
    },
    ChatMessage {
        session_id: String,
        message: String,
    },
    ChatEnd {
        session_id: String,
    },
    ListAgents,
    Ping,
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DaemonMessage {
    Ack,
    Pong { uptime_secs: u64, version: String },
    TextChunk { text: String, done: bool },
    ToolUse { tool: String, input: serde_json::Value },
    ToolResult { tool: String, output: String, success: bool },
    Response { text: String, session_id: String },
    Error { message: String, code: Option<String> },
    ChatStarted { session_id: String },
    AgentList { agents: Vec<AgentInfo> },
    ShuttingDown,
}

#[derive(Debug, Deserialize)]
struct AgentInfo {
    agent_id: u64,
    session_name: String,
    task: Option<String>,
    cwd: Option<String>,
}

fn get_socket_path() -> PathBuf {
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    PathBuf::from(format!("/tmp/gsh-{}.sock", user))
}

fn get_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle one-shot query without subcommand
    if !cli.query.is_empty() {
        let query = get_query_from_args_or_stdin(&cli.query)?;
        return run_prompt(&query, true, cli.provider, cli.model, cli.flow).await;
    }

    match cli.command {
        Some(Commands::Prompt { query, no_stream }) => {
            let query = get_query_from_args_or_stdin(&query)?;
            run_prompt(&query, !no_stream, cli.provider, cli.model, cli.flow).await
        }
        Some(Commands::Chat { session }) => {
            run_chat(session).await
        }
        Some(Commands::Agents) => {
            run_agents().await
        }
        Some(Commands::Attach { agent }) => {
            run_attach(&agent)
        }
        Some(Commands::Logs { agent, all, follow }) => {
            run_logs(&agent, all, follow).await
        }
        Some(Commands::Kill { agent }) => {
            run_kill(&agent).await
        }
        Some(Commands::Status) => {
            run_status().await
        }
        Some(Commands::Stop) => {
            run_stop().await
        }
        None => {
            // No command - check if stdin has data
            let stdin = io::stdin();
            if !stdin.is_terminal() {
                // Read from stdin
                let query = read_stdin()?;
                if !query.is_empty() {
                    return run_prompt(&query, true, cli.provider, cli.model, cli.flow).await;
                }
            }

            // Show help
            println!("gsh v{}", VERSION);
            println!();
            println!("Usage: gsh <query>              Send a prompt (no quotes needed)");
            println!("       echo 'prompt' | gsh      Read prompt from stdin");
            println!("       gsh - < file.txt         Read prompt from file");
            println!("       gsh chat                 Start interactive chat");
            println!("       gsh agents               List running subagents");
            println!("       gsh status               Check daemon status");
            println!("       gsh stop                 Stop the daemon");
            println!();
            println!("Options:");
            println!("       -p, --provider <name>    LLM provider (anthropic, openai, moonshot, ollama)");
            println!("       -m, --model <model>      Model override");
            println!("       -f, --flow <name>        Run a flow");
            println!();
            println!("Run 'gsh --help' for more options");
            Ok(())
        }
    }
}

/// Read query from args, or from stdin if args is ["-"] or empty with piped input
fn get_query_from_args_or_stdin(args: &[String]) -> Result<String> {
    // If single arg is "-", read from stdin
    if args.len() == 1 && args[0] == "-" {
        return read_stdin();
    }

    let query = args.join(" ");

    // If query is empty and stdin is not a terminal, read from stdin
    if query.is_empty() && !io::stdin().is_terminal() {
        return read_stdin();
    }

    Ok(query)
}

/// Read all of stdin into a string
fn read_stdin() -> Result<String> {
    let stdin = io::stdin();
    let mut lines = Vec::new();
    for line in stdin.lock().lines() {
        lines.push(line.context("Failed to read from stdin")?);
    }
    Ok(lines.join("\n").trim().to_string())
}

async fn connect() -> Result<UnixStream> {
    let socket_path = get_socket_path();
    UnixStream::connect(&socket_path)
        .await
        .with_context(|| {
            format!(
                "Failed to connect to daemon at {}. Is gsh-daemon running?",
                socket_path.display()
            )
        })
}

async fn run_prompt(
    query: &str,
    stream: bool,
    provider: Option<String>,
    model: Option<String>,
    flow: Option<String>,
) -> Result<()> {
    let stream_conn = connect().await?;
    let (reader, mut writer) = stream_conn.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Prompt {
        query: query.to_string(),
        cwd: get_cwd(),
        session_id: None,
        stream,
        provider,
        model,
        flow,
    };

    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    let mut stdout = io::stdout();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let response: DaemonMessage = serde_json::from_str(line.trim())?;

        match response {
            DaemonMessage::TextChunk { text, done } => {
                if !text.is_empty() {
                    print!("{}", text);
                    stdout.flush()?;
                }
                if done {
                    println!();
                    break;
                }
            }
            DaemonMessage::ToolUse { tool, input } => {
                eprintln!("\n[Using tool: {}]", tool);
            }
            DaemonMessage::ToolResult { tool, output, success } => {
                let status = if success { "OK" } else { "FAILED" };
                eprintln!("[Tool {} {}]", tool, status);
            }
            DaemonMessage::Response { text, .. } => {
                println!("{}", text);
                break;
            }
            DaemonMessage::Error { message, .. } => {
                eprintln!("Error: {}", message);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

async fn run_chat(session_id: Option<String>) -> Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Start chat session
    let msg = ShellMessage::ChatStart {
        cwd: get_cwd(),
        session_id: session_id.clone(),
    };
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;
    let session_id = match response {
        DaemonMessage::ChatStarted { session_id } => {
            println!("Chat session started ({})", &session_id[..8]);
            session_id
        }
        DaemonMessage::Error { message, .. } => {
            anyhow::bail!("Failed to start chat: {}", message);
        }
        _ => {
            anyhow::bail!("Unexpected response");
        }
    };

    println!("Type 'exit' or Ctrl+D to end the session.\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        print!("> ");
        stdout.flush()?;

        let mut input = String::new();
        match stdin.read_line(&mut input) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("Input error: {}", e);
                break;
            }
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if input == "exit" || input == "quit" {
            break;
        }

        // Send message
        let msg = ShellMessage::ChatMessage {
            session_id: session_id.clone(),
            message: input.to_string(),
        };
        let msg_str = serde_json::to_string(&msg)? + "\n";
        writer.write_all(msg_str.as_bytes()).await?;

        // Receive response
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }

            let response: DaemonMessage = serde_json::from_str(line.trim())?;

            match response {
                DaemonMessage::TextChunk { text, done } => {
                    if !text.is_empty() {
                        print!("{}", text);
                        stdout.flush()?;
                    }
                    if done {
                        println!("\n");
                        break;
                    }
                }
                DaemonMessage::ToolUse { tool, .. } => {
                    eprintln!("\n[Using tool: {}]", tool);
                }
                DaemonMessage::ToolResult { tool, success, .. } => {
                    let status = if success { "OK" } else { "FAILED" };
                    eprintln!("[Tool {} {}]", tool, status);
                }
                DaemonMessage::Error { message, .. } => {
                    eprintln!("Error: {}", message);
                    break;
                }
                _ => {}
            }
        }
    }

    // End session
    let msg = ShellMessage::ChatEnd {
        session_id: session_id.clone(),
    };
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    println!("Chat session ended.");
    Ok(())
}

async fn run_agents() -> Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::ListAgents;
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;

    match response {
        DaemonMessage::AgentList { agents } => {
            if agents.is_empty() {
                println!("No running agents");
            } else {
                println!("Running agents ({}):", agents.len());
                for agent in agents {
                    let task = agent.task.as_deref().unwrap_or("unknown");
                    let task_display: String = task.chars().take(40).collect();
                    println!("  [{:04}] {} - {}", agent.agent_id, agent.session_name, task_display);
                }
            }
        }
        DaemonMessage::Error { message, .. } => {
            eprintln!("Error: {}", message);
        }
        _ => {
            eprintln!("Unexpected response");
        }
    }

    Ok(())
}

async fn run_status() -> Result<()> {
    let stream = match connect().await {
        Ok(s) => s,
        Err(_) => {
            println!("Daemon not running");
            return Ok(());
        }
    };

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Ping;
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;

    if let DaemonMessage::Pong { uptime_secs, version } = response {
        println!("Daemon running");
        println!("  Version: {}", version);
        println!("  Uptime: {}s", uptime_secs);
        println!("  Socket: {}", get_socket_path().display());
    }

    Ok(())
}

async fn run_stop() -> Result<()> {
    let stream = match connect().await {
        Ok(s) => s,
        Err(_) => {
            println!("Daemon not running");
            return Ok(());
        }
    };

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Shutdown;
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    println!("Daemon stopped");
    Ok(())
}

/// Resolve agent identifier to session name
/// Accepts: agent ID (e.g., "1", "0001") or full session name
fn resolve_agent_session(agent: &str) -> String {
    // If it looks like a number, format as session name
    if let Ok(id) = agent.parse::<u64>() {
        format!("gsh-agent-{:04}", id)
    } else if agent.starts_with("gsh-") {
        // Already a full session name
        agent.to_string()
    } else {
        // Assume it's a partial name, add prefix
        format!("gsh-{}", agent)
    }
}

fn run_attach(agent: &str) -> Result<()> {
    use std::process::Command;

    let session = resolve_agent_session(agent);

    // Check if session exists
    let exists = Command::new("tmux")
        .args(["has-session", "-t", &session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        anyhow::bail!("Session '{}' not found. Run 'gsh agents' to list available sessions.", session);
    }

    // Check if we're inside tmux
    let inside_tmux = std::env::var("TMUX").is_ok();

    if inside_tmux {
        // Switch client
        let status = Command::new("tmux")
            .args(["switch-client", "-t", &session])
            .status()
            .context("Failed to switch tmux client")?;

        if !status.success() {
            anyhow::bail!("Failed to switch to session '{}'", session);
        }
    } else {
        // Attach to session - this replaces the current process
        let err = exec::Command::new("tmux")
            .args(&["attach-session", "-t", &session])
            .exec();

        // exec() only returns if there was an error
        anyhow::bail!("Failed to attach to session '{}': {}", session, err);
    }

    Ok(())
}

async fn run_logs(agent: &str, include_history: bool, follow: bool) -> Result<()> {
    use std::process::Command;
    use tokio::time::{sleep, Duration};

    let session = resolve_agent_session(agent);

    // Check if session exists
    let exists = Command::new("tmux")
        .args(["has-session", "-t", &session])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !exists {
        anyhow::bail!("Session '{}' not found. Run 'gsh agents' to list available sessions.", session);
    }

    if follow {
        // Follow mode - continuously capture and print new output
        println!("Following output from '{}' (Ctrl+C to stop)...\n", session);

        let mut last_line_count = 0;

        loop {
            let mut cmd = Command::new("tmux");
            cmd.args(["capture-pane", "-t", &session, "-p"]);

            if include_history {
                cmd.args(["-S", "-"]);
            }

            let output = cmd.output().context("Failed to capture pane")?;

            if output.status.success() {
                let content = String::from_utf8_lossy(&output.stdout);
                let lines: Vec<&str> = content.lines().collect();
                let current_count = lines.len();

                // Print only new lines
                if current_count > last_line_count {
                    for line in &lines[last_line_count..] {
                        println!("{}", line);
                    }
                    last_line_count = current_count;
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    } else {
        // One-shot capture
        let mut cmd = Command::new("tmux");
        cmd.args(["capture-pane", "-t", &session, "-p"]);

        if include_history {
            cmd.args(["-S", "-"]);
        }

        let output = cmd.output().context("Failed to capture pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to capture logs from '{}': {}", session, stderr);
        }

        let content = String::from_utf8_lossy(&output.stdout);
        print!("{}", content);
    }

    Ok(())
}

async fn run_kill(agent: &str) -> Result<()> {
    use std::process::Command;

    let session = resolve_agent_session(agent);

    let output = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .output()
        .context("Failed to kill session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("can't find session") {
            println!("Session '{}' not found", session);
        } else {
            anyhow::bail!("Failed to kill session '{}': {}", session, stderr);
        }
    } else {
        println!("Killed session '{}'", session);
    }

    Ok(())
}
