//! Observability module for gsh
//!
//! Provides structured logging, cost tracking, and metrics for the agentic system.
//!
//! # Components
//!
//! - **events**: Rich event types for all observable actions
//! - **logger**: JSONL file logger for persistent event storage
//! - **cost**: Token usage tracking and cost estimation by provider/model
//!
//! # Usage
//!
//! ```rust,ignore
//! use gsh_daemon::observability::{Observer, ObservabilityEvent, EventKind};
//!
//! // Create an observer
//! let observer = Observer::new()?;
//!
//! // Log events
//! observer.log_event("session-123", "root", EventKind::Prompt {
//!     content: "Hello".to_string(),
//! });
//!
//! // Track usage
//! observer.record_usage("session-123", "anthropic", "claude-sonnet-4", &usage_stats);
//!
//! // Get cost summary
//! let summary = observer.session_summary("session-123");
//! ```

pub mod cost;
pub mod dashboard;
pub mod events;
pub mod logger;

pub use cost::{AccumulatedUsage, CostTracker, ModelPricing, get_model_pricing};
pub use dashboard::{run_dashboard, run_dashboard_with_dir, Dashboard};
pub use events::{AgentRunSummary, EventKind, ObservabilityEvent};
pub use logger::{EventLogger, LoggerConfig, list_log_files, latest_log_file, read_events};

use crate::provider::UsageStats;
use anyhow::Result;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tracing::error;

/// Central observer that coordinates logging and cost tracking
pub struct Observer {
    /// Event logger (writes to JSONL files)
    logger: Arc<EventLogger>,
    /// Cost tracker (accumulates token usage)
    cost_tracker: Arc<RwLock<CostTracker>>,
    /// Async event sender (for non-blocking logging)
    event_tx: Option<mpsc::Sender<ObservabilityEvent>>,
}

impl Observer {
    /// Create a new observer with default configuration
    pub fn new() -> Result<Self> {
        let logger = EventLogger::with_defaults()?;
        Ok(Self {
            logger: Arc::new(logger),
            cost_tracker: Arc::new(RwLock::new(CostTracker::new())),
            event_tx: None,
        })
    }

    /// Create an observer with custom logger configuration
    pub fn with_config(config: LoggerConfig) -> Result<Self> {
        let logger = EventLogger::new(config)?;
        Ok(Self {
            logger: Arc::new(logger),
            cost_tracker: Arc::new(RwLock::new(CostTracker::new())),
            event_tx: None,
        })
    }

    /// Create an observer with async event channel
    ///
    /// Returns the observer and a handle that should be awaited on shutdown.
    pub fn with_async_logging(config: LoggerConfig) -> Result<(Self, tokio::task::JoinHandle<()>)> {
        let logger = EventLogger::new(config)?;
        let (tx, handle) = logger.spawn_async();

        let observer = Self {
            logger: Arc::new(EventLogger::with_defaults()?), // Placeholder, not used in async mode
            cost_tracker: Arc::new(RwLock::new(CostTracker::new())),
            event_tx: Some(tx),
        };

        Ok((observer, handle))
    }

    /// Log an event
    pub fn log_event(&self, session: &str, agent: &str, event: EventKind) {
        let obs_event = ObservabilityEvent::new(session, agent, event);

        if let Some(ref tx) = self.event_tx {
            // Async mode - send to channel
            if let Err(e) = tx.try_send(obs_event) {
                error!("Failed to send event to logger: {}", e);
            }
        } else {
            // Sync mode - write directly
            if let Err(e) = self.logger.log(&obs_event) {
                error!("Failed to log event: {}", e);
            }
        }
    }

    /// Record token usage
    pub fn record_usage(&self, session: &str, provider: &str, model: &str, usage: &UsageStats) {
        // Update cost tracker
        {
            let mut tracker = self.cost_tracker.write().unwrap();
            tracker.record(session, provider, model, usage);
        }

        // Also log as an event
        let cost = get_model_pricing(provider, model)
            .map(|p| p.calculate(usage.input_tokens, usage.output_tokens, usage.cache_read_tokens));

        self.log_event(
            session,
            "root",
            EventKind::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cost_usd: cost,
            },
        );
    }

    /// Get accumulated usage for a session
    pub fn session_usage(&self, session: &str) -> Option<AccumulatedUsage> {
        let tracker = self.cost_tracker.read().unwrap();
        tracker.session_usage(session).cloned()
    }

    /// Get global accumulated usage
    pub fn global_usage(&self) -> AccumulatedUsage {
        let tracker = self.cost_tracker.read().unwrap();
        tracker.global_usage().clone()
    }

    /// Get cost summary string
    pub fn cost_summary(&self) -> String {
        self.global_usage().summary()
    }

    /// Get the current log file path
    pub fn log_file(&self) -> Option<std::path::PathBuf> {
        self.logger.current_file()
    }
}

impl Default for Observer {
    fn default() -> Self {
        Self::new().expect("Failed to create default observer")
    }
}

/// Convenience functions for creating events

impl EventKind {
    /// Create a prompt event
    pub fn prompt(content: impl Into<String>) -> Self {
        Self::Prompt {
            content: content.into(),
        }
    }

    /// Create a tool call event
    pub fn tool_call(tool: impl Into<String>, input: serde_json::Value) -> Self {
        Self::ToolCall {
            tool: tool.into(),
            input,
        }
    }

    /// Create a tool result event
    pub fn tool_result(
        tool: impl Into<String>,
        output: impl Into<String>,
        success: bool,
        duration_ms: Option<u64>,
    ) -> Self {
        Self::ToolResult {
            tool: tool.into(),
            output: output.into(),
            success,
            duration_ms,
        }
    }

    /// Create a text output event
    pub fn text(content: impl Into<String>) -> Self {
        Self::Text {
            content: content.into(),
        }
    }

    /// Create a completion event
    pub fn complete(next: Option<String>, final_text: Option<String>) -> Self {
        Self::Complete { next, final_text }
    }

    /// Create an error event
    pub fn error(message: impl Into<String>, recoverable: Option<bool>) -> Self {
        Self::Error {
            message: message.into(),
            recoverable,
        }
    }

    /// Create a spawn event
    pub fn spawn(
        role: impl Into<String>,
        tmux_session: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self::Spawn {
            role: role.into(),
            tmux_session,
            model,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_observer_creation() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let observer = Observer::with_config(config).unwrap();
        assert!(observer.log_file().is_some());
    }

    #[test]
    fn test_event_logging() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let observer = Observer::with_config(config).unwrap();

        observer.log_event("session-1", "root", EventKind::prompt("Hello"));
        observer.log_event(
            "session-1",
            "root",
            EventKind::tool_call("bash", serde_json::json!({"command": "ls"})),
        );
        observer.log_event(
            "session-1",
            "root",
            EventKind::tool_result("bash", "file.txt", true, Some(50)),
        );

        // Read back events
        let log_file = observer.log_file().unwrap();
        let events = read_events(&log_file).unwrap();

        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_usage_tracking() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let observer = Observer::with_config(config).unwrap();

        let usage = UsageStats {
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        };

        observer.record_usage("session-1", "anthropic", "claude-sonnet-4", &usage);

        let session_usage = observer.session_usage("session-1").unwrap();
        assert_eq!(session_usage.total_input_tokens, 1000);
        assert_eq!(session_usage.total_output_tokens, 500);
        assert!(session_usage.estimated_cost_usd > 0.0);
    }

    #[test]
    fn test_event_kind_builders() {
        let prompt = EventKind::prompt("Hello");
        matches!(prompt, EventKind::Prompt { content } if content == "Hello");

        let tool = EventKind::tool_call("bash", serde_json::json!({"cmd": "ls"}));
        matches!(tool, EventKind::ToolCall { tool, .. } if tool == "bash");

        let error = EventKind::error("Something went wrong", Some(true));
        matches!(error, EventKind::Error { recoverable: Some(true), .. });
    }
}
