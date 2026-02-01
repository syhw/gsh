use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Messages sent from the shell to the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShellMessage {
    /// Command about to execute (preexec)
    Preexec {
        command: String,
        cwd: String,
        timestamp: DateTime<Utc>,
    },
    /// Command finished executing (precmd)
    Postcmd {
        exit_code: i32,
        cwd: String,
        duration_ms: Option<u64>,
        timestamp: DateTime<Utc>,
    },
    /// Directory changed
    Chpwd {
        old_cwd: String,
        new_cwd: String,
        timestamp: DateTime<Utc>,
    },
    /// LLM prompt request
    Prompt {
        query: String,
        cwd: String,
        session_id: Option<String>,
        stream: bool,
    },
    /// Start interactive chat session
    ChatStart {
        cwd: String,
        session_id: Option<String>,
    },
    /// Message in ongoing chat
    ChatMessage {
        session_id: String,
        message: String,
    },
    /// End chat session
    ChatEnd {
        session_id: String,
    },
    /// Ping to check daemon health
    Ping,
    /// Request daemon shutdown
    Shutdown,
}

/// Messages sent from the daemon to the shell/client
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonMessage {
    /// Acknowledgment of received event
    Ack,
    /// Pong response to ping
    Pong {
        uptime_secs: u64,
        version: String,
    },
    /// Streaming text chunk
    TextChunk {
        text: String,
        done: bool,
    },
    /// Tool use notification
    ToolUse {
        tool: String,
        input: serde_json::Value,
    },
    /// Tool result
    ToolResult {
        tool: String,
        output: String,
        success: bool,
    },
    /// Complete response (non-streaming)
    Response {
        text: String,
        session_id: String,
    },
    /// Error message
    Error {
        message: String,
        code: Option<String>,
    },
    /// Chat session started
    ChatStarted {
        session_id: String,
    },
    /// Shutdown acknowledgment
    ShuttingDown,
}

/// A complete request-response pair for the protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolMessage {
    pub id: String,
    #[serde(flatten)]
    pub payload: ProtocolPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProtocolPayload {
    Shell(ShellMessage),
    Daemon(DaemonMessage),
}
