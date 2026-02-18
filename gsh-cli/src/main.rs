use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::task::JoinHandle;

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
    /// List or manage saved chat sessions
    Sessions {
        /// Delete a session by ID (prefix ok)
        #[arg(long)]
        delete: Option<String>,
        /// Maximum number of sessions to show
        #[arg(long, short = 'n', default_value = "20")]
        limit: usize,
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
        #[serde(skip_serializing_if = "Option::is_none")]
        env_info: Option<EnvInfo>,
    },
    ChatStart {
        cwd: String,
        session_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        env_info: Option<EnvInfo>,
    },
    ChatMessage {
        session_id: String,
        message: String,
    },
    ChatEnd {
        session_id: String,
    },
    ListAgents,
    ListSessions {
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    DeleteSession {
        session_id: String,
    },
    Ping,
    Shutdown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(dead_code)]
enum DaemonMessage {
    Ack,
    Pong { uptime_secs: u64, version: String },
    TextChunk { text: String, done: bool },
    ToolUse { tool: String, input: serde_json::Value },
    ToolResult { tool: String, output: String, success: bool },
    Response { text: String, session_id: String },
    Error { message: String, code: Option<String> },
    ChatStarted {
        session_id: String,
        #[serde(default)]
        resumed: bool,
        #[serde(default)]
        message_count: usize,
    },
    AgentList { agents: Vec<AgentInfo> },
    SessionList { sessions: Vec<SessionInfo> },
    SessionDeleted { session_id: String },
    Compacted { original_tokens: usize, summary_tokens: usize },
    Thinking,
    ShuttingDown,
}

#[derive(Debug, Serialize)]
struct EnvInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    conda_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conda_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    virtual_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
}

fn detect_env_info() -> Option<EnvInfo> {
    let conda_env = std::env::var("CONDA_DEFAULT_ENV").ok();
    let conda_prefix = std::env::var("CONDA_PREFIX")
        .ok()
        .or_else(|| std::env::var("MAMBA_ROOT_PREFIX").ok());
    let virtual_env = std::env::var("VIRTUAL_ENV").ok();
    let path = std::env::var("PATH").ok();

    if conda_env.is_some() || conda_prefix.is_some() || virtual_env.is_some() {
        Some(EnvInfo {
            conda_env,
            conda_prefix,
            virtual_env,
            path,
        })
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AgentInfo {
    agent_id: u64,
    session_name: String,
    task: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct SessionInfo {
    id: String,
    title: Option<String>,
    created_at: DateTime<Utc>,
    last_activity: DateTime<Utc>,
    message_count: usize,
    cwd: String,
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
        Some(Commands::Sessions { delete, limit }) => {
            if let Some(session_id) = delete {
                run_delete_session(&session_id).await
            } else {
                run_sessions(limit).await
            }
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
            println!("       gsh chat --session <id>  Resume a saved session");
            println!("       gsh sessions             List saved sessions");
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

/// Client output verbosity level.
/// Controlled via GSH_VERBOSITY env var: "debug", "progress" (default), "none".
/// Legacy GSH_STATS=0 is equivalent to "none", GSH_STATS=1 forces "progress".
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
enum Verbosity {
    /// No bracket messages at all — just LLM text output
    None = 0,
    /// Tool use/result notifications, compaction, generation stats
    Progress = 1,
    /// Everything in Progress plus tool inputs, output previews, raw messages
    Debug = 2,
}

fn get_verbosity() -> Verbosity {
    // GSH_VERBOSITY takes priority
    if let Ok(val) = std::env::var("GSH_VERBOSITY") {
        return match val.to_lowercase().as_str() {
            "none" | "quiet" | "0" => Verbosity::None,
            "debug" | "verbose" | "2" => Verbosity::Debug,
            _ => Verbosity::Progress,
        };
    }
    // Legacy GSH_STATS fallback
    if let Ok(val) = std::env::var("GSH_STATS") {
        return match val.as_str() {
            "0" => Verbosity::None,
            _ => Verbosity::Progress,
        };
    }
    // Default: progress when stderr is a terminal, none otherwise
    if io::stderr().is_terminal() {
        Verbosity::Progress
    } else {
        Verbosity::None
    }
}

/// Braille spinner that runs on a background tokio task, writing to stderr.
struct Spinner {
    stop: Arc<AtomicBool>,
    message: Arc<Mutex<String>>,
    handle: JoinHandle<()>,
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Spinner {
    fn start(msg: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let message = Arc::new(Mutex::new(msg.to_string()));
        let stop_clone = stop.clone();
        let message_clone = message.clone();

        let handle = tokio::spawn(async move {
            let mut i = 0usize;
            let mut stderr = io::stderr();
            while !stop_clone.load(Ordering::Relaxed) {
                let frame = SPINNER_FRAMES[i % SPINNER_FRAMES.len()];
                let msg = message_clone.lock().unwrap().clone();
                let _ = write!(stderr, "\r\x1b[2m{} {}\x1b[0m\x1b[K", frame, msg);
                let _ = stderr.flush();
                i += 1;
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            }
            // Clear the spinner line
            let _ = write!(stderr, "\r\x1b[K");
            let _ = stderr.flush();
        });

        Self { stop, message, handle }
    }

    fn set_message(&self, msg: &str) {
        *self.message.lock().unwrap() = msg.to_string();
    }

    async fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.await;
    }
}

/// Print generation timing stats (TTFT, tokens/sec) to stderr in dim text
fn print_generation_stats(
    start: Instant,
    first_token_time: Option<std::time::Duration>,
    total_chars: usize,
) {
    let total = start.elapsed();
    // Estimate tokens: ~4 chars per token (standard heuristic)
    let estimated_tokens = (total_chars + 2) / 4;

    if let Some(ttft) = first_token_time {
        let generation_time = total.saturating_sub(ttft);
        let tps = if generation_time.as_secs_f64() > 0.01 && estimated_tokens > 1 {
            // tokens/sec during generation (excludes TTFT)
            estimated_tokens as f64 / generation_time.as_secs_f64()
        } else {
            0.0
        };

        let ttft_str = if ttft.as_secs_f64() >= 1.0 {
            format!("{:.1}s", ttft.as_secs_f64())
        } else {
            format!("{:.0}ms", ttft.as_millis())
        };

        if tps > 0.0 {
            eprint!(
                "\x1b[2m[TTFT {ttft_str} | {tps:.0} tok/s | ~{estimated_tokens} tokens | {:.1}s]\x1b[0m\n",
                total.as_secs_f64()
            );
        } else {
            eprint!(
                "\x1b[2m[TTFT {ttft_str} | ~{estimated_tokens} tokens | {:.1}s]\x1b[0m\n",
                total.as_secs_f64()
            );
        }
    }
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
        env_info: detect_env_info(),
    };

    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    let mut stdout = io::stdout();
    let verbosity = get_verbosity();

    // Start spinner for context packing phase
    let mut spinner: Option<Spinner> = if verbosity >= Verbosity::Progress {
        Some(Spinner::start("Packing context..."))
    } else {
        None
    };

    // Timing
    let start = Instant::now();
    let mut first_token_time: Option<std::time::Duration> = None;
    let mut total_chars: usize = 0;
    let mut had_tool_use = false;

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            if let Some(s) = spinner.take() { s.stop().await; }
            break;
        }

        let trimmed = line.trim();
        if verbosity >= Verbosity::Debug {
            eprintln!("\x1b[2m[recv] {}\x1b[0m", &trimmed[..trimmed.len().min(200)]);
        }

        let response: DaemonMessage = serde_json::from_str(trimmed)?;

        match response {
            DaemonMessage::Thinking => {
                if let Some(ref s) = spinner {
                    s.set_message("Thinking...");
                }
            }
            DaemonMessage::TextChunk { text, done } => {
                if !text.is_empty() {
                    if let Some(s) = spinner.take() { s.stop().await; }
                    // Insert newline between text and tool output when tools ran silently
                    if had_tool_use {
                        println!();
                        had_tool_use = false;
                    }
                    if first_token_time.is_none() {
                        first_token_time = Some(start.elapsed());
                    }
                    total_chars += text.len();
                    print!("{}", text);
                    stdout.flush()?;
                }
                if done {
                    if let Some(s) = spinner.take() { s.stop().await; }
                    println!();
                    if verbosity >= Verbosity::Progress {
                        print_generation_stats(start, first_token_time, total_chars);
                    }
                    break;
                }
            }
            DaemonMessage::ToolUse { tool, input } => {
                if let Some(s) = spinner.take() { s.stop().await; }
                had_tool_use = true;
                if verbosity >= Verbosity::Progress {
                    eprintln!("\x1b[2m[Using tool: {}]\x1b[0m", tool);
                }
                if verbosity >= Verbosity::Debug {
                    let input_str = serde_json::to_string(&input).unwrap_or_default();
                    let preview = &input_str[..input_str.len().min(500)];
                    eprintln!("\x1b[2m[tool input: {}]\x1b[0m", preview);
                }
            }
            DaemonMessage::ToolResult { tool, output, success } => {
                if verbosity >= Verbosity::Progress {
                    let status = if success { "OK" } else { "FAILED" };
                    eprintln!("\x1b[2m[Tool {} {}]\x1b[0m", tool, status);
                }
                if !success && !output.is_empty() && verbosity >= Verbosity::Progress {
                    eprintln!("  Error: {}", output.lines().next().unwrap_or(&output));
                }
                if verbosity >= Verbosity::Debug && success && !output.is_empty() {
                    let preview: String = output.lines().take(3).collect::<Vec<_>>().join("\n");
                    eprintln!("\x1b[2m[tool output: {}{}]\x1b[0m", &preview[..preview.len().min(300)],
                        if output.len() > 300 { "..." } else { "" });
                }
            }
            DaemonMessage::Compacted { original_tokens, summary_tokens } => {
                if verbosity >= Verbosity::Progress {
                    eprintln!("\x1b[2m[context compacted: {} -> {} tokens]\x1b[0m", original_tokens, summary_tokens);
                }
            }
            DaemonMessage::Response { text, .. } => {
                if let Some(s) = spinner.take() { s.stop().await; }
                println!("{}", text);
                break;
            }
            DaemonMessage::Error { message, .. } => {
                if let Some(s) = spinner.take() { s.stop().await; }
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
        env_info: detect_env_info(),
    };
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;
    let session_id = match response {
        DaemonMessage::ChatStarted { session_id, resumed, message_count } => {
            if resumed {
                println!("Chat session resumed ({}, {} messages)", &session_id[..8], message_count);
            } else {
                println!("Chat session started ({})", &session_id[..8]);
            }
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
    let verbosity = get_verbosity();

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

        // Start spinner for context packing phase
        let mut spinner: Option<Spinner> = if verbosity >= Verbosity::Progress {
            Some(Spinner::start("Packing context..."))
        } else {
            None
        };

        // Timing for this exchange
        let msg_start = Instant::now();
        let mut first_token_time: Option<std::time::Duration> = None;
        let mut total_chars: usize = 0;
        let mut had_tool_use = false;

        // Receive response
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                if let Some(s) = spinner.take() { s.stop().await; }
                break;
            }

            let trimmed = line.trim();
            if verbosity >= Verbosity::Debug {
                eprintln!("\x1b[2m[recv] {}\x1b[0m", &trimmed[..trimmed.len().min(200)]);
            }

            let response: DaemonMessage = serde_json::from_str(trimmed)?;

            match response {
                DaemonMessage::Thinking => {
                    if let Some(ref s) = spinner {
                        s.set_message("Thinking...");
                    }
                }
                DaemonMessage::TextChunk { text, done } => {
                    if !text.is_empty() {
                        if let Some(s) = spinner.take() { s.stop().await; }
                        if had_tool_use {
                            println!();
                            had_tool_use = false;
                        }
                        if first_token_time.is_none() {
                            first_token_time = Some(msg_start.elapsed());
                        }
                        total_chars += text.len();
                        print!("{}", text);
                        stdout.flush()?;
                    }
                    if done {
                        if let Some(s) = spinner.take() { s.stop().await; }
                        println!();
                        if verbosity >= Verbosity::Progress {
                            print_generation_stats(msg_start, first_token_time, total_chars);
                        }
                        println!();
                        break;
                    }
                }
                DaemonMessage::ToolUse { tool, input } => {
                    if let Some(s) = spinner.take() { s.stop().await; }
                    had_tool_use = true;
                    if verbosity >= Verbosity::Progress {
                        eprintln!("\x1b[2m[Using tool: {}]\x1b[0m", tool);
                    }
                    if verbosity >= Verbosity::Debug {
                        let input_str = serde_json::to_string(&input).unwrap_or_default();
                        let preview = &input_str[..input_str.len().min(500)];
                        eprintln!("\x1b[2m[tool input: {}]\x1b[0m", preview);
                    }
                }
                DaemonMessage::ToolResult { tool, output, success } => {
                    if verbosity >= Verbosity::Progress {
                        let status = if success { "OK" } else { "FAILED" };
                        eprintln!("\x1b[2m[Tool {} {}]\x1b[0m", tool, status);
                    }
                    if !success && !output.is_empty() && verbosity >= Verbosity::Progress {
                        eprintln!("  Error: {}", output.lines().next().unwrap_or(&output));
                    }
                    if verbosity >= Verbosity::Debug && success && !output.is_empty() {
                        let preview: String = output.lines().take(3).collect::<Vec<_>>().join("\n");
                        eprintln!("\x1b[2m[tool output: {}{}]\x1b[0m", &preview[..preview.len().min(300)],
                            if output.len() > 300 { "..." } else { "" });
                    }
                }
                DaemonMessage::Compacted { original_tokens, summary_tokens } => {
                    if verbosity >= Verbosity::Progress {
                        eprintln!("\x1b[2m[context compacted: {} -> {} tokens]\x1b[0m", original_tokens, summary_tokens);
                    }
                }
                DaemonMessage::Error { message, .. } => {
                    if let Some(s) = spinner.take() { s.stop().await; }
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

async fn run_sessions(limit: usize) -> Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::ListSessions {
        limit: Some(limit),
    };
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;

    match response {
        DaemonMessage::SessionList { sessions } => {
            if sessions.is_empty() {
                println!("No saved sessions");
            } else {
                println!("Saved sessions ({}):", sessions.len());
                println!();
                for s in sessions {
                    let title = s.title.as_deref().unwrap_or("(untitled)");
                    let title_display: String = title.chars().take(50).collect();
                    let age = format_age(s.last_activity);
                    println!(
                        "  {} | {:>3} msgs | {} | {}",
                        &s.id[..8],
                        s.message_count,
                        age,
                        title_display,
                    );
                }
                println!();
                println!("Resume with: gsh chat --session <id>");
                println!("Delete with: gsh sessions --delete <id>");
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

async fn run_delete_session(session_id: &str) -> Result<()> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::DeleteSession {
        session_id: session_id.to_string(),
    };
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let response: DaemonMessage = serde_json::from_str(line.trim())?;

    match response {
        DaemonMessage::SessionDeleted { session_id } => {
            println!("Deleted session {}", &session_id[..8.min(session_id.len())]);
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

fn format_age(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);

    if duration.num_days() > 0 {
        format!("{}d ago", duration.num_days())
    } else if duration.num_hours() > 0 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_minutes() > 0 {
        format!("{}m ago", duration.num_minutes())
    } else {
        "just now".to_string()
    }
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
