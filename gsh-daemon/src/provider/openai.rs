use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// OpenAI Chat Completions API provider (backed by rig-core).
///
/// Also used for OpenAI-compatible APIs (Mistral, Cerebras) via base_url override.
pub struct OpenAIProvider {
    model: rig::providers::openai::CompletionModel,
    model_name: String,
}

/// Get model capabilities for OpenAI models
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    let (context_tokens, output_tokens, supports_vision) = if model.contains("gpt-4o") {
        (128_000, 16_384, true)
    } else if model.contains("gpt-4-turbo") {
        (128_000, 4_096, true)
    } else if model.contains("gpt-4") {
        (8_192, 4_096, false)
    } else if model.contains("gpt-3.5") {
        (16_385, 4_096, false)
    } else if model.contains("o1") || model.contains("o3") {
        (200_000, 100_000, true)
    } else {
        (8_192, 4_096, false)
    };

    ProviderCapabilities {
        supports_tools: !model.contains("o1"),
        supports_streaming: true,
        supports_system_message: !model.contains("o1"),
        supports_vision,
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

impl OpenAIProvider {
    pub fn new(api_key: String, model: String, base_url: Option<String>) -> Self {
        let client = if let Some(url) = base_url {
            // Strip /chat/completions suffix if present (rig appends its own paths)
            let base = url
                .trim_end_matches("/chat/completions")
                .trim_end_matches('/');
            rig::providers::openai::CompletionsClient::builder()
                .api_key(&api_key)
                .base_url(base)
                .build()
                .expect("Failed to create OpenAI-compatible client")
        } else {
            rig::providers::openai::CompletionsClient::new(&api_key)
                .expect("Failed to create OpenAI client")
        };
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &str {
        "openai"
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
        (text.len() as u64 + 3) / 4
    }
}
