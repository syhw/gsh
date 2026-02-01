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
    ShuttingDown,
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
        return run_prompt(&query, true).await;
    }

    match cli.command {
        Some(Commands::Prompt { query, no_stream }) => {
            let query = get_query_from_args_or_stdin(&query)?;
            run_prompt(&query, !no_stream).await
        }
        Some(Commands::Chat { session }) => {
            run_chat(session).await
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
                    return run_prompt(&query, true).await;
                }
            }

            // Show help
            println!("gsh v{}", VERSION);
            println!();
            println!("Usage: gsh <query>              Send a prompt (no quotes needed)");
            println!("       echo 'prompt' | gsh      Read prompt from stdin");
            println!("       gsh - < file.txt         Read prompt from file");
            println!("       gsh chat                 Start interactive chat");
            println!("       gsh status               Check daemon status");
            println!("       gsh stop                 Stop the daemon");
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

async fn run_prompt(query: &str, stream: bool) -> Result<()> {
    let stream_conn = connect().await?;
    let (reader, mut writer) = stream_conn.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Prompt {
        query: query.to_string(),
        cwd: get_cwd(),
        session_id: None,
        stream,
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
