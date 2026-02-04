use super::{
    ChatMessage, ChatRole, ContentBlock, MessageContent, Provider,
    ProviderCapabilities, StreamEvent, ToolDefinition, UsageStats,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use futures_util::StreamExt;
use tracing::debug;

const DEFAULT_API_URL: &str = "https://api.together.xyz/v1/chat/completions";

/// Together.AI provider
///
/// Supports models like:
/// - moonshotai/Kimi-K2.5 (131K context)
/// - Qwen/Qwen3-Coder-Next-FP8 (large context coding model)
///
/// Uses OpenAI-compatible API format.
pub struct TogetherProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl TogetherProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        }
    }
}

/// Get model capabilities for Together.AI models
#[allow(dead_code)]
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    let model_lower = model.to_lowercase();

    let (context_tokens, output_tokens, supports_vision) = if model_lower.contains("kimi-k2") {
        // Kimi K2.5 - large context model
        (131_072, 8_192, false)
    } else if model_lower.contains("qwen3-coder") {
        // Qwen3 Coder - coding focused model
        (131_072, 8_192, false)
    } else if model_lower.contains("llama") {
        (128_000, 4_096, false)
    } else if model_lower.contains("deepseek") {
        (64_000, 8_192, false)
    } else {
        // Default for unknown models
        (32_000, 4_096, false)
    };

    ProviderCapabilities {
        supports_tools: true,
        supports_streaming: true,
        supports_system_message: true,
        supports_vision,
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

// OpenAI-compatible request/response types
#[derive(Debug, Serialize)]
struct TogetherRequest {
    model: String,
    messages: Vec<TogetherMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<TogetherTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TogetherMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<TogetherToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TogetherToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: TogetherFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct TogetherFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct TogetherTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: TogetherFunction,
}

#[derive(Debug, Serialize)]
struct TogetherFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TogetherResponse {
    choices: Vec<TogetherChoice>,
    model: Option<String>,
    usage: Option<TogetherUsage>,
}

#[derive(Debug, Deserialize)]
struct TogetherUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<TogetherUsage> for UsageStats {
    fn from(usage: TogetherUsage) -> Self {
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
struct TogetherChoice {
    message: TogetherMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TogetherErrorResponse {
    error: TogetherError,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TogetherError {
    message: String,
    #[serde(rename = "type")]
    error_type: Option<String>,
    code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TogetherStreamResponse {
    choices: Vec<TogetherStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct TogetherStreamChoice {
    delta: TogetherDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct TogetherDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<TogetherToolCallDelta>>,
}

#[derive(Debug, Deserialize)]
struct TogetherToolCallDelta {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<TogetherFunctionDelta>,
}

#[derive(Debug, Deserialize)]
struct TogetherFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

impl From<&ChatMessage> for TogetherMessage {
    fn from(msg: &ChatMessage) -> Self {
        let role = match msg.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        };

        match &msg.content {
            MessageContent::Text(text) => TogetherMessage {
                role: role.to_string(),
                content: Some(text.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
            MessageContent::Blocks(blocks) => {
                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();
                let mut tool_call_id = None;

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(TogetherToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: TogetherFunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { tool_use_id, content, .. } => {
                            tool_call_id = Some(tool_use_id.clone());
                            text_parts.push(content.clone());
                        }
                    }
                }

                TogetherMessage {
                    role: if tool_call_id.is_some() { "tool".to_string() } else { role.to_string() },
                    content: if text_parts.is_empty() { None } else { Some(text_parts.join("\n")) },
                    tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    tool_call_id,
                }
            }
        }
    }
}

impl From<&ToolDefinition> for TogetherTool {
    fn from(tool: &ToolDefinition) -> Self {
        TogetherTool {
            tool_type: "function".to_string(),
            function: TogetherFunction {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.input_schema.clone(),
            },
        }
    }
}

#[async_trait]
impl Provider for TogetherProvider {
    fn name(&self) -> &str {
        "together"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage> {
        let mut together_messages: Vec<TogetherMessage> = Vec::new();

        // Add system message if provided
        if let Some(sys) = system {
            together_messages.push(TogetherMessage {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        // Convert messages
        for msg in &messages {
            together_messages.push(msg.into());
        }

        let together_tools = tools.map(|t| t.iter().map(|tool| tool.into()).collect());

        let request = TogetherRequest {
            model: self.model.clone(),
            messages: together_messages,
            max_tokens,
            tools: together_tools,
            stream: Some(false),
        };

        debug!("Sending request to Together.AI: {:?}", request.model);

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Together.AI")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Together.AI API error {}: {}", status, error_text);
        }

        let together_response: TogetherResponse = response
            .json()
            .await
            .context("Failed to parse Together.AI response")?;

        let choice = together_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from Together.AI"))?;

        // Convert response to ChatMessage
        let content = if let Some(tool_calls) = choice.message.tool_calls {
            let mut blocks: Vec<ContentBlock> = Vec::new();

            if let Some(text) = choice.message.content {
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text { text });
                }
            }

            for tc in tool_calls {
                let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                });
            }

            MessageContent::Blocks(blocks)
        } else {
            MessageContent::Text(choice.message.content.unwrap_or_default())
        };

        Ok(ChatMessage {
            role: ChatRole::Assistant,
            content,
        })
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>> {
        let mut together_messages: Vec<TogetherMessage> = Vec::new();

        if let Some(sys) = system {
            together_messages.push(TogetherMessage {
                role: "system".to_string(),
                content: Some(sys.to_string()),
                tool_calls: None,
                tool_call_id: None,
            });
        }

        for msg in &messages {
            together_messages.push(msg.into());
        }

        let together_tools = tools.map(|t| t.iter().map(|tool| tool.into()).collect());

        let request = TogetherRequest {
            model: self.model.clone(),
            messages: together_messages,
            max_tokens,
            tools: together_tools,
            stream: Some(true),
        };

        debug!("Sending streaming request to Together.AI: {:?}", request.model);

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Together.AI")?;

        let status = response.status();
        if !status.is_success() {
            let error_text = response.text().await.unwrap_or_default();
            anyhow::bail!("Together.AI API error {}: {}", status, error_text);
        }

        let byte_stream = response.bytes_stream();

        let stream = byte_stream
            .map(|result| {
                result
                    .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                    .unwrap_or_default()
            })
            .flat_map(|chunk| {
                let events: Vec<StreamEvent> = chunk
                    .lines()
                    .filter_map(|line| {
                        let line = line.trim();
                        if line.starts_with("data: ") {
                            let data = &line[6..];
                            if data == "[DONE]" {
                                return Some(StreamEvent::MessageComplete {
                                    usage: None,
                                    stop_reason: None,
                                });
                            }
                            parse_stream_event(data)
                        } else {
                            None
                        }
                    })
                    .collect();
                futures::stream::iter(events)
            });

        Ok(Box::pin(stream))
    }
}

fn parse_stream_event(data: &str) -> Option<StreamEvent> {
    let response: TogetherStreamResponse = serde_json::from_str(data).ok()?;
    let choice = response.choices.into_iter().next()?;

    // Check for text content
    if let Some(text) = choice.delta.content {
        if !text.is_empty() {
            return Some(StreamEvent::TextDelta(text));
        }
    }

    // Check for tool calls
    if let Some(tool_calls) = choice.delta.tool_calls {
        for tc in tool_calls {
            if let Some(id) = tc.id {
                if let Some(ref func) = tc.function {
                    if let Some(ref name) = func.name {
                        return Some(StreamEvent::ToolUseStart { id, name: name.clone() });
                    }
                }
            }
            if let Some(func) = tc.function {
                if let Some(args) = func.arguments {
                    return Some(StreamEvent::ToolUseInputDelta(args));
                }
            }
        }
    }

    // Check for finish reason
    if choice.finish_reason.is_some() {
        return Some(StreamEvent::MessageComplete {
            usage: None,
            stop_reason: choice.finish_reason,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name_and_model() {
        let provider = TogetherProvider::new(
            "test-key".to_string(),
            "moonshotai/Kimi-K2.5".to_string(),
            None,
        );
        assert_eq!(provider.name(), "together");
        assert_eq!(provider.model(), "moonshotai/Kimi-K2.5");
    }

    #[test]
    fn test_user_message_conversion() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("Hello".to_string()),
        };
        let together_msg: TogetherMessage = (&msg).into();
        assert_eq!(together_msg.role, "user");
        assert_eq!(together_msg.content, Some("Hello".to_string()));
    }

    #[test]
    fn test_tool_definition_conversion() {
        let tool = ToolDefinition {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let together_tool: TogetherTool = (&tool).into();
        assert_eq!(together_tool.tool_type, "function");
        assert_eq!(together_tool.function.name, "test_tool");
    }

    #[test]
    fn test_model_capabilities() {
        let kimi_caps = get_model_capabilities("moonshotai/Kimi-K2.5");
        assert_eq!(kimi_caps.max_context_tokens, Some(131_072));
        assert!(kimi_caps.supports_tools);

        let qwen_caps = get_model_capabilities("Qwen/Qwen3-Coder-Next-FP8");
        assert_eq!(qwen_caps.max_context_tokens, Some(131_072));
    }
}
