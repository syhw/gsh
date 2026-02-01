pub mod anthropic;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use futures::Stream;

/// Tool definition for the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// A message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl MessageContent {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn blocks(blocks: Vec<ContentBlock>) -> Self {
        Self::Blocks(blocks)
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Blocks(blocks) => {
                // Return first text block
                blocks.iter().find_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: Option<bool> },
}

/// Streaming event from the provider
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Text content delta
    TextDelta(String),
    /// Tool use started
    ToolUseStart { id: String, name: String },
    /// Tool use input delta (JSON string chunk)
    ToolUseInputDelta(String),
    /// Message complete
    MessageComplete,
    /// Error occurred
    Error(String),
}

/// Provider trait for LLM backends
#[async_trait]
pub trait Provider: Send + Sync {
    /// Get the provider name
    fn name(&self) -> &str;

    /// Send a chat completion request and get a full response
    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage>;

    /// Send a chat completion request and stream the response
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>>;
}

/// Create a provider from configuration
pub fn create_provider(
    provider_name: &str,
    config: &crate::config::Config,
) -> Result<Box<dyn Provider>> {
    match provider_name {
        "anthropic" => {
            let api_key = config
                .anthropic_api_key()
                .ok_or_else(|| anyhow::anyhow!("Anthropic API key not configured"))?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                api_key,
                config.llm.anthropic.model.clone(),
            )))
        }
        "openai" => {
            let api_key = config
                .openai_api_key()
                .ok_or_else(|| anyhow::anyhow!("OpenAI API key not configured"))?;
            Ok(Box::new(openai::OpenAIProvider::new(
                api_key,
                config.llm.openai.model.clone(),
                config.llm.openai.base_url.clone(),
            )))
        }
        _ => Err(anyhow::anyhow!("Unknown provider: {}", provider_name)),
    }
}
