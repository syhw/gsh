pub mod tools;

use crate::config::Config;
use crate::provider::{
    ChatMessage, ChatRole, ContentBlock, MessageContent, Provider, StreamEvent, ToolDefinition,
};
use anyhow::Result;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tools::ToolExecutor;

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
}

impl Agent {
    pub fn new(provider: Box<dyn Provider>, config: &Config, cwd: String) -> Self {
        let default_system = format!(
            r#"You are an AI assistant integrated into a shell environment. You help users with coding, system administration, and general tasks.

Current working directory: {}

You have access to tools for interacting with the file system and running commands. Use them when helpful to accomplish the user's request.

Guidelines:
- Be concise and direct in your responses
- When asked to perform actions, use the available tools
- For file operations, prefer using absolute paths
- Explain what you're doing when running commands or modifying files
- If a command fails, analyze the error and suggest solutions"#,
            cwd
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
        }
    }

    /// Get tool definitions for the LLM
    pub fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_executor.definitions()
    }

    /// Run the agent with a single query (one-shot)
    pub async fn run_oneshot(
        &self,
        query: &str,
        context: Option<&str>,
        event_tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String> {
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

        for _ in 0..self.max_iterations {
            // Call the LLM with streaming
            let mut stream = self
                .provider
                .chat_stream(
                    messages.clone(),
                    Some(&self.system_prompt),
                    Some(&tools),
                    self.max_tokens,
                )
                .await?;

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
                    StreamEvent::MessageComplete => {
                        // Save last tool's input
                        if let Some(idx) = current_tool_idx {
                            tool_uses[idx].2 = std::mem::take(&mut current_tool_input);
                        }
                        break;
                    }
                    StreamEvent::Error(e) => {
                        let _ = event_tx.send(AgentEvent::Error(e.clone())).await;
                        anyhow::bail!("Stream error: {}", e);
                    }
                }
            }

            // If no tool uses, we're done
            if tool_uses.is_empty() {
                final_text = response_text;
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
                let input: serde_json::Value = serde_json::from_str(&input_json)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

                let _ = event_tx.send(AgentEvent::ToolStart {
                    name: name.clone(),
                    input: input.clone(),
                }).await;

                let result = self.tool_executor.execute(&name, &input).await;
                let (output, success) = match result {
                    Ok(output) => (output, true),
                    Err(e) => (format!("Error: {}", e), false),
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
