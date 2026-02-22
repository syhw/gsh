use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Together.AI provider (backed by rig-core)
pub struct TogetherProvider {
    model: rig::providers::together::CompletionModel,
    model_name: String,
}

/// Get model capabilities for Together.AI models
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    let model_lower = model.to_lowercase();

    let (context_tokens, output_tokens, supports_vision) = if model_lower.contains("kimi-k2") {
        (131_072, 8_192, false)
    } else if model_lower.contains("qwen3-coder") {
        (131_072, 8_192, false)
    } else if model_lower.contains("llama") {
        (128_000, 4_096, false)
    } else if model_lower.contains("deepseek") {
        (64_000, 8_192, false)
    } else {
        (32_000, 4_096, false)
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

impl TogetherProvider {
    pub fn new(api_key: String, model: String, _base_url: Option<String>) -> Self {
        let client = rig::providers::together::Client::new(&api_key)
            .expect("Failed to create Together client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for TogetherProvider {
    fn name(&self) -> &str {
        "together"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_capabilities() {
        let kimi_caps = get_model_capabilities("moonshotai/Kimi-K2.5");
        assert_eq!(kimi_caps.max_context_tokens, Some(131_072));
        assert!(kimi_caps.supports_tools);

        let qwen_caps = get_model_capabilities("Qwen/Qwen3-Coder-Next-FP8");
        assert_eq!(qwen_caps.max_context_tokens, Some(131_072));
    }
}
