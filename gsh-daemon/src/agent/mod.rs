pub mod tools;

use crate::config::Config;
use crate::observability::{EventKind, Observer};
use crate::protocol::EnvInfo;
use crate::provider::{
    ChatMessage, ChatRole, ContentBlock, MessageContent, Provider, StreamEvent, ToolDefinition,
    UsageStats,
};
use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tools::{ToolExecutor, ToolResult as ExecResult};

/// Result from a custom tool execution
#[derive(Debug, Clone)]
pub struct CustomToolResult {
    pub output: String,
    pub success: bool,
}

/// Trait for custom tool handlers (e.g., publication tools)
#[async_trait]
pub trait CustomToolHandler: Send + Sync {
    /// Check if this handler handles the given tool name
    fn handles(&self, tool_name: &str) -> bool;

    /// Execute a tool and return the result
    async fn execute(&self, tool_name: &str, input: &serde_json::Value) -> CustomToolResult;

    /// Get tool definitions for tools this handler provides
    fn tool_definitions(&self) -> Vec<ToolDefinition>;
}

/// Events emitted by the agent during execution
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Text chunk from the LLM
    TextChunk(String),
    /// Tool use started
    ToolStart { name: String, input: serde_json::Value },
    /// Tool execution completed
    ToolResult { name: String, output: String, success: bool },
    /// Agent finished
    Done { final_text: String },
    /// Error occurred
    Error(String),
}

/// The agentic loop that processes messages and executes tools
pub struct Agent {
    provider: Box<dyn Provider>,
    tool_executor: ToolExecutor,
    system_prompt: String,
    max_tokens: u32,
    max_iterations: usize,
    /// Optional observer for logging and metrics
    observer: Option<Arc<Observer>>,
    /// Session ID for observability
    session_id: String,
    /// Agent ID within the session
    agent_id: String,
    /// Custom tool handlers (e.g., publication tools for flow coordination)
    custom_handlers: Vec<Arc<dyn CustomToolHandler>>,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, config: &Config, cwd: String) -> Self {
        Self::new_with_env(provider, config, cwd, None)
    }

    pub fn new_with_env(provider: Box<dyn Provider>, config: &Config, cwd: String, env_info: Option<EnvInfo>) -> Self {
        let python_env = detect_python_env(&cwd, env_info.as_ref());

        let default_system = format!(
            r#"You are an AI assistant integrated into a shell environment. You help users with coding, system administration, and general tasks.

Current working directory: {}
Platform: {}

{}

You have access to tools for interacting with the file system and running commands. Use them when helpful to accomplish the user's request.

Guidelines:
- Be concise and direct in your responses
- When asked to perform actions, use the available tools
- For file operations, prefer using absolute paths
- Explain what you're doing when running commands or modifying files
- If a command fails, analyze the error and suggest solutions
- On macOS, use `python3` not `python`, and `pip3` not `pip`"#,
            cwd,
            std::env::consts::OS,
            python_env,
        );

        let system_prompt = config
            .llm
            .system_prompt
            .clone()
            .unwrap_or(default_system);

        Self {
            provider,
            tool_executor: ToolExecutor::new(config.clone(), cwd),
            system_prompt,
            max_tokens: config.llm.max_tokens,
            max_iterations: 10,
            observer: None,
            session_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "root".to_string(),
            custom_handlers: Vec::new(),
        }
    }

    /// Add a custom tool handler
    pub fn with_custom_handler(mut self, handler: Arc<dyn CustomToolHandler>) -> Self {
        self.custom_handlers.push(handler);
        self
    }

    /// Set the observer for logging and metrics
    pub fn with_observer(mut self, observer: Arc<Observer>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Set the session ID
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    /// Set the agent ID
    pub fn with_agent_id(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = agent_id.into();
        self
    }

    /// Log an event if observer is present
    fn log_event(&self, event: EventKind) {
        if let Some(ref observer) = self.observer {
            observer.log_event(&self.session_id, &self.agent_id, event);
        }
    }

    /// Record usage if observer is present
    fn record_usage(&self, usage: &UsageStats) {
        if let Some(ref observer) = self.observer {
            observer.record_usage(
                &self.session_id,
                self.provider.name(),
                self.provider.model(),
                usage,
            );
        }
    }

    /// Get tool definitions for the LLM (including custom tools)
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = self.tool_executor.definitions();
        // Add custom tool definitions
        for handler in &self.custom_handlers {
            tools.extend(handler.tool_definitions());
        }
        tools
    }

    /// Try to execute a tool using custom handlers
    async fn try_custom_execute(&self, name: &str, input: &serde_json::Value) -> Option<CustomToolResult> {
        for handler in &self.custom_handlers {
            if handler.handles(name) {
                return Some(handler.execute(name, input).await);
            }
        }
        None
    }

    /// Run the agent with a single query (one-shot)
    pub async fn run_oneshot(
        &self,
        query: &str,
        context: Option<&str>,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String> {
        // Log the prompt
        self.log_event(EventKind::prompt(query));

        let mut messages = Vec::new();

        // Build the user message with context
        let user_content = if let Some(ctx) = context {
            format!("{}\n\n---\n\nUser request: {}", ctx, query)
        } else {
            query.to_string()
        };

        messages.push(ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text(user_content),
        });

        self.run_loop(messages, event_tx).await
    }

    /// Run the agent with existing message history
    pub async fn run_with_history(
        &self,
        messages: Vec<ChatMessage>,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String> {
        self.run_loop(messages, event_tx).await
    }

    async fn run_loop(
        &self,
        mut messages: Vec<ChatMessage>,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String> {
        let tools = self.tool_definitions();
        let mut final_text = String::new();

        // Log start event
        self.log_event(EventKind::Start { flow: None });

        for iteration in 0..self.max_iterations {
            // Log iteration
            self.log_event(EventKind::Iteration {
                iteration,
                max_iterations: self.max_iterations,
            });
            // Call the LLM with streaming
            let stream_result = self
                .provider
                .chat_stream(
                    messages.clone(),
                    Some(&self.system_prompt),
                    Some(&tools),
                    self.max_tokens,
                )
                .await;

            let mut stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    let error_msg = e.to_string();
                    self.log_event(EventKind::error(&error_msg, Some(false)));
                    let _ = event_tx.send(AgentEvent::Error(error_msg.clone())).await;
                    anyhow::bail!("{}", error_msg);
                }
            };

            let mut response_text = String::new();
            let mut tool_uses: Vec<(String, String, String)> = Vec::new(); // (id, name, input_json)
            let mut current_tool_input = String::new();
            let mut current_tool_idx: Option<usize> = None;

            // Process the stream
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::TextDelta(text) => {
                        response_text.push_str(&text);
                        let _ = event_tx.send(AgentEvent::TextChunk(text)).await;
                    }
                    StreamEvent::ToolUseStart { id, name } => {
                        // Save previous tool's input if any
                        if let Some(idx) = current_tool_idx {
                            tool_uses[idx].2 = std::mem::take(&mut current_tool_input);
                        }
                        current_tool_idx = Some(tool_uses.len());
                        tool_uses.push((id, name, String::new()));
                    }
                    StreamEvent::ToolUseInputDelta(json) => {
                        current_tool_input.push_str(&json);
                    }
                    StreamEvent::MessageComplete { usage, stop_reason: _ } => {
                        // Save last tool's input
                        if let Some(idx) = current_tool_idx {
                            tool_uses[idx].2 = std::mem::take(&mut current_tool_input);
                        }
                        // Record usage if available
                        if let Some(ref usage_stats) = usage {
                            self.record_usage(usage_stats);
                        }
                        break;
                    }
                    StreamEvent::Error(e) => {
                        let error_msg = e.to_string();
                        self.log_event(EventKind::error(&error_msg, Some(false)));
                        let _ = event_tx.send(AgentEvent::Error(error_msg.clone())).await;
                        anyhow::bail!("Stream error: {}", error_msg);
                    }
                }
            }

            // Safety net: if stream ended without MessageComplete, save any
            // accumulated tool input that wasn't flushed
            if let Some(idx) = current_tool_idx {
                if !current_tool_input.is_empty() && tool_uses[idx].2.is_empty() {
                    tool_uses[idx].2 = std::mem::take(&mut current_tool_input);
                }
            }

            // If no tool uses, we're done
            if tool_uses.is_empty() {
                final_text = response_text;
                // Log completion
                self.log_event(EventKind::complete(None, Some(final_text.clone())));
                let _ = event_tx.send(AgentEvent::Done { final_text: final_text.clone() }).await;
                break;
            }

            // Build the assistant message with tool uses
            let mut content_blocks = Vec::new();
            if !response_text.is_empty() {
                content_blocks.push(ContentBlock::Text { text: response_text.clone() });
            }

            for (id, name, input_json) in &tool_uses {
                let input: serde_json::Value = serde_json::from_str(input_json)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                content_blocks.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
            }

            messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: MessageContent::Blocks(content_blocks),
            });

            // Execute tools and collect results
            let mut tool_results = Vec::new();

            for (id, name, input_json) in tool_uses {
                // Debug: log the raw input JSON
                tracing::debug!("Tool '{}' raw input_json: '{}'", name, input_json);

                let input: serde_json::Value = match serde_json::from_str(&input_json) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("Failed to parse tool input JSON for '{}': {}. Raw: '{}'", name, e, input_json);
                        serde_json::Value::Object(serde_json::Map::new())
                    }
                };

                // Log tool call
                self.log_event(EventKind::tool_call(&name, input.clone()));

                let _ = event_tx.send(AgentEvent::ToolStart {
                    name: name.clone(),
                    input: input.clone(),
                }).await;

                let start_time = Instant::now();

                // Try custom handlers first, then fall back to built-in executor
                let (output, success) = if let Some(custom_result) = self.try_custom_execute(&name, &input).await {
                    let duration_ms = start_time.elapsed().as_millis() as u64;
                    self.log_event(EventKind::tool_result(&name, &custom_result.output, custom_result.success, Some(duration_ms)));
                    (custom_result.output, custom_result.success)
                } else {
                    let result = self.tool_executor.execute(&name, &input).await;
                    let duration_ms = start_time.elapsed().as_millis() as u64;

                    let (output, success) = match &result {
                        Ok(exec_result) => (exec_result.as_string(), exec_result.is_success()),
                        Err(e) => (format!("Error: {}", e), false),
                    };

                    // Log detailed bash execution if applicable
                    if let Ok(ExecResult::Bash(bash_result)) = &result {
                        self.log_event(EventKind::BashExec {
                            command: bash_result.command.clone(),
                            stdout: bash_result.stdout.clone(),
                            stderr: bash_result.stderr.clone(),
                            exit_code: bash_result.exit_code,
                            duration_ms: Some(bash_result.duration_ms),
                        });
                    } else {
                        // Log generic tool result for non-bash tools
                        self.log_event(EventKind::tool_result(&name, &output, success, Some(duration_ms)));
                    }

                    (output, success)
                };

                let _ = event_tx.send(AgentEvent::ToolResult {
                    name: name.clone(),
                    output: output.clone(),
                    success,
                }).await;

                tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: id,
                    content: output,
                    is_error: Some(!success),
                });
            }

            // Add tool results as a user message
            messages.push(ChatMessage {
                role: ChatRole::User,
                content: MessageContent::Blocks(tool_results),
            });
        }

        Ok(final_text)
    }
}

/// Detect available Python environments and return system prompt section
fn detect_python_env(cwd: &str, env_info: Option<&EnvInfo>) -> String {
    let home = dirs::home_dir().unwrap_or_default();
    let cwd_path = std::path::Path::new(cwd);
    let gsh_venv = home.join(".local").join("share").join("gsh").join("python-env");

    // Check for active env from client's shell (highest priority)
    if let Some(info) = env_info {
        // Active conda/micromamba env
        if let (Some(env_name), Some(prefix)) = (&info.conda_env, &info.conda_prefix) {
            tracing::info!("Python env: conda/micromamba '{}' at {}", env_name, prefix);
            tracing::info!("  activate: micromamba activate {} (or conda activate {})", env_name, env_name);
            return format!(
                r#"Python environment: conda/micromamba environment '{}' is active.
Path: {}
IMPORTANT: When running Python commands, first activate with: eval "$(micromamba shell hook -s bash)" && micromamba activate {}
Or prefix commands with the full path: {}/bin/python3"#,
                env_name, prefix, env_name, prefix
            );
        }
        // Active venv
        if let Some(venv) = &info.virtual_env {
            tracing::info!("Python env: venv at {}", venv);
            tracing::info!("  activate: source {}/bin/activate", venv);
            return format!(
                r#"Python environment: venv is active.
Path: {}
IMPORTANT: When running Python commands, first activate with: source {}/bin/activate
Or prefix commands with the full path: {}/bin/python3"#,
                venv, venv, venv
            );
        }
    }

    let mut found = Vec::new();

    // Check for gsh-managed venv
    let gsh_venv_active = gsh_venv.join("bin").join("python3").exists();
    if gsh_venv_active {
        found.push(format!(
            "- gsh managed venv: {} (activate with `source {}/bin/activate`)",
            gsh_venv.display(),
            gsh_venv.display()
        ));
    }

    // Check for local venv/.venv in cwd
    for name in &[".venv", "venv"] {
        let venv = cwd_path.join(name);
        if venv.join("bin").join("python3").exists() {
            found.push(format!(
                "- Project venv: {}/{}  (activate with `source {}/{}/bin/activate`)",
                cwd, name, cwd, name
            ));
        }
    }

    // Check for conda/micromamba
    let conda_paths = [
        (home.join("miniconda3"), "conda (miniconda3)"),
        (home.join("anaconda3"), "conda (anaconda3)"),
        (home.join("miniforge3"), "conda (miniforge3)"),
        (home.join("mambaforge"), "mamba (mambaforge)"),
        (home.join("micromamba"), "micromamba"),
        (home.join(".local/share/micromamba"), "micromamba"),
    ];
    for (path, label) in &conda_paths {
        if path.exists() {
            found.push(format!("- {}: {}", label, path.display()));
        }
    }

    // Check for pyenv
    let pyenv = home.join(".pyenv");
    if pyenv.exists() {
        found.push(format!("- pyenv: {}", pyenv.display()));
    }

    if found.is_empty() {
        let venv_path = gsh_venv.display().to_string();
        tracing::info!("Python env: none detected. Will use: {}", venv_path);
        format!(
            r#"Python environment: None detected.
When Python is needed, create a venv at {} using:
  python3 -m venv {}
  source {}/bin/activate"#,
            venv_path, venv_path, venv_path,
        )
    } else {
        let preferred = if gsh_venv_active {
            let venv_path = gsh_venv.display().to_string();
            tracing::info!("Python env: gsh venv at {}", venv_path);
            tracing::info!("  activate: source {}/bin/activate", venv_path);
            format!(
                "\nPreferred: use the gsh venv (`source {}/bin/activate`) before running Python commands.",
                venv_path
            )
        } else if found.iter().any(|f| f.contains("Project venv")) {
            tracing::info!("Python env: project venv in {}", cwd);
            tracing::info!("  activate: source {}/.venv/bin/activate", cwd);
            "\nPreferred: use the project venv found in the current directory.".to_string()
        } else {
            if let Some(first) = found.first() {
                tracing::info!("Python env: {}", first.trim_start_matches("- "));
            }
            let venv_path = gsh_venv.display().to_string();
            tracing::info!("  For pip installs, create: python3 -m venv {}", venv_path);
            format!(
                "\nWhen installing packages, create/use a venv at {} to avoid polluting the system Python.",
                venv_path
            )
        };

        format!("Python environments found:\n{}{}", found.join("\n"), preferred)
    }
}
