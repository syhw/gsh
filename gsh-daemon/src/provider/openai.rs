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
use tracing::{debug, warn};

const DEFAULT_API_URL: &str = "https://api.openai.com/v1/chat/completions";

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

#[derive(Debug, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
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
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamResponse {
    choices: Vec<OpenAIStreamChoice>,
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

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage> {
        let request = OpenAIRequest {
            model: self.model.clone(),
            messages: convert_messages_to_openai(&messages, system),
            max_tokens,
            tools: tools.map(convert_tools_to_openai),
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
            .context("Failed to send request to OpenAI")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
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

        Ok(ChatMessage {
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

        // Track tool call state across chunks
        let event_stream = futures_util::stream::unfold(
            (stream, String::new(), Vec::<(String, String, String)>::new()),
            |(mut stream, mut buffer, mut tool_calls)| async move {
                loop {
                    // Check for complete SSE events
                    while let Some(pos) = buffer.find("\n\n") {
                        let event_str = buffer[..pos].to_string();
                        buffer = buffer[pos + 2..].to_string();

                        for line in event_str.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    return Some((StreamEvent::MessageComplete, (stream, buffer, tool_calls)));
                                }

                                if let Ok(resp) = serde_json::from_str::<OpenAIStreamResponse>(data) {
                                    if let Some(choice) = resp.choices.first() {
                                        // Handle text content
                                        if let Some(content) = &choice.delta.content {
                                            if !content.is_empty() {
                                                return Some((StreamEvent::TextDelta(content.clone()), (stream, buffer, tool_calls)));
                                            }
                                        }

                                        // Handle tool calls
                                        if let Some(tcs) = &choice.delta.tool_calls {
                                            for tc in tcs {
                                                let idx = tc.index;

                                                // Ensure we have space for this tool call
                                                while tool_calls.len() <= idx {
                                                    tool_calls.push((String::new(), String::new(), String::new()));
                                                }

                                                if let Some(id) = &tc.id {
                                                    tool_calls[idx].0 = id.clone();
                                                }
                                                if let Some(func) = &tc.function {
                                                    if let Some(name) = &func.name {
                                                        tool_calls[idx].1 = name.clone();
                                                        return Some((StreamEvent::ToolUseStart {
                                                            id: tool_calls[idx].0.clone(),
                                                            name: name.clone(),
                                                        }, (stream, buffer, tool_calls)));
                                                    }
                                                    if let Some(args) = &func.arguments {
                                                        tool_calls[idx].2.push_str(args);
                                                        return Some((StreamEvent::ToolUseInputDelta(args.clone()), (stream, buffer, tool_calls)));
                                                    }
                                                }
                                            }
                                        }

                                        if choice.finish_reason.is_some() {
                                            return Some((StreamEvent::MessageComplete, (stream, buffer, tool_calls)));
                                        }
                                    }
                                }
                            }
                        }
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
                            return Some((StreamEvent::Error(e.to_string()), (stream, buffer, tool_calls)));
                        }
                        None => {
                            debug!("Stream ended, buffer remaining: {}", buffer);
                            // If there's remaining content in buffer, it might be a non-streaming response
                            if !buffer.is_empty() {
                                // Try to parse as a complete JSON response (non-streaming)
                                if let Ok(resp) = serde_json::from_str::<OpenAIStreamResponse>(&buffer) {
                                    if let Some(choice) = resp.choices.first() {
                                        if let Some(content) = &choice.delta.content {
                                            if !content.is_empty() {
                                                buffer.clear();
                                                return Some((StreamEvent::TextDelta(content.clone()), (stream, buffer, tool_calls)));
                                            }
                                        }
                                    }
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
}
