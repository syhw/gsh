//! OpenAI Responses API provider (for gpt-5.x-codex and newer models)
//!
//! Uses the /v1/responses endpoint with streaming SSE events.
//! Different from the Chat Completions API used by the standard OpenAI provider.

use super::{
    ChatMessage, ChatResponse, ChatRole, ContentBlock, MessageContent, Provider,
    ProviderCapabilities, ProviderError, StreamEvent, ToolDefinition, UsageStats,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::pin::Pin;
const DEFAULT_API_URL: &str = "https://api.openai.com/v1/responses";

pub struct OpenAIResponsesProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        }
    }
}

// --- Request types ---

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    input: serde_json::Value, // string or array of input items
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// --- Response types (non-streaming) ---

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[allow(dead_code)]
    id: String,
    output: Vec<ResponsesOutputItem>,
    usage: Option<ResponsesUsage>,
    #[allow(dead_code)]
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesOutputItem {
    #[serde(rename = "message")]
    Message {
        content: Vec<ResponsesContentPart>,
        #[allow(dead_code)]
        role: Option<String>,
    },
    #[serde(rename = "function_call")]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesContentPart {
    #[serde(rename = "output_text")]
    OutputText { text: String },
}

#[derive(Debug, Deserialize)]
struct ResponsesUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
}

impl From<ResponsesUsage> for UsageStats {
    fn from(u: ResponsesUsage) -> Self {
        UsageStats {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.total_tokens,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }
    }
}

// --- Conversion helpers ---

fn convert_tools(tools: &[ToolDefinition]) -> Vec<ResponsesTool> {
    tools
        .iter()
        .map(|t| ResponsesTool {
            tool_type: "function".to_string(),
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        })
        .collect()
}

/// Convert ChatMessage array to Responses API input items.
fn convert_input(messages: &[ChatMessage]) -> serde_json::Value {
    let mut items: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match &msg.content {
            MessageContent::Text(text) => {
                let role = match msg.role {
                    ChatRole::User => "user",
                    ChatRole::Assistant => "assistant",
                };
                items.push(serde_json::json!({
                    "role": role,
                    "content": text,
                }));
            }
            MessageContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": serde_json::to_string(input).unwrap_or_default(),
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            ..
                        } => {
                            tool_results.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": content,
                            }));
                        }
                    }
                }

                // Add text message if present
                if !text_parts.is_empty() {
                    let role = match msg.role {
                        ChatRole::User => "user",
                        ChatRole::Assistant => "assistant",
                    };
                    items.push(serde_json::json!({
                        "role": role,
                        "content": text_parts.join(""),
                    }));
                }

                // Add function calls as separate items
                items.extend(tool_calls);

                // Add function outputs as separate items
                items.extend(tool_results);
            }
        }
    }

    serde_json::Value::Array(items)
}

// --- Streaming SSE parsing ---

/// Streaming state for the Responses API.
struct StreamState {
    /// Current tool call being accumulated (call_id, name, arguments_buffer)
    current_tool: Option<(String, String, String)>,
    event_queue: VecDeque<StreamEvent>,
}

impl StreamState {
    fn new() -> Self {
        Self {
            current_tool: None,
            event_queue: VecDeque::new(),
        }
    }

    /// Process an SSE event (event_type + data JSON) and queue stream events.
    fn process_sse(&mut self, event_type: &str, data: &str) {
        let parsed: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return,
        };

        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                    if !delta.is_empty() {
                        self.event_queue
                            .push_back(StreamEvent::TextDelta(delta.to_string()));
                    }
                }
            }

            "response.output_item.added" => {
                // Check if this is a function_call item
                if let Some(item) = parsed.get("item") {
                    if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                        let call_id = item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        if !name.is_empty() {
                            self.event_queue.push_back(StreamEvent::ToolUseStart {
                                id: call_id.clone(),
                                name: name.clone(),
                            });
                        }
                        self.current_tool = Some((call_id, name, String::new()));
                    }
                }
            }

            "response.function_call_arguments.delta" => {
                if let Some(delta) = parsed.get("delta").and_then(|d| d.as_str()) {
                    if !delta.is_empty() {
                        if let Some((_, _, ref mut buf)) = self.current_tool {
                            buf.push_str(delta);
                        }
                        self.event_queue
                            .push_back(StreamEvent::ToolUseInputDelta(delta.to_string()));
                    }
                }
            }

            "response.function_call_arguments.done" => {
                // Tool call complete — the accumulated args should be valid JSON
                self.current_tool = None;
            }

            "response.completed" => {
                let usage = parsed
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .and_then(|u| serde_json::from_value::<ResponsesUsage>(u.clone()).ok())
                    .map(|u| u.into());

                self.event_queue.push_back(StreamEvent::MessageComplete {
                    usage,
                    stop_reason: Some("end_turn".to_string()),
                });
            }

            "response.failed" => {
                let msg = parsed
                    .get("response")
                    .and_then(|r| r.get("status_details"))
                    .and_then(|s| s.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("Unknown error");
                self.event_queue
                    .push_back(StreamEvent::Error(ProviderError::ServiceError(
                        msg.to_string(),
                    )));
            }

            // Ignore other event types (response.created, response.in_progress, etc.)
            _ => {}
        }
    }
}

// --- Provider implementation ---

#[async_trait]
impl Provider for OpenAIResponsesProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_tools: true,
            supports_streaming: true,
            supports_system_message: true,
            supports_vision: false,
            max_context_tokens: Some(200_000),
            max_output_tokens: Some(32_000),
        }
    }

    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage> {
        let resp = self
            .chat_with_usage(messages, system, tools, max_tokens)
            .await?;
        Ok(resp.message)
    }

    async fn chat_with_usage(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatResponse> {
        let request = ResponsesRequest {
            model: self.model.clone(),
            input: convert_input(&messages),
            instructions: system.map(|s| s.to_string()),
            tools: tools.map(convert_tools),
            max_output_tokens: Some(max_tokens),
            stream: None,
        };

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenAI Responses API")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            anyhow::bail!("OpenAI Responses API error {}: {}", status, &body[..body.len().min(500)]);
        }

        let resp: ResponsesResponse =
            serde_json::from_str(&body).context("Failed to parse OpenAI Responses API response")?;

        let mut blocks = Vec::new();

        for item in &resp.output {
            match item {
                ResponsesOutputItem::Message { content, .. } => {
                    for part in content {
                        match part {
                            ResponsesContentPart::OutputText { text } => {
                                blocks.push(ContentBlock::Text { text: text.clone() });
                            }
                        }
                    }
                }
                ResponsesOutputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    let input: serde_json::Value = serde_json::from_str(arguments)
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                    blocks.push(ContentBlock::ToolUse {
                        id: call_id.clone(),
                        name: name.clone(),
                        input,
                    });
                }
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
            usage: resp.usage.map(|u| u.into()),
            stop_reason: Some("end_turn".to_string()),
            model: Some(self.model.clone()),
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>> {
        let request = ResponsesRequest {
            model: self.model.clone(),
            input: convert_input(&messages),
            instructions: system.map(|s| s.to_string()),
            tools: tools.map(convert_tools),
            max_output_tokens: Some(max_tokens),
            stream: Some(true),
        };

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenAI Responses API")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "OpenAI Responses API error {}: {}",
                status,
                &body[..body.len().min(500)]
            );
        }

        let byte_stream = response.bytes_stream();

        // SSE parser state: (byte_stream, text_buffer, current_event_type, stream_state)
        let event_stream = futures_util::stream::unfold(
            (
                byte_stream,
                String::new(),
                String::new(), // current event type
                StreamState::new(),
            ),
            |(mut stream, mut buffer, mut event_type, mut state)| async move {
                loop {
                    // Drain queued events first
                    if let Some(event) = state.event_queue.pop_front() {
                        return Some((event, (stream, buffer, event_type, state)));
                    }

                    // Parse SSE lines from buffer
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].trim_end_matches('\r').to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            // Empty line = end of SSE event, ignore
                            continue;
                        }

                        if let Some(et) = line.strip_prefix("event: ") {
                            event_type = et.to_string();
                        } else if let Some(data) = line.strip_prefix("data: ") {
                            if !event_type.is_empty() {
                                state.process_sse(&event_type, data);
                                event_type.clear();
                            }
                        }
                    }

                    // If we got events, loop back to drain them
                    if !state.event_queue.is_empty() {
                        continue;
                    }

                    // Need more data
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
                        }
                        Some(Err(e)) => {
                            return Some((
                                StreamEvent::Error(ProviderError::NetworkError(e.to_string())),
                                (stream, buffer, event_type, state),
                            ));
                        }
                        None => {
                            // Stream ended — process any remaining buffer
                            if !buffer.is_empty() {
                                for line in buffer.lines() {
                                    let line = line.trim();
                                    if let Some(et) = line.strip_prefix("event: ") {
                                        event_type = et.to_string();
                                    } else if let Some(data) = line.strip_prefix("data: ") {
                                        if !event_type.is_empty() {
                                            state.process_sse(&event_type, data);
                                            event_type.clear();
                                        }
                                    }
                                }
                                buffer.clear();
                                if !state.event_queue.is_empty() {
                                    continue;
                                }
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
        (text.len() as u64 + 2) / 4
    }
}
