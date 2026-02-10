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
    /// Context was compacted (summarized to save space)
    Compacted { summary_tokens: usize, original_tokens: usize },
}

// ============================================================================
// Context Engineering Helpers
// ============================================================================

/// Compaction prompt template
pub(crate) const COMPACTION_PROMPT: &str = r#"You are performing a CONTEXT CHECKPOINT. Summarize the conversation so far into a handoff briefing for yourself to continue the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences discovered
- Files modified or read (with paths)
- What remains to be done (clear next steps)
- Any critical data, code snippets, or references needed to continue

Be thorough but concise. Focus on what's needed to seamlessly continue."#;

/// Estimate token count from text (bytes/4 heuristic, same as Codex)
pub(crate) fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Estimate token count for a ChatMessage
pub(crate) fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    let content_tokens = match &msg.content {
        MessageContent::Text(t) => estimate_tokens(t),
        MessageContent::Blocks(blocks) => {
            blocks.iter().map(|b| match b {
                ContentBlock::Text { text } => estimate_tokens(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    estimate_tokens(name) + estimate_tokens(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
            }).sum()
        }
    };
    content_tokens + 4 // per-message overhead
}

/// Estimate total tokens across all messages
pub(crate) fn estimate_total_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

/// Find the nearest char boundary at or before the given byte position
fn find_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() { return s.len(); }
    let mut p = pos;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Truncate tool output to max_bytes using prefix-suffix strategy
pub(crate) fn truncate_tool_output(output: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || output.len() <= max_bytes {
        return output.to_string();
    }
    let keep = max_bytes / 2;
    let prefix_end = find_char_boundary(output, keep);
    let suffix_start = find_char_boundary(output, output.len().saturating_sub(keep));
    let prefix = &output[..prefix_end];
    let suffix = &output[suffix_start..];
    let omitted = output.len() - prefix.len() - suffix.len();
    format!("{}\n\n... ({} bytes omitted) ...\n\n{}", prefix, omitted, suffix)
}

/// Prune old tool outputs beyond a protection window
///
/// Walks backwards through messages counting tool output tokens.
/// Once we've seen `protect_tokens` worth of tool outputs, replaces
/// older (larger) tool outputs with a placeholder.
pub(crate) fn prune_old_tool_outputs(messages: &mut [ChatMessage], protect_tokens: usize) {
    let mut seen_tokens: usize = 0;
    let mut pruned_count = 0usize;

    for msg in messages.iter_mut().rev() {
        if let MessageContent::Blocks(blocks) = &mut msg.content {
            for block in blocks.iter_mut().rev() {
                if let ContentBlock::ToolResult { content, .. } = block {
                    let tokens = estimate_tokens(content);
                    if seen_tokens > protect_tokens && tokens > 100 {
                        *content = format!("[output pruned, was ~{} tokens]", tokens);
                        pruned_count += 1;
                    }
                    seen_tokens += tokens;
                }
            }
        }
    }

    if pruned_count > 0 {
        tracing::debug!("Pruned {} old tool outputs (protected {} tokens)", pruned_count, protect_tokens);
    }
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
    /// Flow name for cost aggregation (set when running inside a flow)
    flow_name: Option<String>,
    /// Custom tool handlers (e.g., publication tools for flow coordination)
    custom_handlers: Vec<Arc<dyn CustomToolHandler>>,
    /// Max bytes for a single tool output before truncation
    max_tool_output_bytes: usize,
    /// Tokens worth of recent tool outputs to protect from pruning
    prune_protect_tokens: usize,
    /// Context window limit from provider (in tokens)
    context_limit: usize,
    /// Fraction of context window at which to trigger compaction
    compact_threshold: f64,
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, config: &Config, cwd: String) -> Self {
        Self::new_with_env(provider, config, cwd, None, None)
    }

    pub fn new_with_env(
        provider: Box<dyn Provider>,
        config: &Config,
        cwd: String,
        env_info: Option<EnvInfo>,
        context_retriever: Option<Arc<crate::context::ContextRetriever>>,
    ) -> Self {
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

        let context_limit = provider.capabilities().max_context_tokens
            .map(|t| t as usize)
            .unwrap_or(100_000);

        Self {
            provider,
            tool_executor: ToolExecutor::new(config.clone(), cwd, context_retriever),
            system_prompt,
            max_tokens: config.llm.max_tokens,
            max_iterations: config.context.max_iterations,
            observer: None,
            session_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "root".to_string(),
            flow_name: None,
            custom_handlers: Vec::new(),
            max_tool_output_bytes: config.context.max_tool_output_bytes,
            prune_protect_tokens: config.context.prune_protect_tokens,
            context_limit,
            compact_threshold: config.context.compact_threshold,
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

    /// Set the flow name for cost aggregation
    pub fn with_flow_name(mut self, flow_name: impl Into<String>) -> Self {
        self.flow_name = Some(flow_name.into());
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
            if let Some(ref flow_name) = self.flow_name {
                observer.record_flow_usage(
                    flow_name,
                    &self.session_id,
                    self.provider.name(),
                    self.provider.model(),
                    usage,
                );
            } else {
                observer.record_usage(
                    &self.session_id,
                    self.provider.name(),
                    self.provider.model(),
                    usage,
                );
            }
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

    /// Check if we should trigger compaction
    fn should_compact(&self, messages: &[ChatMessage]) -> bool {
        let total = estimate_total_tokens(messages);
        let threshold = (self.context_limit as f64 * self.compact_threshold) as usize;
        total > threshold
    }

    /// Compact the conversation by summarizing it via the LLM
    async fn compact(
        &self,
        messages: Vec<ChatMessage>,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<Vec<ChatMessage>> {
        let original_tokens = estimate_total_tokens(&messages);
        tracing::info!("Compacting context: ~{} tokens (limit: {})", original_tokens, self.context_limit);

        // Build compaction request: full history + compaction prompt
        let messages_before = messages.len();
        let mut compact_messages = messages;
        compact_messages.push(ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text(COMPACTION_PROMPT.to_string()),
        });

        // Use the same provider to generate the summary (non-streaming for simplicity)
        let summary_response = self.provider.chat(
            compact_messages,
            Some(&self.system_prompt),
            None, // no tools during compaction
            self.max_tokens,
        ).await?;

        let summary_text = match &summary_response.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Blocks(blocks) => {
                blocks.iter().filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n")
            }
        };

        let summary_tokens = estimate_tokens(&summary_text);

        // Log compaction event
        self.log_event(EventKind::Compaction {
            original_tokens,
            summary_tokens,
            messages_before,
            messages_after: 1,
        });

        // Notify client
        let _ = event_tx.send(AgentEvent::Compacted {
            summary_tokens,
            original_tokens,
        }).await;

        tracing::info!("Compacted: {} → {} tokens", original_tokens, summary_tokens);

        // Return new message history with just the summary
        Ok(vec![ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text(format!(
                "[Context compacted - previous conversation summary]\n\n{}",
                summary_text
            )),
        }])
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
            // Layer 2: Prune old tool outputs before each LLM call
            prune_old_tool_outputs(&mut messages, self.prune_protect_tokens);

            // Layer 3: Check if compaction is needed
            if self.should_compact(&messages) {
                let to_compact = std::mem::take(&mut messages);
                match self.compact(to_compact, &event_tx).await {
                    Ok(compacted) => messages = compacted,
                    Err(e) => {
                        tracing::warn!("Compaction failed, continuing with full context: {}", e);
                        // messages was taken, but compact failed — we lost it
                        // This is a fatal situation, bail out
                        return Err(e.context("Compaction failed and context was consumed"));
                    }
                }
            }

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

                // Layer 1: Truncate large tool outputs
                let original_len = output.len();
                let output = truncate_tool_output(&output, self.max_tool_output_bytes);
                if output.len() < original_len {
                    self.log_event(EventKind::Truncation {
                        tool: name.clone(),
                        original_bytes: original_len,
                        truncated_bytes: output.len(),
                    });
                }

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
        (home.join(".local/share/mamba"), "mamba"),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Token estimation tests =====

    #[test]
    fn test_estimate_tokens_empty() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn test_estimate_tokens_short() {
        // "hello" = 5 bytes / 4 = 1 token
        assert_eq!(estimate_tokens("hello"), 1);
    }

    #[test]
    fn test_estimate_tokens_longer() {
        // 400 bytes / 4 = 100 tokens
        let text = "a".repeat(400);
        assert_eq!(estimate_tokens(&text), 100);
    }

    #[test]
    fn test_estimate_message_tokens_text() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("hello world".to_string()),
        };
        // 11 bytes / 4 = 2, + 4 overhead = 6
        assert_eq!(estimate_message_tokens(&msg), 6);
    }

    #[test]
    fn test_estimate_message_tokens_blocks() {
        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Blocks(vec![
                ContentBlock::Text { text: "hello".to_string() },
                ContentBlock::ToolResult {
                    tool_use_id: "1".into(),
                    content: "result data".to_string(),
                    is_error: Some(false),
                },
            ]),
        };
        // "hello" = 1, "result data" = 2, + 4 overhead = 7
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(tokens, 7);
    }

    #[test]
    fn test_estimate_total_tokens() {
        let messages = vec![
            ChatMessage {
                role: ChatRole::User,
                content: MessageContent::Text("a".repeat(400)),
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: MessageContent::Text("b".repeat(200)),
            },
        ];
        // (400/4 + 4) + (200/4 + 4) = 104 + 54 = 158
        assert_eq!(estimate_total_tokens(&messages), 158);
    }

    // ===== find_char_boundary tests =====

    #[test]
    fn test_find_char_boundary_ascii() {
        let s = "hello world";
        assert_eq!(find_char_boundary(s, 5), 5);
    }

    #[test]
    fn test_find_char_boundary_beyond_len() {
        let s = "hi";
        assert_eq!(find_char_boundary(s, 100), 2);
    }

    #[test]
    fn test_find_char_boundary_multibyte() {
        // "Héllo" — 'é' is 2 bytes (0xC3, 0xA9), so byte 2 is mid-char
        let s = "Héllo";
        assert!(s.len() > 5); // 6 bytes
        // byte 2 is inside 'é', should snap back to byte 1
        assert_eq!(find_char_boundary(s, 2), 1);
        // byte 3 is the start of 'l'
        assert_eq!(find_char_boundary(s, 3), 3);
    }

    // ===== truncate_tool_output tests =====

    #[test]
    fn test_truncate_small_output() {
        let output = "small output";
        let result = truncate_tool_output(output, 100);
        assert_eq!(result, "small output");
    }

    #[test]
    fn test_truncate_zero_limit() {
        let output = "anything";
        let result = truncate_tool_output(output, 0);
        assert_eq!(result, "anything");
    }

    #[test]
    fn test_truncate_large_output() {
        let output = "A".repeat(100);
        let result = truncate_tool_output(&output, 40);
        // Should have prefix (20 chars) + omission marker + suffix (20 chars)
        assert!(result.contains("... ("));
        assert!(result.contains("bytes omitted) ..."));
        // Prefix should be the first 20 A's
        assert!(result.starts_with(&"A".repeat(20)));
        // Suffix should be the last 20 A's
        assert!(result.ends_with(&"A".repeat(20)));
        // Total should be smaller than original
        assert!(result.len() < output.len() + 50); // some overhead from marker
    }

    #[test]
    fn test_truncate_multibyte_safe() {
        // Create a string with multi-byte characters
        let output = "日本語".repeat(100); // each char is 3 bytes
        let result = truncate_tool_output(&output, 50);
        // Should not panic — the result should be valid UTF-8
        assert!(result.contains("bytes omitted"));
    }

    // ===== prune_old_tool_outputs tests =====

    fn make_tool_result_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "test".to_string(),
                content: content.to_string(),
                is_error: Some(false),
            }]),
        }
    }

    #[test]
    fn test_prune_within_protection_window() {
        let mut messages = vec![
            make_tool_result_message("short output"),
            make_tool_result_message("another short one"),
        ];
        // protect_tokens = 1000, these are tiny — nothing should be pruned
        prune_old_tool_outputs(&mut messages, 1000);

        // Verify nothing was pruned
        for msg in &messages {
            if let MessageContent::Blocks(blocks) = &msg.content {
                for block in blocks {
                    if let ContentBlock::ToolResult { content, .. } = block {
                        assert!(!content.contains("pruned"));
                    }
                }
            }
        }
    }

    #[test]
    fn test_prune_old_outputs_beyond_window() {
        // Create messages with large tool outputs
        let large_output = "x".repeat(2000); // 2000 bytes = ~500 tokens
        let mut messages = vec![
            make_tool_result_message(&large_output), // oldest - should be pruned
            make_tool_result_message(&large_output), // middle - should be pruned
            make_tool_result_message(&large_output), // newest - protected
        ];

        // Protect only 400 tokens — last message ~500 tokens, so oldest two should be pruned
        prune_old_tool_outputs(&mut messages, 400);

        // Check newest (last) is preserved
        if let MessageContent::Blocks(blocks) = &messages[2].content {
            if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
                assert!(!content.contains("pruned"), "Newest should be preserved");
            }
        }

        // Check oldest (first) is pruned
        if let MessageContent::Blocks(blocks) = &messages[0].content {
            if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
                assert!(content.contains("pruned"), "Oldest should be pruned");
            }
        }
    }

    #[test]
    fn test_prune_skips_small_outputs() {
        // Small outputs (< 100 tokens) should never be pruned
        let small_output = "ok"; // 0 tokens estimate — below 100 threshold
        let large_output = "x".repeat(2000); // ~500 tokens

        let mut messages = vec![
            make_tool_result_message(small_output),  // small — should NOT be pruned
            make_tool_result_message(&large_output), // large — protected (newest)
        ];

        prune_old_tool_outputs(&mut messages, 0); // protect nothing

        // Small output should still be intact (below 100-token threshold)
        if let MessageContent::Blocks(blocks) = &messages[0].content {
            if let ContentBlock::ToolResult { content, .. } = &blocks[0] {
                assert_eq!(content, "ok", "Small outputs should not be pruned");
            }
        }
    }

    // ===== Config defaults tests =====

    #[test]
    fn test_context_config_defaults() {
        let config: crate::config::ContextConfig = Default::default();
        assert_eq!(config.max_tool_output_bytes, 30_000);
        assert_eq!(config.prune_protect_tokens, 40_000);
        assert!((config.compact_threshold - 0.85).abs() < f64::EPSILON);
        assert_eq!(config.max_iterations, 25);
    }
}
