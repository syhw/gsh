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
        /// Optional provider override (anthropic, openai, moonshot, ollama)
        #[serde(default)]
        provider: Option<String>,
        /// Optional model override
        #[serde(default)]
        model: Option<String>,
        /// Optional flow to execute
        #[serde(default)]
        flow: Option<String>,
        /// Client-side environment info (active Python env, etc.)
        #[serde(default)]
        env_info: Option<EnvInfo>,
    },
    /// List available subagents
    ListAgents,
    /// Kill a subagent
    KillAgent {
        agent_id: u64,
    },
    /// Start interactive chat session
    ChatStart {
        cwd: String,
        session_id: Option<String>,
        /// Client-side environment info
        #[serde(default)]
        env_info: Option<EnvInfo>,
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
    /// List saved chat sessions
    ListSessions {
        #[serde(default)]
        limit: Option<usize>,
    },
    /// Delete a saved session
    DeleteSession {
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
        #[serde(default)]
        resumed: bool,
        #[serde(default)]
        message_count: usize,
    },
    /// List of running agents
    AgentList {
        agents: Vec<AgentInfo>,
    },
    /// Agent killed confirmation
    AgentKilled {
        agent_id: u64,
    },
    /// Flow event during flow execution
    FlowEvent {
        event: String,
        data: serde_json::Value,
    },
    /// List of saved sessions
    SessionList {
        sessions: Vec<SessionInfo>,
    },
    /// Session deleted confirmation
    SessionDeleted {
        session_id: String,
    },
    /// Context was compacted (summarized to save space)
    Compacted {
        original_tokens: usize,
        summary_tokens: usize,
    },
    /// Agent is calling the LLM (waiting for first token)
    Thinking,
    /// Shutdown acknowledgment
    ShuttingDown,
}

/// Information about a saved chat session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub title: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
    pub cwd: String,
}

/// Environment info from the client's shell
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnvInfo {
    /// Active conda/micromamba env name (from CONDA_DEFAULT_ENV)
    pub conda_env: Option<String>,
    /// Active conda/micromamba env path (from CONDA_PREFIX)
    pub conda_prefix: Option<String>,
    /// Active Python venv path (from VIRTUAL_ENV)
    pub virtual_env: Option<String>,
    /// PATH from the client's shell
    pub path: Option<String>,
}

/// Information about a running agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_id: u64,
    pub session_name: String,
    pub task: Option<String>,
    pub cwd: Option<String>,
}

/// A complete request-response pair for the protocol
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ProtocolMessage {
    pub id: String,
    #[serde(flatten)]
    pub payload: ProtocolPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
pub enum ProtocolPayload {
    Shell(ShellMessage),
    Daemon(DaemonMessage),
}
