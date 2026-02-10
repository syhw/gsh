use crate::config::Config;
use crate::context::{ContextAccumulator, ContextRetriever};
use crate::observability::Observer;
use crate::protocol::EnvInfo;
use crate::session::Session;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Shared daemon state
pub struct DaemonState {
    /// Configuration
    pub config: Config,
    /// Context accumulator for shell events
    pub context: RwLock<ContextAccumulator>,
    /// Active chat sessions
    pub sessions: RwLock<HashMap<String, Session>>,
    /// Client environment info per session (for Python env, PATH, etc.)
    pub session_env: RwLock<HashMap<String, EnvInfo>>,
    /// Daemon start time
    pub started_at: std::time::Instant,
    /// Observer for logging and metrics
    pub observer: Arc<Observer>,
    /// Session storage directory
    pub session_dir: PathBuf,
    /// Context retriever for the search_context tool
    pub context_retriever: Arc<ContextRetriever>,
}

impl DaemonState {
    pub fn new(config: Config) -> Arc<Self> {
        let max_events = config.context.max_events;
        let session_dir = config.session_dir();

        // Ensure session directory exists
        if let Err(e) = std::fs::create_dir_all(&session_dir) {
            warn!("Failed to create session directory {:?}: {}", session_dir, e);
        }

        // Run session cleanup on startup
        match crate::session::cleanup_sessions(
            &session_dir,
            config.sessions.max_sessions,
            config.sessions.max_age_days,
        ) {
            Ok(deleted) if deleted > 0 => {
                info!("Session cleanup: removed {} old session(s)", deleted);
            }
            Err(e) => {
                warn!("Session cleanup failed: {}", e);
            }
            _ => {}
        }

        // Create observer for logging
        let observer = match Observer::new() {
            Ok(obs) => Arc::new(obs),
            Err(e) => {
                warn!("Failed to create observer, using default: {}", e);
                Arc::new(Observer::default())
            }
        };

        // Data directory for persistent stores
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gsh");

        // Shell history file path
        let history_path = data_dir.join("shell-history.jsonl");

        // Create context accumulator with persistent history
        let context = ContextAccumulator::with_history(
            max_events,
            history_path.clone(),
            config.context.max_history_lines,
        );

        // Create context retriever
        let context_retriever = Arc::new(ContextRetriever::new(
            history_path,
            data_dir.join("logs"),
            session_dir.clone(),
        ));

        Arc::new(Self {
            config,
            context: RwLock::new(context),
            sessions: RwLock::new(HashMap::new()),
            session_env: RwLock::new(HashMap::new()),
            started_at: std::time::Instant::now(),
            observer,
            session_dir,
            context_retriever,
        })
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}
