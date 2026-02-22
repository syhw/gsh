//! Ollama provider for local LLM models
//!
//! Backed by rig-core's ollama provider.

use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Ollama provider for local models (backed by rig-core)
pub struct OllamaProvider {
    model: rig::providers::ollama::CompletionModel,
    model_name: String,
}

fn get_model_capabilities(model: &str) -> ProviderCapabilities {
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
        supports_tools: model.contains("llama3") || model.contains("mistral"),
        supports_streaming: true,
        supports_system_message: true,
        supports_vision: model.contains("llava") || model.contains("bakllava"),
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

impl OllamaProvider {
    pub fn new(model: String, base_url: Option<String>) -> Self {
        let base = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let client = rig::providers::ollama::Client::builder()
            .api_key(rig::client::Nothing)
            .base_url(&base)
            .build()
            .expect("Failed to create Ollama client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    fn name(&self) -> &str {
        "ollama"
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
