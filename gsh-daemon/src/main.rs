use gsh_daemon::{agent, config, flow, protocol, provider, session, state, tmux};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use protocol::{DaemonMessage, ShellMessage};
use state::DaemonState;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "gsh-daemon")]
#[command(about = "Daemon for gsh - an agentic shell")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Configuration file path
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Log level (trace, debug, info, warn, error)
    #[arg(short, long, global = true)]
    log_level: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the daemon
    Start {
        /// Run in foreground (don't daemonize)
        #[arg(short, long)]
        foreground: bool,
    },
    /// Stop a running daemon
    Stop,
    /// Check daemon status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration
    let config = if let Some(path) = &cli.config {
        config::Config::load_from(path)?
    } else {
        config::Config::load()?
    };

    // Setup logging (CLI flag takes precedence over config)
    let log_level_str = cli.log_level.as_deref().unwrap_or(&config.daemon.log_level);
    let log_level = log_level_str.parse().unwrap_or(tracing::Level::INFO);
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false);

    if let Some(log_file) = &config.daemon.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .with_context(|| format!("Failed to open log file: {}", log_file.display()))?;
        subscriber.with_writer(std::sync::Mutex::new(file)).init();
    } else {
        subscriber.init();
    }

    match cli.command {
        Commands::Start { foreground } => {
            if !foreground {
                info!("Note: Daemonization not implemented, running in foreground");
            }
            run_daemon(config).await
        }
        Commands::Stop => {
            send_shutdown(&config).await
        }
        Commands::Status => {
            check_status(&config).await
        }
    }
}

async fn run_daemon(config: config::Config) -> Result<()> {
    let socket_path = config.socket_path();

    // Remove existing socket if present
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)
            .with_context(|| format!("Failed to remove existing socket: {}", socket_path.display()))?;
    }

    // Create parent directory if needed
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind to socket: {}", socket_path.display()))?;

    info!("gsh-daemon v{} listening on {}", VERSION, socket_path.display());

    let state = DaemonState::new(config);

    // Handle shutdown signal
    let socket_path_clone = socket_path.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received shutdown signal");
        // Clean up socket
        let _ = std::fs::remove_file(&socket_path_clone);
        std::process::exit(0);
    });

    // Accept connections
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state).await {
                        error!("Connection error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("Accept error: {}", e);
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, state: Arc<DaemonState>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // Connection closed
        }

        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        debug!("Received: {}", line);

        let message: ShellMessage = match serde_json::from_str(line) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Failed to parse message: {}", e);
                let error = DaemonMessage::Error {
                    message: format!("Invalid message format: {}", e),
                    code: Some("PARSE_ERROR".to_string()),
                };
                let response = serde_json::to_string(&error)? + "\n";
                writer.write_all(response.as_bytes()).await?;
                continue;
            }
        };

        let response = process_message(message, &state, &mut writer).await?;
        if let Some(resp) = response {
            let response_str = serde_json::to_string(&resp)? + "\n";
            writer.write_all(response_str.as_bytes()).await?;
        }
    }

    Ok(())
}

async fn process_message(
    message: ShellMessage,
    state: &Arc<DaemonState>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<Option<DaemonMessage>> {
    match message {
        ShellMessage::Preexec { command, cwd, timestamp } => {
            let mut ctx = state.context.write().await;
            ctx.preexec(command, cwd, timestamp);
            Ok(Some(DaemonMessage::Ack))
        }

        ShellMessage::Postcmd { exit_code, cwd, duration_ms, timestamp } => {
            let mut ctx = state.context.write().await;
            ctx.postcmd(exit_code, cwd, duration_ms, timestamp);
            Ok(Some(DaemonMessage::Ack))
        }

        ShellMessage::Chpwd { old_cwd, new_cwd, timestamp } => {
            let mut ctx = state.context.write().await;
            ctx.chpwd(old_cwd, new_cwd, timestamp);
            Ok(Some(DaemonMessage::Ack))
        }

        ShellMessage::Prompt { query, cwd, session_id, stream, provider: provider_override, model: model_override, flow: flow_name } => {
            // Check if this is a flow execution
            if let Some(flow_name) = flow_name {
                return run_flow(
                    &flow_name, &query, &cwd, stream, session_id,
                    provider_override, model_override,
                    state, writer
                ).await;
            }

            // Get shell context
            let context = {
                let ctx = state.context.read().await;
                ctx.generate_context(state.config.context.max_context_chars)
            };

            // Determine provider name
            let provider_name = provider_override
                .as_deref()
                .unwrap_or(&state.config.llm.default_provider);

            // Create provider (with optional model override)
            let provider = if let Some(model) = model_override {
                provider::create_provider_with_model(provider_name, &model, &state.config)?
            } else {
                provider::create_provider(provider_name, &state.config)?
            };

            // Create agent
            let agent = agent::Agent::new(provider, &state.config, cwd.clone());

            // Create event channel
            let (event_tx, mut event_rx) = mpsc::channel::<agent::AgentEvent>(100);

            // Spawn agent task
            let agent_handle = tokio::spawn(async move {
                agent.run_oneshot(&query, Some(&context), event_tx).await
            });

            // Stream events to client
            let mut final_text = String::new();
            let mut had_error = false;
            while let Some(event) = event_rx.recv().await {
                let msg = match event {
                    agent::AgentEvent::TextChunk(text) => {
                        if stream {
                            Some(DaemonMessage::TextChunk { text, done: false })
                        } else {
                            final_text.push_str(&text);
                            None
                        }
                    }
                    agent::AgentEvent::ToolStart { name, input } => {
                        Some(DaemonMessage::ToolUse { tool: name, input })
                    }
                    agent::AgentEvent::ToolResult { name, output, success } => {
                        Some(DaemonMessage::ToolResult { tool: name, output, success })
                    }
                    agent::AgentEvent::Done { final_text: text } => {
                        final_text = text;
                        if stream {
                            Some(DaemonMessage::TextChunk { text: String::new(), done: true })
                        } else {
                            None
                        }
                    }
                    agent::AgentEvent::Error(e) => {
                        had_error = true;
                        Some(DaemonMessage::Error { message: e, code: None })
                    }
                };

                if let Some(msg) = msg {
                    let response_str = serde_json::to_string(&msg)? + "\n";
                    writer.write_all(response_str.as_bytes()).await?;
                }
            }

            // Wait for agent to finish
            let _ = agent_handle.await?;

            // If not streaming and no error, send final response
            if !stream && !had_error {
                let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                return Ok(Some(DaemonMessage::Response {
                    text: final_text,
                    session_id,
                }));
            }

            Ok(None)
        }

        ShellMessage::ListAgents => {
            let tmux_manager = tmux::TmuxManager::new();
            let agents = tmux_manager.list_subagents().unwrap_or_default();
            let agent_infos: Vec<protocol::AgentInfo> = agents.into_iter().map(|a| {
                protocol::AgentInfo {
                    agent_id: a.agent_id,
                    session_name: a.session_name,
                    task: a.task,
                    cwd: a.cwd,
                }
            }).collect();
            Ok(Some(DaemonMessage::AgentList { agents: agent_infos }))
        }

        ShellMessage::KillAgent { agent_id } => {
            let tmux_manager = tmux::TmuxManager::new();
            tmux_manager.kill_agent(agent_id)?;
            Ok(Some(DaemonMessage::AgentKilled { agent_id }))
        }

        ShellMessage::ChatStart { cwd, session_id } => {
            let session = if let Some(id) = session_id {
                session::Session::with_id(id, cwd)
            } else {
                session::Session::new(cwd)
            };
            let session_id = session.id.clone();

            let mut sessions = state.sessions.write().await;
            sessions.insert(session_id.clone(), session);

            Ok(Some(DaemonMessage::ChatStarted { session_id }))
        }

        ShellMessage::ChatMessage { session_id, message } => {
            let mut sessions = state.sessions.write().await;
            let session = sessions.get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            session.add_user_message(message.clone());
            let cwd = session.cwd.clone();

            // Build messages from session history
            let messages: Vec<provider::ChatMessage> = session.messages.iter().map(|m| {
                provider::ChatMessage {
                    role: match m.role {
                        session::MessageRole::User => provider::ChatRole::User,
                        session::MessageRole::Assistant => provider::ChatRole::Assistant,
                        session::MessageRole::System => provider::ChatRole::User, // System messages go as user
                    },
                    content: provider::MessageContent::Text(m.content.clone()),
                }
            }).collect();

            drop(sessions); // Release lock before async work

            // Get shell context (TODO: incorporate into system prompt for chat sessions)
            let _context = {
                let ctx = state.context.read().await;
                ctx.generate_context(state.config.context.max_context_chars)
            };

            // Create provider and agent
            let provider = provider::create_provider(
                &state.config.llm.default_provider,
                &state.config,
            )?;
            let agent = agent::Agent::new(provider, &state.config, cwd);

            let (event_tx, mut event_rx) = mpsc::channel::<agent::AgentEvent>(100);

            // Run agent with history
            let agent_handle = tokio::spawn(async move {
                agent.run_with_history(messages, event_tx).await
            });

            // Stream events
            let mut final_text = String::new();
            while let Some(event) = event_rx.recv().await {
                let msg = match event {
                    agent::AgentEvent::TextChunk(text) => {
                        Some(DaemonMessage::TextChunk { text, done: false })
                    }
                    agent::AgentEvent::ToolStart { name, input } => {
                        Some(DaemonMessage::ToolUse { tool: name, input })
                    }
                    agent::AgentEvent::ToolResult { name, output, success } => {
                        Some(DaemonMessage::ToolResult { tool: name, output, success })
                    }
                    agent::AgentEvent::Done { final_text: text } => {
                        final_text = text;
                        Some(DaemonMessage::TextChunk { text: String::new(), done: true })
                    }
                    agent::AgentEvent::Error(e) => {
                        Some(DaemonMessage::Error { message: e, code: None })
                    }
                };

                if let Some(msg) = msg {
                    let response_str = serde_json::to_string(&msg)? + "\n";
                    writer.write_all(response_str.as_bytes()).await?;
                }
            }

            let _ = agent_handle.await?;

            // Update session with assistant response
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                session.add_assistant_message(final_text);
            }

            Ok(None)
        }

        ShellMessage::ChatEnd { session_id } => {
            let mut sessions = state.sessions.write().await;
            sessions.remove(&session_id);
            Ok(Some(DaemonMessage::Ack))
        }

        ShellMessage::Ping => {
            Ok(Some(DaemonMessage::Pong {
                uptime_secs: state.uptime_secs(),
                version: VERSION.to_string(),
            }))
        }

        ShellMessage::Shutdown => {
            info!("Shutdown requested");
            // Send response before exiting
            let response = DaemonMessage::ShuttingDown;
            let response_str = serde_json::to_string(&response)? + "\n";
            writer.write_all(response_str.as_bytes()).await?;

            // Clean up socket
            let _ = std::fs::remove_file(state.config.socket_path());

            std::process::exit(0);
        }
    }
}

async fn send_shutdown(config: &config::Config) -> Result<()> {
    let socket_path = config.socket_path();

    if !socket_path.exists() {
        println!("Daemon not running (socket not found)");
        return Ok(());
    }

    let stream = UnixStream::connect(&socket_path)
        .await
        .context("Failed to connect to daemon")?;

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Shutdown;
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut response = String::new();
    reader.read_line(&mut response).await?;

    println!("Daemon stopped");
    Ok(())
}

async fn check_status(config: &config::Config) -> Result<()> {
    let socket_path = config.socket_path();

    if !socket_path.exists() {
        println!("Daemon not running");
        return Ok(());
    }

    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(_) => {
            println!("Daemon not running (connection failed)");
            return Ok(());
        }
    };

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let msg = ShellMessage::Ping;
    let msg_str = serde_json::to_string(&msg)? + "\n";
    writer.write_all(msg_str.as_bytes()).await?;

    let mut response = String::new();
    reader.read_line(&mut response).await?;

    let response: DaemonMessage = serde_json::from_str(&response)?;

    if let DaemonMessage::Pong { uptime_secs, version } = response {
        println!("Daemon running");
        println!("  Version: {}", version);
        println!("  Uptime: {}s", uptime_secs);
        println!("  Socket: {}", socket_path.display());
    }

    Ok(())
}

/// Load and execute a flow
async fn run_flow(
    flow_name: &str,
    input: &str,
    cwd: &str,
    stream: bool,
    session_id: Option<String>,
    _provider_override: Option<String>,
    _model_override: Option<String>,
    state: &Arc<DaemonState>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<Option<DaemonMessage>> {
    use flow::engine::{FlowEngine, FlowEvent};
    

    // Find flow file
    let flow_path = find_flow_file(flow_name)?;

    // Parse flow
    let flow = flow::parser::parse_flow_file(&flow_path)
        .map_err(|e| anyhow::anyhow!("Failed to parse flow '{}': {}", flow_name, e))?;

    // Validate flow
    flow.validate()
        .map_err(|e| anyhow::anyhow!("Invalid flow '{}': {}", flow_name, e))?;

    info!("Running flow: {} ({} nodes)", flow.name, flow.nodes.len());

    // Create flow engine
    let engine = FlowEngine::new(state.config.clone());

    // Create event channel
    let (event_tx, mut event_rx) = mpsc::channel::<FlowEvent>(100);

    // Spawn flow execution
    let flow_clone = flow.clone();
    let input_clone = input.to_string();
    let cwd_clone = cwd.to_string();

    let flow_handle = tokio::spawn(async move {
        engine.run(&flow_clone, &input_clone, &cwd_clone, event_tx).await
    });

    // Stream events to client
    let mut final_output = String::new();
    let mut had_error = false;

    while let Some(event) = event_rx.recv().await {
        let msg = match event {
            FlowEvent::FlowStarted { flow_name, entry_node } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!("[Flow '{}' started, entry: {}]\n", flow_name, entry_node),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::NodeStarted { node_id, node_name, tmux_session, .. } => {
                if stream {
                    let session_info = tmux_session
                        .map(|s| format!(" (tmux: {})", s))
                        .unwrap_or_default();
                    Some(DaemonMessage::TextChunk {
                        text: format!("[Node '{}' ({}) started{}]\n", node_id, node_name, session_info),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::NodeText { text, .. } => {
                if stream {
                    Some(DaemonMessage::TextChunk { text, done: false })
                } else {
                    final_output.push_str(&text);
                    None
                }
            }
            FlowEvent::NodeToolUse { node_id, tool, input } => {
                Some(DaemonMessage::ToolUse {
                    tool: format!("{}:{}", node_id, tool),
                    input,
                })
            }
            FlowEvent::NodeToolResult { node_id, tool, output, success } => {
                Some(DaemonMessage::ToolResult {
                    tool: format!("{}:{}", node_id, tool),
                    output,
                    success,
                })
            }
            FlowEvent::NodeCompleted { node_id, output, next_node } => {
                final_output = output.clone();
                if stream {
                    let next_info = next_node
                        .map(|n| format!(" -> {}", n))
                        .unwrap_or_else(|| " (end)".to_string());
                    Some(DaemonMessage::TextChunk {
                        text: format!("\n[Node '{}' completed{}]\n", node_id, next_info),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::ParallelStarted { node_ids } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!("[Parallel execution: {}]\n", node_ids.join(", ")),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::ParallelCompleted { join_node, .. } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!("[Parallel complete, joining at: {}]\n", join_node),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::FlowCompleted { final_output: output } => {
                final_output = output;
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: "\n[Flow completed]\n".to_string(),
                        done: true,
                    })
                } else {
                    None
                }
            }
            FlowEvent::FlowError { node_id, error } => {
                had_error = true;
                let node_info = node_id
                    .map(|n| format!(" in node '{}'", n))
                    .unwrap_or_default();
                Some(DaemonMessage::Error {
                    message: format!("Flow error{}: {}", node_info, error),
                    code: None,
                })
            }
        };

        if let Some(msg) = msg {
            let response_str = serde_json::to_string(&msg)? + "\n";
            writer.write_all(response_str.as_bytes()).await?;
        }
    }

    // Wait for flow to complete
    let _ = flow_handle.await?;

    // Send final response if not streaming and no error
    if !stream && !had_error {
        let session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        return Ok(Some(DaemonMessage::Response {
            text: final_output,
            session_id,
        }));
    }

    Ok(None)
}

/// Find a flow file by name
/// Searches in:
/// 1. .gsh/flows/{name}.toml (project-local)
/// 2. ~/.config/gsh/flows/{name}.toml (user config)
fn find_flow_file(name: &str) -> Result<std::path::PathBuf> {
    // Add .toml extension if not present
    let filename = if name.ends_with(".toml") {
        name.to_string()
    } else {
        format!("{}.toml", name)
    };

    // Check project-local first
    let local_path = std::path::Path::new(".gsh/flows").join(&filename);
    if local_path.exists() {
        return Ok(local_path);
    }

    // Check user config
    if let Some(home) = dirs::home_dir() {
        let user_path = home.join(".config/gsh/flows").join(&filename);
        if user_path.exists() {
            return Ok(user_path);
        }
    }

    // Check XDG config
    if let Some(config_dir) = dirs::config_dir() {
        let config_path = config_dir.join("gsh/flows").join(&filename);
        if config_path.exists() {
            return Ok(config_path);
        }
    }

    Err(anyhow::anyhow!(
        "Flow '{}' not found. Looked in:\n  - .gsh/flows/{}\n  - ~/.config/gsh/flows/{}",
        name, filename, filename
    ))
}
