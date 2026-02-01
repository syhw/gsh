use super::{
    ChatMessage, ChatRole, ContentBlock, MessageContent, Provider, StreamEvent, ToolDefinition,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use futures_util::StreamExt;

const API_URL: &str = "https://api.anthropic.com/v1/messages";

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    model: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
        }
    }
}

// Request/Response types for Anthropic API
#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, #[serde(skip_serializing_if = "Option::is_none")] is_error: Option<bool> },
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    index: usize,
    #[serde(default)]
    content_block: Option<AnthropicContentBlock>,
    #[serde(default)]
    delta: Option<AnthropicDelta>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    text: Option<String>,
    partial_json: Option<String>,
}

impl From<&ChatMessage> for AnthropicMessage {
    fn from(msg: &ChatMessage) -> Self {
        let role = match msg.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        };
        let content = match &msg.content {
            MessageContent::Text(s) => AnthropicContent::Text(s.clone()),
            MessageContent::Blocks(blocks) => {
                AnthropicContent::Blocks(blocks.iter().map(|b| b.into()).collect())
            }
        };
        AnthropicMessage {
            role: role.to_string(),
            content,
        }
    }
}

impl From<&ContentBlock> for AnthropicContentBlock {
    fn from(block: &ContentBlock) -> Self {
        match block {
            ContentBlock::Text { text } => AnthropicContentBlock::Text { text: text.clone() },
            ContentBlock::ToolUse { id, name, input } => AnthropicContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
            },
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                AnthropicContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: *is_error,
                }
            }
        }
    }
}

impl From<AnthropicContentBlock> for ContentBlock {
    fn from(block: AnthropicContentBlock) -> Self {
        match block {
            AnthropicContentBlock::Text { text } => ContentBlock::Text { text },
            AnthropicContentBlock::ToolUse { id, name, input } => {
                ContentBlock::ToolUse { id, name, input }
            }
            AnthropicContentBlock::ToolResult { tool_use_id, content, is_error } => {
                ContentBlock::ToolResult { tool_use_id, content, is_error }
            }
        }
    }
}

impl From<&ToolDefinition> for AnthropicTool {
    fn from(tool: &ToolDefinition) -> Self {
        AnthropicTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage> {
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            messages: messages.iter().map(|m| m.into()).collect(),
            system: system.map(|s| s.to_string()),
            tools: tools.map(|t| t.iter().map(|tool| tool.into()).collect()),
            stream: None,
        };

        let response = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error {}: {}", status, body);
        }

        let response: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        let content_blocks: Vec<ContentBlock> = response.content.into_iter().map(|b| b.into()).collect();

        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Blocks(content_blocks),
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>> {
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens,
            messages: messages.iter().map(|m| m.into()).collect(),
            system: system.map(|s| s.to_string()),
            tools: tools.map(|t| t.iter().map(|tool| tool.into()).collect()),
            stream: Some(true),
        };

        let response = self
            .client
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Anthropic")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error {}: {}", status, body);
        }

        let stream = response.bytes_stream();

        // Process SSE stream
        let event_stream = futures_util::stream::unfold(
            (stream, String::new()),
            |(mut stream, mut buffer)| async move {
                loop {
                    // Check if we have a complete event in the buffer
                    if let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        if let Some(event) = parse_sse_event(&event_str) {
                            return Some((event, (stream, buffer)));
                        }
                        continue;
                    }

                    // Read more data
                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
                        }
                        Some(Err(e)) => {
                            return Some((StreamEvent::Error(e.to_string()), (stream, buffer)));
                        }
                        None => {
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }
}

fn parse_sse_event(event_str: &str) -> Option<StreamEvent> {
    let mut event_type = None;
    let mut data = None;

    for line in event_str.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            event_type = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            data = Some(rest.to_string());
        }
    }

    let data = data?;

    match event_type.as_deref() {
        Some("content_block_start") => {
            if let Ok(event) = serde_json::from_str::<AnthropicStreamEvent>(&data) {
                if let Some(AnthropicContentBlock::ToolUse { id, name, .. }) = event.content_block {
                    return Some(StreamEvent::ToolUseStart { id, name });
                }
            }
            None
        }
        Some("content_block_delta") => {
            if let Ok(event) = serde_json::from_str::<AnthropicStreamEvent>(&data) {
                if let Some(delta) = event.delta {
                    if let Some(text) = delta.text {
                        return Some(StreamEvent::TextDelta(text));
                    }
                    if let Some(json) = delta.partial_json {
                        return Some(StreamEvent::ToolUseInputDelta(json));
                    }
                }
            }
            None
        }
        Some("message_stop") => Some(StreamEvent::MessageComplete),
        Some("error") => {
            Some(StreamEvent::Error(data))
        }
        _ => None,
    }
}
