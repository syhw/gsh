//! Zhipu AI provider (GLM models)
//!
//! Supports GLM-4 and other Zhipu models via OpenAI-compatible API.

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

const DEFAULT_API_URL: &str = "https://api.z.ai/api/coding/paas/v4/chat/completions";

/// Zhipu AI provider (GLM)
pub struct ZhipuProvider {
    client: Client,
    api_key: String,
    model: String,
    base_url: String,
}

impl ZhipuProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            api_key,
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        }
    }
}

#[allow(dead_code)]
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    // GLM model capabilities (case-insensitive check)
    let model_lower = model.to_lowercase();
    let (context_tokens, output_tokens, supports_vision) = if model_lower.contains("glm-4") {
        (128_000, 8_192, model_lower.contains("v") || model_lower.contains("vision"))
    } else if model_lower.contains("glm-3") {
        (8_192, 4_096, false)
    } else {
        (8_192, 4_096, false)
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

// Zhipu uses OpenAI-compatible API format
#[derive(Debug, Serialize)]
struct ZhipuRequest {
    model: String,
    messages: Vec<ZhipuMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ZhipuTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ZhipuMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ZhipuToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ZhipuToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: ZhipuFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ZhipuFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct ZhipuTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: ZhipuFunction,
}

#[derive(Debug, Serialize)]
struct ZhipuFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ZhipuResponse {
    choices: Vec<ZhipuChoice>,
    model: Option<String>,
    usage: Option<ZhipuUsage>,
}

#[derive(Debug, Deserialize)]
struct ZhipuErrorResponse {
    error: ZhipuError,
}

#[derive(Debug, Deserialize)]
struct ZhipuError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct ZhipuUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<ZhipuUsage> for UsageStats {
    fn from(usage: ZhipuUsage) -> Self {
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
struct ZhipuChoice {
    message: ZhipuMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZhipuStreamResponse {
    choices: Vec<ZhipuStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct ZhipuStreamChoice {
    delta: ZhipuDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ZhipuDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ZhipuStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ZhipuStreamToolCall {
    index: usize,
    id: Option<String>,
    function: Option<ZhipuStreamFunction>,
}

#[derive(Debug, Deserialize)]
struct ZhipuStreamFunction {
    name: Option<String>,
    arguments: Option<String>,
}

fn convert_messages(messages: &[ChatMessage], system: Option<&str>) -> Vec<ZhipuMessage> {
    let mut result = Vec::new();

    if let Some(sys) = system {
        result.push(ZhipuMessage {
            role: "system".to_string(),
            content: Some(sys.to_string()),
            tool_calls: None,
            tool_call_id: None,
        });
    }

    for msg in messages {
        match &msg.content {
            MessageContent::Text(text) => {
                result.push(ZhipuMessage {
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
                let mut text_content = String::new();
                let mut tool_calls = Vec::new();
                let mut tool_results = Vec::new();

                for block in blocks {
                    match block {
                        ContentBlock::Text { text } => {
                            text_content.push_str(text);
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(ZhipuToolCall {
                                id: id.clone(),
                                call_type: "function".to_string(),
                                function: ZhipuFunctionCall {
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
                    result.push(ZhipuMessage {
                        role: match msg.role {
                            ChatRole::User => "user",
                            ChatRole::Assistant => "assistant",
                        }.to_string(),
                        content: if text_content.is_empty() { None } else { Some(text_content) },
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                        tool_call_id: None,
                    });
                }

                for (tool_id, content) in tool_results {
                    result.push(ZhipuMessage {
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

fn convert_tools(tools: &[ToolDefinition]) -> Vec<ZhipuTool> {
    tools.iter().map(|t| ZhipuTool {
        tool_type: "function".to_string(),
        function: ZhipuFunction {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }).collect()
}

#[async_trait]
impl Provider for ZhipuProvider {
    fn name(&self) -> &str {
        "z"
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
        let request = ZhipuRequest {
            model: self.model.clone(),
            messages: convert_messages(&messages, system),
            max_tokens: Some(max_tokens),
            tools: tools.map(convert_tools),
            stream: None,
            temperature: Some(0.7),
        };

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Zhipu")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            anyhow::bail!("Z API error {}: {}", status, body);
        }

        // Check for error response in body (some APIs return 200 with error JSON)
        if let Ok(error_resp) = serde_json::from_str::<ZhipuErrorResponse>(&body) {
            anyhow::bail!("Z API error {}: {}", error_resp.error.code, error_resp.error.message);
        }

        let response: ZhipuResponse = serde_json::from_str(&body)
            .context("Failed to parse Z response")?;

        let choice = response.choices.into_iter().next()
            .ok_or_else(|| anyhow::anyhow!("No response from Zhipu"))?;

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
        let request = ZhipuRequest {
            model: self.model.clone(),
            messages: convert_messages(&messages, system),
            max_tokens: Some(max_tokens),
            tools: tools.map(convert_tools),
            stream: Some(true),
            temperature: Some(0.7),
        };

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Zhipu")?;

        // For streaming, we need to check if the first chunk is an error
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Z API error {}: {}", status, body);
        }

        // Read first chunk to check for error response
        let mut stream = response.bytes_stream();
        let mut initial_buffer = String::new();

        // Peek at the response to detect errors
        if let Some(Ok(chunk)) = stream.next().await {
            initial_buffer = String::from_utf8_lossy(&chunk).to_string();

            // Check if this looks like an error response
            if initial_buffer.starts_with("{\"error\"") {
                if let Ok(error_resp) = serde_json::from_str::<ZhipuErrorResponse>(&initial_buffer) {
                    anyhow::bail!("Z API error {}: {}", error_resp.error.code, error_resp.error.message);
                }
            }
        }

        let event_stream = futures_util::stream::unfold(
            (stream, initial_buffer, Vec::<(String, String, String)>::new()),
            |(mut stream, mut buffer, mut tool_calls)| async move {
                loop {
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        for line in event_str.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    return Some((
                                        StreamEvent::MessageComplete {
                                            usage: None,
                                            stop_reason: None,
                                        },
                                        (stream, buffer, tool_calls),
                                    ));
                                }

                                if let Ok(resp) = serde_json::from_str::<ZhipuStreamResponse>(data) {
                                    if let Some(choice) = resp.choices.first() {
                                        if let Some(content) = &choice.delta.content {
                                            if !content.is_empty() {
                                                return Some((
                                                    StreamEvent::TextDelta(content.clone()),
                                                    (stream, buffer, tool_calls),
                                                ));
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
                                                        return Some((
                                                            StreamEvent::ToolUseStart {
                                                                id: tool_calls[idx].0.clone(),
                                                                name: name.clone(),
                                                            },
                                                            (stream, buffer, tool_calls),
                                                        ));
                                                    }
                                                    if let Some(args) = &func.arguments {
                                                        tool_calls[idx].2.push_str(args);
                                                        return Some((
                                                            StreamEvent::ToolUseInputDelta(args.clone()),
                                                            (stream, buffer, tool_calls),
                                                        ));
                                                    }
                                                }
                                            }
                                        }

                                        if choice.finish_reason.is_some() {
                                            return Some((
                                                StreamEvent::MessageComplete {
                                                    usage: None,
                                                    stop_reason: choice.finish_reason.clone(),
                                                },
                                                (stream, buffer, tool_calls),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Also try parsing newline-delimited JSON (some APIs use this)
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        // Handle data: prefix if present
                        let json_str = line.strip_prefix("data: ").unwrap_or(&line);

                        if json_str == "[DONE]" {
                            return Some((
                                StreamEvent::MessageComplete {
                                    usage: None,
                                    stop_reason: None,
                                },
                                (stream, buffer, tool_calls),
                            ));
                        }

                        if let Ok(resp) = serde_json::from_str::<ZhipuStreamResponse>(json_str) {
                            if let Some(choice) = resp.choices.first() {
                                if let Some(content) = &choice.delta.content {
                                    if !content.is_empty() {
                                        return Some((
                                            StreamEvent::TextDelta(content.clone()),
                                            (stream, buffer, tool_calls),
                                        ));
                                    }
                                }

                                if choice.finish_reason.is_some() {
                                    return Some((
                                        StreamEvent::MessageComplete {
                                            usage: None,
                                            stop_reason: choice.finish_reason.clone(),
                                        },
                                        (stream, buffer, tool_calls),
                                    ));
                                }
                            }
                        }
                    }

                    match stream.next().await {
                        Some(Ok(chunk)) => {
                            buffer.push_str(&String::from_utf8_lossy(&chunk));
                        }
                        Some(Err(e)) => {
                            return Some((
                                StreamEvent::Error(ProviderError::NetworkError(e.to_string())),
                                (stream, buffer, tool_calls),
                            ));
                        }
                        None => return None,
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }

    fn estimate_tokens(&self, text: &str) -> u64 {
        // Chinese text uses roughly 1.5-2 chars per token
        // English uses roughly 4 chars per token
        // Use a blended estimate
        (text.len() as u64 + 2) / 3
    }
}
