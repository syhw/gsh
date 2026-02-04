use super::{
    ChatMessage, ChatResponse, ChatRole, ContentBlock, MessageContent, Provider,
    ProviderCapabilities, ProviderError, StreamEvent, ToolDefinition, UsageStats,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use futures_util::StreamExt;

const API_URL: &str = "https://api.anthropic.com/v1/messages";

/// Anthropic Claude API provider
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    model: String,
}

/// Model capability information for Anthropic models
#[allow(dead_code)]
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    // Context windows and capabilities for known Anthropic models
    // as of early 2025
    let (context_tokens, output_tokens) = if model.contains("opus") {
        (200_000, 4_096)
    } else if model.contains("sonnet") {
        (200_000, 8_192)
    } else if model.contains("haiku") {
        (200_000, 4_096)
    } else {
        // Default for unknown models
        (100_000, 4_096)
    };

    ProviderCapabilities {
        supports_tools: true,
        supports_streaming: true,
        supports_system_message: true,
        supports_vision: true, // Claude 3+ supports vision
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
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
#[allow(dead_code)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    model: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
}

impl From<AnthropicUsage> for UsageStats {
    fn from(usage: AnthropicUsage) -> Self {
        UsageStats {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.input_tokens + usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_creation_tokens: usage.cache_creation_input_tokens,
        }
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorResponse {
    error: AnthropicError,
}

impl AnthropicErrorResponse {
    fn into_provider_error(self, status_code: u16) -> ProviderError {
        let msg = self.error.message;
        match (status_code, self.error.error_type.as_str()) {
            (401, _) => ProviderError::AuthenticationError(msg),
            (429, _) => ProviderError::RateLimitError {
                message: msg,
                retry_after_secs: None, // Could parse from headers
            },
            (400, "invalid_request_error") => ProviderError::InvalidRequestError(msg),
            (404, _) => ProviderError::ModelNotFoundError(msg),
            (400, _) if msg.contains("context") || msg.contains("token") => {
                ProviderError::ContextLengthExceededError {
                    message: msg,
                    max_tokens: None,
                }
            }
            (500..=599, _) => ProviderError::ServiceError(msg),
            _ => ProviderError::Other(msg),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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

        let status = response.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let body = response.text().await.unwrap_or_default();

            // Try to parse structured error
            if let Ok(error_resp) = serde_json::from_str::<AnthropicErrorResponse>(&body) {
                let provider_error = error_resp.into_provider_error(status_code);
                anyhow::bail!("{}", provider_error);
            }

            anyhow::bail!("Anthropic API error {}: {}", status, body);
        }

        let response: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic response")?;

        let content_blocks: Vec<ContentBlock> = response.content.into_iter().map(|b| b.into()).collect();

        let message = ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Blocks(content_blocks),
        };

        Ok(ChatResponse {
            message,
            usage: response.usage.map(|u| u.into()),
            stop_reason: response.stop_reason,
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
                            return Some((
                                StreamEvent::Error(ProviderError::NetworkError(e.to_string())),
                                (stream, buffer),
                            ));
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

    fn estimate_tokens(&self, text: &str) -> u64 {
        // Claude uses a tokenizer similar to GPT, roughly 4 chars per token
        // This is a rough estimate; for accurate counts, use the Anthropic tokenizer
        (text.len() as u64 + 3) / 4
    }
}

/// Parse an SSE event string into a StreamEvent
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
        Some("message_stop") => Some(StreamEvent::MessageComplete {
            usage: None,
            stop_reason: None,
        }),
        Some("message_delta") => {
            // Parse final usage and stop_reason from message_delta
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(&data) {
                let stop_reason = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string());

                let usage = event.get("usage").and_then(|u| {
                    let input = u.get("input_tokens")?.as_u64()?;
                    let output = u.get("output_tokens")?.as_u64()?;
                    Some(UsageStats::new(input, output))
                });

                if stop_reason.is_some() || usage.is_some() {
                    return Some(StreamEvent::MessageComplete { usage, stop_reason });
                }
            }
            None
        }
        Some("error") => {
            // Try to parse structured error
            if let Ok(error_resp) = serde_json::from_str::<AnthropicErrorResponse>(&data) {
                return Some(StreamEvent::Error(
                    error_resp.into_provider_error(400), // Default to 400 for stream errors
                ));
            }
            Some(StreamEvent::Error(ProviderError::Other(data)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // Message Conversion Tests
    // =========================================================================
    // These tests verify that our internal message types correctly convert to
    // and from Anthropic's API format. This is critical because:
    // 1. Incorrect conversion = API errors or lost data
    // 2. The conversion logic handles multiple message formats (text vs blocks)
    // 3. Tool use/results must preserve all fields exactly

    #[test]
    fn test_user_message_text_conversion() {
        // Simple text messages should convert to Anthropic's format
        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("Hello, Claude!".to_string()),
        };

        let anthropic_msg: AnthropicMessage = (&msg).into();

        assert_eq!(anthropic_msg.role, "user");
        match anthropic_msg.content {
            AnthropicContent::Text(text) => assert_eq!(text, "Hello, Claude!"),
            _ => panic!("Expected text content"),
        }
    }

    #[test]
    fn test_assistant_message_conversion() {
        // Assistant messages use "assistant" role in Anthropic API
        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Text("I'm here to help.".to_string()),
        };

        let anthropic_msg: AnthropicMessage = (&msg).into();

        assert_eq!(anthropic_msg.role, "assistant");
    }

    #[test]
    fn test_tool_use_block_conversion() {
        // Tool use blocks must preserve id, name, and input exactly
        // The LLM generates these, and we send them back as tool_result
        let block = ContentBlock::ToolUse {
            id: "toolu_123".to_string(),
            name: "bash".to_string(),
            input: serde_json::json!({"command": "ls -la"}),
        };

        let anthropic_block: AnthropicContentBlock = (&block).into();

        match anthropic_block {
            AnthropicContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "bash");
                assert_eq!(input["command"], "ls -la");
            }
            _ => panic!("Expected ToolUse block"),
        }
    }

    #[test]
    fn test_tool_result_block_conversion() {
        // Tool results link back to the original tool_use via tool_use_id
        // is_error flag tells the LLM if the tool execution failed
        let block = ContentBlock::ToolResult {
            tool_use_id: "toolu_123".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
            is_error: Some(false),
        };

        let anthropic_block: AnthropicContentBlock = (&block).into();

        match anthropic_block {
            AnthropicContentBlock::ToolResult { tool_use_id, content, is_error } => {
                assert_eq!(tool_use_id, "toolu_123");
                assert_eq!(content, "file1.txt\nfile2.txt");
                assert_eq!(is_error, Some(false));
            }
            _ => panic!("Expected ToolResult block"),
        }
    }

    #[test]
    fn test_tool_result_error_flag() {
        // When a tool fails, is_error=true helps the LLM understand and recover
        let block = ContentBlock::ToolResult {
            tool_use_id: "toolu_456".to_string(),
            content: "Error: file not found".to_string(),
            is_error: Some(true),
        };

        let anthropic_block: AnthropicContentBlock = (&block).into();

        match anthropic_block {
            AnthropicContentBlock::ToolResult { is_error, .. } => {
                assert_eq!(is_error, Some(true));
            }
            _ => panic!("Expected ToolResult block"),
        }
    }

    #[test]
    fn test_message_with_multiple_blocks() {
        // Messages can contain multiple content blocks (text + tool use)
        // This happens when the LLM explains what it's doing AND calls a tool
        let blocks = vec![
            ContentBlock::Text { text: "Let me check that for you.".to_string() },
            ContentBlock::ToolUse {
                id: "toolu_789".to_string(),
                name: "read_file".to_string(),
                input: serde_json::json!({"path": "/etc/hosts"}),
            },
        ];

        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Blocks(blocks),
        };

        let anthropic_msg: AnthropicMessage = (&msg).into();

        match anthropic_msg.content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(blocks.len(), 2);
                assert!(matches!(&blocks[0], AnthropicContentBlock::Text { .. }));
                assert!(matches!(&blocks[1], AnthropicContentBlock::ToolUse { .. }));
            }
            _ => panic!("Expected blocks content"),
        }
    }

    // =========================================================================
    // Tool Definition Conversion Tests
    // =========================================================================
    // Tools define what capabilities the LLM can use. The schema must be
    // valid JSON Schema for the API to accept it.

    #[test]
    fn test_tool_definition_conversion() {
        let tool = ToolDefinition {
            name: "execute_command".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to execute"
                    }
                },
                "required": ["command"]
            }),
        };

        let anthropic_tool: AnthropicTool = (&tool).into();

        assert_eq!(anthropic_tool.name, "execute_command");
        assert_eq!(anthropic_tool.description, "Run a shell command");
        assert!(anthropic_tool.input_schema["properties"]["command"].is_object());
    }

    // =========================================================================
    // Round-trip Conversion Tests
    // =========================================================================
    // These test that data survives conversion to Anthropic format and back.
    // Important for when we receive responses and convert them to our format.

    #[test]
    fn test_content_block_roundtrip() {
        // Text block should survive round-trip unchanged
        let original = ContentBlock::Text { text: "Hello world".to_string() };
        let anthropic: AnthropicContentBlock = (&original).into();
        let back: ContentBlock = anthropic.into();

        match back {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Round-trip failed"),
        }
    }

    #[test]
    fn test_tool_use_roundtrip() {
        // Tool use with complex JSON input should survive round-trip
        let original = ContentBlock::ToolUse {
            id: "toolu_abc".to_string(),
            name: "search".to_string(),
            input: serde_json::json!({
                "query": "rust async",
                "limit": 10,
                "filters": ["docs", "examples"]
            }),
        };

        let anthropic: AnthropicContentBlock = (&original).into();
        let back: ContentBlock = anthropic.into();

        match back {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_abc");
                assert_eq!(name, "search");
                assert_eq!(input["query"], "rust async");
                assert_eq!(input["limit"], 10);
            }
            _ => panic!("Round-trip failed"),
        }
    }

    // =========================================================================
    // Usage Statistics Tests
    // =========================================================================

    #[test]
    fn test_usage_stats_conversion() {
        // Anthropic returns usage with cache tokens; we convert to our format
        let anthropic_usage = AnthropicUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(20),
            cache_read_input_tokens: Some(80),
        };

        let usage: UsageStats = anthropic_usage.into();

        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
        assert_eq!(usage.cache_creation_tokens, Some(20));
        assert_eq!(usage.cache_read_tokens, Some(80));
    }

    // =========================================================================
    // Provider Instance Tests
    // =========================================================================

    #[test]
    fn test_provider_name_and_model() {
        let provider = AnthropicProvider::new(
            "test-api-key".to_string(),
            "claude-sonnet-4-20250514".to_string(),
        );

        assert_eq!(provider.name(), "anthropic");
        assert_eq!(provider.model(), "claude-sonnet-4-20250514");
    }
}
