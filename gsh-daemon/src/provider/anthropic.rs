use super::{
    ChatMessage, ChatResponse, Provider, ProviderCapabilities, StreamEvent, ToolDefinition,
};
use super::rig_adapt;
use rig::client::CompletionClient;
use anyhow::Result;
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

/// Anthropic Claude API provider (backed by rig-core)
pub struct AnthropicProvider {
    model: rig::providers::anthropic::completion::CompletionModel,
    model_name: String,
}

/// Model capability information for Anthropic models
fn get_model_capabilities(model: &str) -> ProviderCapabilities {
    let (context_tokens, output_tokens) = if model.contains("opus") {
        (200_000, 4_096)
    } else if model.contains("sonnet") {
        (200_000, 8_192)
    } else if model.contains("haiku") {
        (200_000, 4_096)
    } else {
        (100_000, 4_096)
    };

    ProviderCapabilities {
        supports_tools: true,
        supports_streaming: true,
        supports_system_message: true,
        supports_vision: true,
        max_context_tokens: Some(context_tokens),
        max_output_tokens: Some(output_tokens),
    }
}

impl AnthropicProvider {
    pub fn new(api_key: String, model: String) -> Self {
        let client = rig::providers::anthropic::Client::new(&api_key)
            .expect("Failed to create Anthropic client");
        let model_obj = client.completion_model(&model);
        Self {
            model: model_obj,
            model_name: model,
        }
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
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
    use super::super::{ChatRole, ContentBlock, MessageContent, UsageStats};

    #[test]
    fn test_provider_name_and_model() {
        let provider = AnthropicProvider {
            model: {
                // Create a dummy model for testing - we can't call the API
                // so we test the provider metadata methods
                let client = rig::providers::anthropic::Client::new("test-api-key")
                    .expect("Failed to create test client");
                client.completion_model("claude-sonnet-4-20250514")
            },
            model_name: "claude-sonnet-4-20250514".to_string(),
        };

        assert_eq!(provider.name(), "anthropic");
        assert_eq!(provider.model(), "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_model_capabilities() {
        let caps = get_model_capabilities("claude-sonnet-4-20250514");
        assert_eq!(caps.max_context_tokens, Some(200_000));
        assert_eq!(caps.max_output_tokens, Some(8_192));
        assert!(caps.supports_tools);
        assert!(caps.supports_vision);
    }

    // =========================================================================
    // rig_adapt conversion tests (testing the conversion layer)
    // =========================================================================

    #[test]
    fn test_user_message_conversion() {
        use rig::message::Message;

        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("Hello, Claude!".to_string()),
        };

        let rig_messages = rig_adapt::to_rig_messages(&[msg]);
        assert_eq!(rig_messages.len(), 1);
        assert!(matches!(&rig_messages[0], Message::User { .. }));
    }

    #[test]
    fn test_tool_use_block_conversion() {
        let blocks = vec![
            ContentBlock::Text { text: "Let me check.".to_string() },
            ContentBlock::ToolUse {
                id: "toolu_123".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls -la"}),
            },
        ];

        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Blocks(blocks),
        };

        let rig_messages = rig_adapt::to_rig_messages(&[msg]);
        assert_eq!(rig_messages.len(), 1);
        assert!(matches!(&rig_messages[0], rig::message::Message::Assistant { .. }));
    }

    #[test]
    fn test_tool_result_conversion() {
        let blocks = vec![ContentBlock::ToolResult {
            tool_use_id: "toolu_123".to_string(),
            content: "file1.txt\nfile2.txt".to_string(),
            is_error: Some(false),
        }];

        let msg = ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Blocks(blocks),
        };

        let rig_messages = rig_adapt::to_rig_messages(&[msg]);
        assert_eq!(rig_messages.len(), 1);
        assert!(matches!(&rig_messages[0], rig::message::Message::User { .. }));
    }

    #[test]
    fn test_tool_definition_conversion() {
        let tool = ToolDefinition {
            name: "execute_command".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                },
                "required": ["command"]
            }),
        };

        let rig_tools = rig_adapt::to_rig_tools(&[tool]);
        assert_eq!(rig_tools.len(), 1);
        assert_eq!(rig_tools[0].name, "execute_command");
        assert_eq!(rig_tools[0].description, "Run a shell command");
    }

    #[test]
    fn test_usage_stats_conversion() {
        let usage = rig::completion::Usage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        };

        let stats: UsageStats = rig_adapt::from_rig_usage(usage);
        assert_eq!(stats.input_tokens, 100);
        assert_eq!(stats.output_tokens, 50);
        assert_eq!(stats.total_tokens, 150);
    }
}
