use gsh_daemon::{agent, config, flow, observability, protocol, provider, session, state, tmux};

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
    /// Open the observability dashboard
    Dashboard {
        /// Custom log directory
        #[arg(short = 'd', long)]
        log_dir: Option<PathBuf>,
    },
}

/// Get the PID file path
fn pid_file_path(config: &config::Config) -> PathBuf {
    let socket_path = config.socket_path();
    socket_path.with_extension("pid")
}

/// Get the default log file path for daemon mode
fn default_daemon_log() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("gsh")
        .join("daemon.log")
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load configuration
    let config = if let Some(path) = &cli.config {
        config::Config::load_from(path)?
    } else {
        config::Config::load()?
    };

    match cli.command {
        Commands::Start { foreground } => {
            if foreground {
                // Foreground: setup logging to stderr, then run
                setup_logging(&config, cli.log_level.as_deref(), false)?;
                tokio::runtime::Runtime::new()?
                    .block_on(run_daemon(config))
            } else {
                // Daemonize: fork into background, then run
                let log_file = config.daemon.log_file.clone()
                    .unwrap_or_else(default_daemon_log);

                // Ensure log directory exists
                if let Some(parent) = log_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }

                let pid_file = pid_file_path(&config);
                let stdout_file = std::fs::OpenOptions::new()
                    .create(true).append(true).open(&log_file)
                    .with_context(|| format!("Failed to open log file: {}", log_file.display()))?;
                let stderr_file = std::fs::OpenOptions::new()
                    .create(true).append(true).open(&log_file)
                    .with_context(|| format!("Failed to open log file: {}", log_file.display()))?;

                let daemonize = daemonize::Daemonize::new()
                    .pid_file(&pid_file)
                    .working_directory(".")
                    .stdout(stdout_file)
                    .stderr(stderr_file);

                match daemonize.start() {
                    Ok(_) => {
                        // We're in the child process now
                        setup_logging(&config, cli.log_level.as_deref(), true)?;
                        tokio::runtime::Runtime::new()?
                            .block_on(run_daemon(config))
                    }
                    Err(e) => {
                        eprintln!("Failed to daemonize: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::Stop => {
            tokio::runtime::Runtime::new()?
                .block_on(send_shutdown(&config))
        }
        Commands::Status => {
            tokio::runtime::Runtime::new()?
                .block_on(check_status(&config))
        }
        Commands::Dashboard { log_dir } => {
            if let Some(dir) = log_dir {
                observability::run_dashboard_with_dir(dir)
            } else {
                observability::run_dashboard()
            }
        }
    }
}

fn setup_logging(config: &config::Config, log_level_override: Option<&str>, daemon_mode: bool) -> Result<()> {
    let log_level_str = log_level_override.unwrap_or(&config.daemon.log_level);
    let log_level = log_level_str.parse().unwrap_or(tracing::Level::INFO);
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_target(false);

    if daemon_mode {
        // In daemon mode, log to the daemon log file (stdout/stderr already redirected)
        subscriber.init();
    } else if let Some(log_file) = &config.daemon.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_file)
            .with_context(|| format!("Failed to open log file: {}", log_file.display()))?;
        subscriber.with_writer(std::sync::Mutex::new(file)).init();
    } else {
        subscriber.init();
    }

    Ok(())
}

async fn run_daemon(config: config::Config) -> Result<()> {
    let socket_path = config.socket_path();
    let pid_path = pid_file_path(&config);

    // Remove existing socket if present
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)
            .with_context(|| format!("Failed to remove existing socket: {}", socket_path.display()))?;
    }

    // Create parent directory if needed
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write PID file (for foreground mode; daemon mode writes it via daemonize)
    if !pid_path.exists() {
        let _ = std::fs::write(&pid_path, format!("{}", std::process::id()));
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind to socket: {}", socket_path.display()))?;

    info!("gsh-daemon v{} (pid {}) listening on {}", VERSION, std::process::id(), socket_path.display());

    let state = DaemonState::new(config);

    // Handle shutdown signal
    let socket_path_clone = socket_path.clone();
    let pid_path_clone = pid_path.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("Received shutdown signal");
        // Clean up tmux sessions spawned by flows
        info!("Cleaning up tmux sessions...");
        let killed_sessions = tmux::TmuxManager::cleanup_all_sessions();
        if !killed_sessions.is_empty() {
            info!("Killed {} tmux session(s): {}", killed_sessions.len(), killed_sessions.join(", "));
        }
        // Clean up socket and PID file
        let _ = std::fs::remove_file(&socket_path_clone);
        let _ = std::fs::remove_file(&pid_path_clone);
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

        ShellMessage::Prompt { query, cwd, session_id, stream, provider: provider_override, model: model_override, flow: flow_name, env_info } => {
            // Check if this is a flow execution
            if let Some(flow_name) = flow_name {
                return run_flow(
                    &flow_name, &query, &cwd, stream, session_id,
                    provider_override, model_override,
                    state, writer
                ).await;
            }

            // Get ambient shell context (last N commands for JIT loading)
            let context = {
                let ctx = state.context.read().await;
                ctx.generate_ambient_summary(state.config.context.ambient_commands)
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

            // Create or load session for persistence
            let session_dir = state.session_dir.clone();
            let mut prompt_session = if let Some(ref id) = session_id {
                session::Session::load(id, session_dir.clone()).unwrap_or_else(|_| {
                    session::Session::create(cwd.clone(), session_dir).unwrap()
                })
            } else {
                session::Session::create(cwd.clone(), session_dir)?
            };
            let session_id_for_agent = prompt_session.id().to_string();

            // Persist user message
            prompt_session.add_message(provider::ChatMessage {
                role: provider::ChatRole::User,
                content: provider::MessageContent::Text(query.clone()),
            })?;

            let messages = prompt_session.messages.clone();

            // Create agent with observer
            let agent = agent::Agent::new_with_env(
                provider, &state.config, cwd.clone(), env_info,
                Some(state.context_retriever.clone()),
            )
                .with_observer(state.observer.clone())
                .with_session_id(&session_id_for_agent);

            // Create event channel
            let (event_tx, mut event_rx) = mpsc::channel::<agent::AgentEvent>(100);

            // Spawn agent task with history for resumed sessions
            let has_history = messages.len() > 1;
            let agent_handle = if has_history {
                tokio::spawn(async move {
                    agent.run_with_history(messages, event_tx).await
                })
            } else {
                tokio::spawn(async move {
                    agent.run_oneshot(&query, Some(&context), event_tx).await
                })
            };

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
                    agent::AgentEvent::Compacted { summary_tokens, original_tokens } => {
                        Some(DaemonMessage::Compacted { original_tokens, summary_tokens })
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

            // Persist assistant response
            if !final_text.is_empty() {
                prompt_session.add_message(provider::ChatMessage {
                    role: provider::ChatRole::Assistant,
                    content: provider::MessageContent::Text(final_text.clone()),
                })?;
            }
            prompt_session.close();

            // If not streaming and no error, send final response
            if !stream && !had_error {
                return Ok(Some(DaemonMessage::Response {
                    text: final_text,
                    session_id: session_id_for_agent,
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

        ShellMessage::ChatStart { cwd, session_id, env_info } => {
            let session_dir = state.session_dir.clone();
            let session = if let Some(ref id) = session_id {
                // Try to resume existing session from disk
                match session::Session::load(id, session_dir.clone()) {
                    Ok(s) => {
                        info!("Resumed session {} ({} messages)", id, s.messages.len());
                        s
                    }
                    Err(e) => {
                        warn!("Failed to resume session {}: {}, creating new", id, e);
                        session::Session::create(cwd, session_dir)?
                    }
                }
            } else {
                session::Session::create(cwd, session_dir)?
            };
            let session_id = session.id().to_string();
            let message_count = session.messages.len();

            let mut sessions = state.sessions.write().await;
            sessions.insert(session_id.clone(), session);

            // Store client env info for this session
            if let Some(env) = env_info {
                let mut envs = state.session_env.write().await;
                envs.insert(session_id.clone(), env);
            }

            // Include message count so CLI can show "(resumed, N messages)"
            if message_count > 0 {
                info!("Chat session resumed: {} ({} messages)", &session_id[..8], message_count);
            }

            let resumed = message_count > 0;
            Ok(Some(DaemonMessage::ChatStarted { session_id, resumed, message_count }))
        }

        ShellMessage::ChatMessage { session_id, message } => {
            let mut sessions = state.sessions.write().await;
            let session = sessions.get_mut(&session_id)
                .ok_or_else(|| anyhow::anyhow!("Session not found: {}", session_id))?;

            // Create and persist user message using provider types
            let user_msg = provider::ChatMessage {
                role: provider::ChatRole::User,
                content: provider::MessageContent::Text(message.clone()),
            };
            session.add_message(user_msg)?;
            let cwd = session.metadata.cwd.clone();

            // Messages are already Vec<ChatMessage> — no conversion needed
            let messages = session.messages.clone();

            drop(sessions); // Release lock before async work

            // Create provider and agent with observer
            let provider = provider::create_provider(
                &state.config.llm.default_provider,
                &state.config,
            )?;
            let env_info = {
                let envs = state.session_env.read().await;
                envs.get(&session_id).cloned()
            };
            let agent = agent::Agent::new_with_env(
                provider, &state.config, cwd, env_info,
                Some(state.context_retriever.clone()),
            )
                .with_observer(state.observer.clone())
                .with_session_id(&session_id);

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
                    agent::AgentEvent::Compacted { summary_tokens, original_tokens } => {
                        Some(DaemonMessage::Compacted { original_tokens, summary_tokens })
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

            // Persist assistant response
            let mut sessions = state.sessions.write().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                let assistant_msg = provider::ChatMessage {
                    role: provider::ChatRole::Assistant,
                    content: provider::MessageContent::Text(final_text),
                };
                if let Err(e) = session.add_message(assistant_msg) {
                    warn!("Failed to persist assistant message: {}", e);
                }
            }

            Ok(None)
        }

        ShellMessage::ChatEnd { session_id } => {
            let mut sessions = state.sessions.write().await;
            if let Some(mut session) = sessions.remove(&session_id) {
                session.close(); // Flush to disk, keep file for future resume
            }
            // Clean up env info
            let mut envs = state.session_env.write().await;
            envs.remove(&session_id);
            Ok(Some(DaemonMessage::Ack))
        }

        ShellMessage::ListSessions { limit } => {
            let metas = session::list_sessions(&state.session_dir)
                .unwrap_or_default();
            let limit = limit.unwrap_or(20);
            let sessions: Vec<protocol::SessionInfo> = metas.into_iter()
                .take(limit)
                .map(|m| protocol::SessionInfo {
                    id: m.id,
                    title: m.title,
                    created_at: m.created_at,
                    last_activity: m.last_activity,
                    message_count: m.message_count,
                    cwd: m.cwd,
                })
                .collect();
            Ok(Some(DaemonMessage::SessionList { sessions }))
        }

        ShellMessage::DeleteSession { session_id } => {
            // Remove from active sessions if loaded
            let mut sessions = state.sessions.write().await;
            if let Some(mut session) = sessions.remove(&session_id) {
                session.close();
            }
            drop(sessions);

            // Delete from disk
            session::delete_session(&session_id, &state.session_dir)?;
            Ok(Some(DaemonMessage::SessionDeleted { session_id }))
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

            // Clean up tmux sessions
            let killed_sessions = tmux::TmuxManager::cleanup_all_sessions();
            if !killed_sessions.is_empty() {
                info!("Killed {} tmux session(s)", killed_sessions.len());
            }

            // Clean up socket and PID file
            let _ = std::fs::remove_file(state.config.socket_path());
            let _ = std::fs::remove_file(pid_file_path(&state.config));

            std::process::exit(0);
        }
    }
}

async fn send_shutdown(config: &config::Config) -> Result<()> {
    let socket_path = config.socket_path();
    let pid_path = pid_file_path(config);

    if !socket_path.exists() {
        // Try PID file as fallback
        if pid_path.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                    // Send SIGTERM
                    unsafe { libc::kill(pid, libc::SIGTERM); }
                    let _ = std::fs::remove_file(&pid_path);
                    println!("Daemon stopped (via SIGTERM, pid {})", pid);
                    return Ok(());
                }
            }
            let _ = std::fs::remove_file(&pid_path);
        }
        println!("Daemon not running (socket not found)");
        return Ok(());
    }

    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(_) => {
            // Stale socket — clean up
            let _ = std::fs::remove_file(&socket_path);
            let _ = std::fs::remove_file(&pid_path);
            println!("Daemon not running (stale socket removed)");
            return Ok(());
        }
    };

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
    let pid_path = pid_file_path(config);

    if !socket_path.exists() {
        println!("Daemon not running");
        return Ok(());
    }

    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(_) => {
            println!("Daemon not running (stale socket)");
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
        // Show PID if available
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                println!("  PID: {}", pid);
            }
        }
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
    let mut engine = FlowEngine::new(state.config.clone());

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
            // Publication mode events
            FlowEvent::PublicationCreated { node_id, publication_id, title, author } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!(
                            "[Publication {} by {} (node {}): {}]\n",
                            publication_id, author, node_id, title
                        ),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::PublicationReviewed { publication_id, reviewer, grade, consensus_score } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!(
                            "[Review: {} gave {} to {} (score: {})]\n",
                            reviewer, grade, publication_id, consensus_score
                        ),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::PublicationCited { publication_id, cited_id, node_id } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!(
                            "[Citation: {} cited {} (node {})]\n",
                            publication_id, cited_id, node_id
                        ),
                        done: false,
                    })
                } else {
                    None
                }
            }
            FlowEvent::PublicationConsensus { publication_id, title, accept_count } => {
                if stream {
                    Some(DaemonMessage::TextChunk {
                        text: format!(
                            "[CONSENSUS REACHED: {} - '{}' ({} accepts)]\n",
                            publication_id, title, accept_count
                        ),
                        done: false,
                    })
                } else {
                    None
                }
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
