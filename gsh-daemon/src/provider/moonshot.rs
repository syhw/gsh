//! Moonshot AI provider (Kimi)
//!
//! Backed by rig-core's moonshot provider.

use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Moonshot AI provider (Kimi, backed by rig-core)
pub struct MoonshotProvider {
    model: rig::providers::moonshot::CompletionModel,
    model_name: String,
}

fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    let (context_tokens, output_tokens, supports_vision) = if model.contains("k2") {
        (200_000, 32_768, true)
    } else if model.contains("moonshot-v1-128k") {
        (128_000, 8_192, false)
    } else if model.contains("moonshot-v1-32k") {
        (32_000, 8_192, false)
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

impl MoonshotProvider {
    pub fn new(api_key: String, model: String, _base_url: Option<String>) -> Self {
        let client = rig::providers::moonshot::Client::new(&api_key)
            .expect("Failed to create Moonshot client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for MoonshotProvider {
    fn name(&self) -> &str {
        "moonshot"
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
        (text.len() as u64 + 2) / 3
    }
}
