//! OpenAI Responses API provider (for gpt-5.x-codex and newer models)
//!
//! Uses rig's default OpenAI client which targets the Responses API.

use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// OpenAI Responses API provider (backed by rig-core).
pub struct OpenAIResponsesProvider {
    model: rig::providers::openai::responses_api::ResponsesCompletionModel,
    model_name: String,
}

impl OpenAIResponsesProvider {
    pub fn new(api_key: String, model: String, _base_url: Option<String>) -> Self {
        // rig's default OpenAI Client uses the Responses API
        let client = rig::providers::openai::Client::new(&api_key)
            .expect("Failed to create OpenAI Responses client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for OpenAIResponsesProvider {
    fn name(&self) -> &str {
        "openai"
    }

    fn model(&self) -> &str {
        &self.model_name
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
        let resp = self.chat_with_usage(messages, system, tools, max_tokens).await?;
        Ok(resp.message)
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
        (text.len() as u64 + 2) / 4
    }
}
