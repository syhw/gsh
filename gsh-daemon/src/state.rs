use crate::config::Config;
use crate::context::ContextAccumulator;
use crate::session::Session;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Shared daemon state
pub struct DaemonState {
    /// Configuration
    pub config: Config,
    /// Context accumulator for shell events
    pub context: RwLock<ContextAccumulator>,
    /// Active chat sessions
    pub sessions: RwLock<HashMap<String, Session>>,
    /// Daemon start time
    pub started_at: std::time::Instant,
}

impl DaemonState {
    pub fn new(config: Config) -> Arc<Self> {
        let max_events = config.context.max_events;
        Arc::new(Self {
            config,
            context: RwLock::new(ContextAccumulator::new(max_events)),
            sessions: RwLock::new(HashMap::new()),
            started_at: std::time::Instant::now(),
        })
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}
