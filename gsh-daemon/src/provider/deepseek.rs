//! DeepSeek provider (v3 + R1 reasoning)
//!
//! Backed by rig-core's deepseek provider.
//!
//! Models:
//! - `deepseek-chat` — DeepSeek-V3 (64K context, tool use)
//! - `deepseek-reasoner` — DeepSeek-R1 (64K context, extended reasoning)

use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

pub const DEEPSEEK_CHAT: &str = "deepseek-chat";
pub const DEEPSEEK_REASONER: &str = "deepseek-reasoner";

pub struct DeepSeekProvider {
    model: rig::providers::deepseek::CompletionModel,
    model_name: String,
}

fn get_model_capabilities(_model: &str) -> ProviderCapabilities {
    // R1 (reasoner) supports tool use as of the latest API; context is 64K for both
    ProviderCapabilities {
        supports_tools: true,
        supports_streaming: true,
        supports_system_message: true,
        supports_vision: false,
        max_context_tokens: Some(64_000),
        max_output_tokens: Some(8_192),
    }
}

impl DeepSeekProvider {
    pub fn new(api_key: String, model: String) -> Self {
        let client = rig::providers::deepseek::Client::new(&api_key)
            .expect("Failed to create DeepSeek client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    fn name(&self) -> &str {
        "deepseek"
    }

    fn model(&self) -> &str {
        &self.model_name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        get_model_capabilities(&self.model_name)
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
        rig_adapt::rig_chat_with_usage(&self.model, messages, system, tools, max_tokens).await
    }

    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>> {
        rig_adapt::rig_chat_stream(&self.model, messages, system, tools, max_tokens).await
    }

    fn estimate_tokens(&self, text: &str) -> u64 {
        // DeepSeek uses a similar tokenizer to GPT (~4 chars/token for English)
        (text.len() as u64 + 3) / 4
    }
}
