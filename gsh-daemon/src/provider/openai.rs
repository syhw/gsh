use super::{
    ChatMessage, ChatResponse, ChatRole, ContentBlock, MessageContent, Provider,
    ProviderCapabilities, ProviderError, StreamEvent, ToolDefinition, UsageStats,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::pin::Pin;
use futures_util::StreamExt;
use tracing::{debug, warn};

const DEFAULT_API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI API provider (also supports OpenAI-compatible APIs)
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAIProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        }
    }
}

/// Get model capabilities for OpenAI models
#[allow(dead_code)]
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    // Context windows and capabilities for known OpenAI models
    let (context_tokens, output_tokens, supports_vision) = if model.contains("gpt-4o") {
        (128_000, 16_384, true)
    } else if model.contains("gpt-4-turbo") {
        (128_000, 4_096, true)
    } else if model.contains("gpt-4") {
        (8_192, 4_096, false)
    } else if model.contains("gpt-3.5") {
        (16_385, 4_096, false)
    } else if model.contains("o1") || model.contains("o3") {
        // Reasoning models
        (200_000, 100_000, true)
    } else {
        // Default for unknown models
        (8_192, 4_096, false)
    };

    ProviderCapabilities {
        supports_tools: !model.contains("o1"), // o1 models don't support tools (yet)
        supports_streaming: true,
        supports_system_message: !model.contains("o1"), // o1 uses different system handling
        supports_vision,
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    /// Request usage stats in the final streaming chunk
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct OpenAIMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAIToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Serialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    model: Option<String>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<OpenAIUsage> for UsageStats {
    fn from(usage: OpenAIUsage) -> Self {
        UsageStats {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIChoice {
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIErrorResponse {
    error: OpenAIError,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OpenAIError {
    message: String,
    #[serde(rename = "type")]
    error_type: Option<String>,
    code: Option<String>,
}

#[allow(dead_code)]
impl OpenAIErrorResponse {
    fn into_provider_error(self, status_code: u16) -> ProviderError {
        let msg = self.error.message;
        let code = self.error.code.as_deref();

        match (status_code, code) {
            (401, _) => ProviderError::AuthenticationError(msg),
            (429, _) => ProviderError::RateLimitError {
                message: msg,
                retry_after_secs: None,
            },
            (400, Some("context_length_exceeded")) => ProviderError::ContextLengthExceededError {
                message: msg,
                max_tokens: None,
            },
            (400, _) => ProviderError::InvalidRequestError(msg),
            (404, _) => ProviderError::ModelNotFoundError(msg),
            (500..=599, _) => ProviderError::ServiceError(msg),
            _ => ProviderError::Other(msg),
        }
    }
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
    /// Usage stats (only present in the final chunk when stream_options.include_usage is true)
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIDelta {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<OpenAIStreamFunction>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn convert_messages_to_openai(messages: &[ChatMessage], system: Option<&str>) -> Vec<OpenAIMessage> {
    let mut result = Vec::new();

    // Add system message if provided
    if let Some(sys) = system {
        result.push(OpenAIMessage {
            role: "system".to_string(),
            content: Some(sys.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        match &msg.content {
            MessageContent::Text(text) => {
                result.push(OpenAIMessage {
                    role: match msg.role {
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                    }.to_string(),
                    content: Some(text.clone()),
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
            MessageContent::Blocks(blocks) => {
                // Handle tool calls and tool results
                let mut text_content = String::new();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_content.push_str(text);
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(OpenAIToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: OpenAIFunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { tool_use_id, content, .. } => {
                            tool_results.push((tool_use_id.clone(), content.clone()));
                        }
                    }
                }

                if !text_content.is_empty() || !tool_calls.is_empty() {
                    result.push(OpenAIMessage {
                        role: match msg.role {
                            ChatRole::User => "user",
                            ChatRole::Assistant => "assistant",
                        }.to_string(),
                        content: if text_content.is_empty() { None } else { Some(text_content) },
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                        tool_call_id: None,
                    });
                }

                // Tool results become separate "tool" role messages
                for (tool_id, content) in tool_results {
                    result.push(OpenAIMessage {
                        role: "tool".to_string(),
                        content: Some(content),
                        tool_calls: None,
                        tool_call_id: Some(tool_id),
                    });
                }
            }
        }
    }

    result
}

fn convert_tools_to_openai(tools: &[ToolDefinition]) -> Vec<OpenAITool> {
    tools.iter().map(|t| OpenAITool {
        tool_type: "function".to_string(),
        function: OpenAIFunction {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }).collect()
}

/// Process a single SSE data payload and return all events it produces.
/// Handles the case where tool name and arguments arrive in the same chunk.
/// Accumulated state for deferred MessageComplete emission.
/// OpenAI sends finish_reason and usage in separate chunks, so we accumulate
/// them and only emit MessageComplete when [DONE] arrives.
struct PendingComplete {
    usage: Option<UsageStats>,
    stop_reason: Option<String>,
}

fn process_openai_sse_data(
    data: &str,
    tool_calls: &mut Vec<(String, String, String)>,
    pending: &mut PendingComplete,
) -> Vec<StreamEvent> {
    let mut events = Vec::new();

    if data == "[DONE]" {
        // Emit the final MessageComplete with any accumulated usage/stop_reason
        events.push(StreamEvent::MessageComplete {
            usage: pending.usage.take(),
            stop_reason: pending.stop_reason.take(),
        });
        return events;
    }

    if let Ok(resp) = serde_json::from_str::<OpenAIStreamResponse>(data) {
        if let Some(choice) = resp.choices.first() {
            if let Some(content) = &choice.delta.content {
                if !content.is_empty() {
                    events.push(StreamEvent::TextDelta(content.clone()));
                }
            }

            if let Some(tcs) = &choice.delta.tool_calls {
                for tc in tcs {
                    let idx = tc.index;
                    while tool_calls.len() <= idx {
                        tool_calls.push((String::new(), String::new(), String::new()));
                    }

                    if let Some(id) = &tc.id {
                        tool_calls[idx].0 = id.clone();
                    }

                    if let Some(func) = &tc.function {
                        if let Some(name) = &func.name {
                            tool_calls[idx].1 = name.clone();
                            events.push(StreamEvent::ToolUseStart {
                                id: tool_calls[idx].0.clone(),
                                name: name.clone(),
                            });
                        }
                        if let Some(args) = &func.arguments {
                            if !args.is_empty() {
                                tool_calls[idx].2.push_str(args);
                                events.push(StreamEvent::ToolUseInputDelta(args.clone()));
                            }
                        }
                    }
                }
            }

            // Store finish_reason but don't emit MessageComplete yet - wait for [DONE]
            if let Some(ref finish_reason) = choice.finish_reason {
                pending.stop_reason = Some(finish_reason.clone());
            }
        }

        // Accumulate usage from the usage-only chunk (empty choices, usage present)
        if let Some(usage) = resp.usage {
            pending.usage = Some(usage.into());
        }
    }

    events
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        get_model_capabilities(&self.model)
    }

    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage> {
        let response = self.chat_with_usage(messages, system, tools, max_tokens).await?;
        Ok(response.message)
    }

    async fn chat_with_usage(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatResponse> {
        let request = OpenAIRequest {
            model: self.model.clone(),
            messages: convert_messages_to_openai(&messages, system),
            max_tokens,
            tools: tools.map(convert_tools_to_openai),
            stream: None,
            stream_options: None,
        };

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenAI")?;

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();

            // Try to parse structured error
            if let Ok(error_resp) = serde_json::from_str::<OpenAIErrorResponse>(&body) {
                let provider_error = error_resp.into_provider_error(status_code);
                anyhow::bail!("{}", provider_error);
            }

            anyhow::bail!("OpenAI API error {}: {}", status, body);
        }

        let response: OpenAIResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI response")?;

        let choice = response.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("No response from OpenAI"))?;

        let mut blocks = Vec::new();

        if let Some(content) = choice.message.content {
            if !content.is_empty() {
                blocks.push(ContentBlock::Text { text: content });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                });
            }
        }

        let message = ChatMessage {
            role: ChatRole::Assistant,
            content: if blocks.len() == 1 {
                if let ContentBlock::Text { text } = &blocks[0] {
                    MessageContent::Text(text.clone())
                } else {
                    MessageContent::Blocks(blocks)
                }
            } else {
                MessageContent::Blocks(blocks)
            },
        };

        Ok(ChatResponse {
            message,
            usage: response.usage.map(|u| u.into()),
            stop_reason: choice.finish_reason,
            model: response.model,
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>> {
        let request = OpenAIRequest {
            model: self.model.clone(),
            messages: convert_messages_to_openai(&messages, system),
            max_tokens,
            tools: tools.map(convert_tools_to_openai),
            stream: Some(true),
            stream_options: Some(StreamOptions { include_usage: true }),
        };

        debug!("OpenAI request to {}: model={}", self.base_url, self.model);
        debug!("Request body: {}", serde_json::to_string(&request).unwrap_or_default());

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenAI")?;

        debug!("Response status: {}", response.status());

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI API error {}: {}", status, body);
        }

        let stream = response.bytes_stream();

        // State: (stream, buffer, tool_calls_state, event_queue, pending_complete)
        // event_queue buffers events when a single SSE payload produces multiple
        // (e.g., ToolUseStart + ToolUseInputDelta when name and args arrive together)
        // pending_complete accumulates usage and stop_reason across chunks
        let initial_pending = PendingComplete { usage: None, stop_reason: None };
        let event_stream = futures_util::stream::unfold(
            (stream, String::new(), Vec::<(String, String, String)>::new(), VecDeque::<StreamEvent>::new(), initial_pending),
            |(mut stream, mut buffer, mut tool_calls, mut event_queue, mut pending)| async move {
                loop {
                    // Drain queued events first (one per iteration)
                    if let Some(event) = event_queue.pop_front() {
                        return Some((event, (stream, buffer, tool_calls, event_queue, pending)));
                    }

                    // Parse double-newline delimited SSE events
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        for line in event_str.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                event_queue.extend(process_openai_sse_data(data, &mut tool_calls, &mut pending));
                            }
                        }
                    }

                    // Parse single-newline delimited lines (some OpenAI-compatible APIs use this)
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        let data = line.strip_prefix("data: ").unwrap_or(&line);
                        event_queue.extend(process_openai_sse_data(data, &mut tool_calls, &mut pending));
                    }

                    // If we collected any events, loop back to drain them
                    if !event_queue.is_empty() {
                        continue;
                    }

                    // Read more data
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            let chunk_str = String::from_utf8_lossy(&chunk);
                            debug!("Received chunk: {}", chunk_str);
                            buffer.push_str(&chunk_str);
                        }
                        Some(Err(e)) => {
                            warn!("Stream error: {}", e);
                            return Some((
                                StreamEvent::Error(ProviderError::NetworkError(e.to_string())),
                                (stream, buffer, tool_calls, event_queue, pending),
                            ));
                        }
                        None => {
                            debug!("Stream ended, buffer remaining: {}", buffer);
                            // Try parsing any remaining buffer content
                            if !buffer.is_empty() {
                                let data = buffer.strip_prefix("data: ").unwrap_or(&buffer);
                                event_queue.extend(process_openai_sse_data(data, &mut tool_calls, &mut pending));
                                buffer.clear();
                                if !event_queue.is_empty() {
                                    continue;
                                }
                            }
                            // If stream ends without [DONE], emit pending MessageComplete
                            if pending.usage.is_some() || pending.stop_reason.is_some() {
                                return Some((
                                    StreamEvent::MessageComplete {
                                        usage: pending.usage.take(),
                                        stop_reason: pending.stop_reason.take(),
                                    },
                                    (stream, buffer, tool_calls, event_queue, pending),
                                ));
                            }
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }

    fn estimate_tokens(&self, text: &str) -> u64 {
        // GPT models use roughly 4 chars per token for English text
        // This is a rough estimate; for accurate counts, use the tiktoken library
        (text.len() as u64 + 3) / 4
    }
}
