//! Rich event types for observability
//!
//! These events are designed for structured logging and can be serialized
//! to JSONL for analysis and debugging.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A timestamped event from the agent system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityEvent {
    /// Timestamp of the event
    pub ts: DateTime<Utc>,
    /// Session ID
    pub session: String,
    /// Agent ID (e.g., "root", "agent-1")
    pub agent: String,
    /// The event details
    #[serde(flatten)]
    pub event: EventKind,
}

impl ObservabilityEvent {
    /// Create a new event with the current timestamp
    pub fn new(session: impl Into<String>, agent: impl Into<String>, event: EventKind) -> Self {
        Self {
            ts: Utc::now(),
            session: session.into(),
            agent: agent.into(),
            event,
        }
    }
}

/// The kind of event that occurred
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventKind {
    /// User submitted a prompt
    Prompt {
        content: String,
    },

    /// Agent was spawned
    Spawn {
        role: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tmux_session: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },

    /// Agent started processing
    Start {
        #[serde(skip_serializing_if = "Option::is_none")]
        flow: Option<String>,
    },

    /// Text output from the agent
    Text {
        content: String,
    },

    /// Tool was called
    ToolCall {
        tool: String,
        input: serde_json::Value,
    },

    /// Tool returned a result
    ToolResult {
        tool: String,
        output: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },

    /// Bash command executed with detailed I/O capture
    BashExec {
        command: String,
        stdout: String,
        stderr: String,
        exit_code: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },

    /// Agent completed its work
    Complete {
        #[serde(skip_serializing_if = "Option::is_none")]
        next: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_text: Option<String>,
    },

    /// Error occurred
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        recoverable: Option<bool>,
    },

    /// Token usage for a request
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_creation_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// Provider name (for per-model dashboard tracking)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        provider: Option<String>,
        /// Model name (for per-model dashboard tracking)
        #[serde(skip_serializing_if = "Option::is_none", default)]
        model: Option<String>,
    },

    /// Agent was paused (steering control)
    Paused,

    /// Agent was resumed (steering control)
    Resumed,

    /// External message injected into agent context
    Inject {
        content: String,
    },

    /// Agent redirected to a different flow node
    Redirect {
        from: String,
        to: String,
    },

    /// Flow started
    FlowStart {
        flow_name: String,
        entry_node: String,
    },

    /// Flow completed
    FlowComplete {
        flow_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_agents: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        total_duration_ms: Option<u64>,
    },

    /// Iteration within the agent loop
    Iteration {
        iteration: usize,
        max_iterations: usize,
    },

    /// Context was compacted (summarized to save space)
    Compaction {
        original_tokens: usize,
        summary_tokens: usize,
        messages_before: usize,
        messages_after: usize,
    },

    /// Tool output was truncated
    Truncation {
        tool: String,
        original_bytes: usize,
        truncated_bytes: usize,
    },
}

/// Summary of a completed agent run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunSummary {
    pub session_id: String,
    pub agent_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub iterations: usize,
    pub tool_calls: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_cost_usd: f64,
    pub success: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_serialization() {
        let event = ObservabilityEvent::new(
            "session-123",
            "root",
            EventKind::ToolCall {
                tool: "bash".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            },
        );

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"tool_call\""));
        assert!(json.contains("\"tool\":\"bash\""));
        assert!(json.contains("\"session\":\"session-123\""));
    }

    #[test]
    fn test_usage_event() {
        let event = ObservabilityEvent::new(
            "session-123",
            "root",
            EventKind::Usage {
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: Some(200),
                cache_creation_tokens: None,
                cost_usd: Some(0.015),
                provider: Some("anthropic".to_string()),
                model: Some("claude-sonnet-4".to_string()),
            },
        );

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"input_tokens\":1000"));
        assert!(json.contains("\"cost_usd\":0.015"));
        // None values should not appear in JSON
        assert!(!json.contains("cache_creation_tokens"));
    }
}
