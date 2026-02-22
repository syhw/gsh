//! Zhipu AI provider (GLM models)
//!
//! Uses rig's OpenAI-compatible client with Z.ai base URL.

use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

const DEFAULT_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

/// Zhipu AI provider (GLM, backed by rig-core OpenAI-compatible client)
pub struct ZhipuProvider {
    model: rig::providers::openai::CompletionModel,
    model_name: String,
}

fn get_model_capabilities(model: &str) -> ProviderCapabilities {
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

impl ZhipuProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        let base = base_url
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        // Strip /chat/completions suffix if present
        let base = base
            .trim_end_matches("/chat/completions")
            .trim_end_matches('/');
        let client = rig::providers::openai::CompletionsClient::builder()
            .api_key(&api_key)
            .base_url(base)
            .build()
            .expect("Failed to create Zhipu client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for ZhipuProvider {
    fn name(&self) -> &str {
        "z"
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
