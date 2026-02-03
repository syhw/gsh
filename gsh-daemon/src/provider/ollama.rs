//! Ollama provider for local LLM models
//!
//! Connects to a local Ollama server for privacy-focused, offline LLM usage.

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

const DEFAULT_API_URL: &str = "http://localhost:11434";

/// Ollama provider for local models
pub struct OllamaProvider {
    client: Client,
    model: String,
    base_url: String,
}

impl OllamaProvider {
    pub fn new(model: String, base_url: Option<String>) -> Self {
        Self {
            client: Client::new(),
            model,
            base_url: base_url.unwrap_or_else(|| DEFAULT_API_URL.to_string()),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/api/chat", self.base_url)
    }
}

fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    // Capabilities vary by model - these are reasonable defaults
    let (context_tokens, output_tokens) = if model.contains("llama3") {
        (8_192, 4_096)
    } else if model.contains("mistral") {
        (32_000, 8_192)
    } else if model.contains("codellama") {
        (16_000, 4_096)
    } else if model.contains("deepseek") {
        (32_000, 8_192)
    } else {
        (4_096, 2_048)
    };

    ProviderCapabilities {
        supports_tools: model.contains("llama3") || model.contains("mistral"), // Not all models support tools
        supports_streaming: true,
        supports_system_message: true,
        supports_vision: model.contains("llava") || model.contains("bakllava"),
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

#[derive(Debug, Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OllamaTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<OllamaOptions>,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct OllamaMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaToolCall {
    function: OllamaFunctionCall,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct OllamaFunctionCall {
    name: String,
    arguments: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct OllamaTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OllamaFunction,
}

#[derive(Debug, Serialize)]
struct OllamaFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OllamaResponse {
    message: OllamaMessage,
    done: bool,
    #[serde(default)]
    eval_count: Option<u64>,
    #[serde(default)]
    prompt_eval_count: Option<u64>,
}

fn convert_messages(messages: &[ChatMessage], system: Option<&str>) -> Vec<OllamaMessage> {
    let mut result = Vec::new();

    if let Some(sys) = system {
        result.push(OllamaMessage {
            role: "system".to_string(),
            content: sys.to_string(),
            tool_calls: None,
        });
    }

    for msg in messages {
        let content = msg.content.all_text();
        result.push(OllamaMessage {
            role: match msg.role {
                ChatRole::User => "user",
                ChatRole::Assistant => "assistant",
            }.to_string(),
            content,
            tool_calls: None,
        });

        // Handle tool results as separate messages
        if let MessageContent::Blocks(blocks) = &msg.content {
            for block in blocks {
                if let ContentBlock::ToolResult { tool_use_id: _, content, .. } = block {
                    result.push(OllamaMessage {
                        role: "tool".to_string(),
                        content: content.clone(),
                        tool_calls: None,
                    });
                }
            }
        }
    }

    result
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<OllamaTool> {
    tools.iter().map(|t| OllamaTool {
        tool_type: "function".to_string(),
        function: OllamaFunction {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }).collect()
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
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
        let request = OllamaRequest {
            model: self.model.clone(),
            messages: convert_messages(&messages, system),
            stream: false,
            tools: tools.filter(|t| !t.is_empty()).map(convert_tools),
            options: Some(OllamaOptions {
                num_predict: Some(max_tokens),
                temperature: Some(0.7),
            }),
        };

        let response = self
            .client
            .post(self.chat_url())
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Ollama")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama API error {}: {}", status, body);
        }

        let response: OllamaResponse = response
            .json()
            .await
            .context("Failed to parse Ollama response")?;

        let mut blocks = Vec::new();

        if !response.message.content.is_empty() {
            blocks.push(ContentBlock::Text { text: response.message.content.clone() });
        }

        if let Some(tool_calls) = response.message.tool_calls {
            for (idx, tc) in tool_calls.into_iter().enumerate() {
                blocks.push(ContentBlock::ToolUse {
                    id: format!("tool_{}", idx),
                    name: tc.function.name,
                    input: tc.function.arguments,
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
            } else if blocks.is_empty() {
                MessageContent::Text(String::new())
            } else {
                MessageContent::Blocks(blocks)
            },
        };

        let usage = if response.prompt_eval_count.is_some() || response.eval_count.is_some() {
            Some(UsageStats {
                input_tokens: response.prompt_eval_count.unwrap_or(0),
                output_tokens: response.eval_count.unwrap_or(0),
                total_tokens: response.prompt_eval_count.unwrap_or(0) + response.eval_count.unwrap_or(0),
                cache_read_tokens: None,
                cache_creation_tokens: None,
            })
        } else {
            None
        };

        Ok(ChatResponse {
            message,
            usage,
            stop_reason: if response.done { Some("stop".to_string()) } else { None },
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
        let request = OllamaRequest {
            model: self.model.clone(),
            messages: convert_messages(&messages, system),
            stream: true,
            tools: tools.filter(|t| !t.is_empty()).map(convert_tools),
            options: Some(OllamaOptions {
                num_predict: Some(max_tokens),
                temperature: Some(0.7),
            }),
        };

        let response = self
            .client
            .post(self.chat_url())
            .json(&request)
            .send()
            .await
            .context("Failed to send request to Ollama")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Ollama API error {}: {}", status, body);
        }

        let stream = response.bytes_stream();

        let event_stream = futures_util::stream::unfold(
            (stream, String::new()),
            |(mut stream, mut buffer)| async move {
                loop {
                    // Ollama sends newline-delimited JSON
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if let Ok(resp) = serde_json::from_str::<OllamaResponse>(&line) {
                            if resp.done {
                                let usage = if resp.prompt_eval_count.is_some() || resp.eval_count.is_some() {
                                    Some(UsageStats {
                                        input_tokens: resp.prompt_eval_count.unwrap_or(0),
                                        output_tokens: resp.eval_count.unwrap_or(0),
                                        total_tokens: resp.prompt_eval_count.unwrap_or(0) + resp.eval_count.unwrap_or(0),
                                        cache_read_tokens: None,
                                        cache_creation_tokens: None,
                                    })
                                } else {
                                    None
                                };

                                return Some((
                                    StreamEvent::MessageComplete {
                                        usage,
                                        stop_reason: Some("stop".to_string()),
                                    },
                                    (stream, buffer),
                                ));
                            }

                            if !resp.message.content.is_empty() {
                                return Some((
                                    StreamEvent::TextDelta(resp.message.content),
                                    (stream, buffer),
                                ));
                            }

                            // Handle tool calls in streaming
                            if let Some(tool_calls) = resp.message.tool_calls {
                                for (idx, tc) in tool_calls.into_iter().enumerate() {
                                    return Some((
                                        StreamEvent::ToolUseStart {
                                            id: format!("tool_{}", idx),
                                            name: tc.function.name,
                                        },
                                        (stream, buffer),
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
                                (stream, buffer),
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
        // Most local models use similar tokenization to Llama
        // Roughly 4 chars per token for English
        (text.len() as u64 + 3) / 4
    }
}
