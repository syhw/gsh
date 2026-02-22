pub mod anthropic;
pub mod moonshot;
pub mod ollama;
pub mod openai;
pub mod openai_responses;
pub mod rig_adapt;
pub mod together;
pub mod zhipu;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::pin::Pin;
use futures::Stream;

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during provider operations
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ProviderError {
    /// Authentication failed (invalid API key, etc.)
    AuthenticationError(String),
    /// Rate limit exceeded
    RateLimitError {
        message: String,
        retry_after_secs: Option<u64>,
    },
    /// Request was invalid
    InvalidRequestError(String),
    /// Model not found or not available
    ModelNotFoundError(String),
    /// Context length exceeded
    ContextLengthExceededError {
        message: String,
        max_tokens: Option<u64>,
    },
    /// Network or connection error
    NetworkError(String),
    /// Provider service error (5xx)
    ServiceError(String),
    /// Timeout during request
    TimeoutError(String),
    /// Content was filtered/blocked
    ContentFilteredError(String),
    /// Unknown or other error
    Other(String),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthenticationError(msg) => write!(f, "Authentication error: {}", msg),
            Self::RateLimitError { message, retry_after_secs } => {
                if let Some(secs) = retry_after_secs {
                    write!(f, "Rate limit exceeded: {} (retry after {}s)", message, secs)
                } else {
                    write!(f, "Rate limit exceeded: {}", message)
                }
            }
            Self::InvalidRequestError(msg) => write!(f, "Invalid request: {}", msg),
            Self::ModelNotFoundError(msg) => write!(f, "Model not found: {}", msg),
            Self::ContextLengthExceededError { message, max_tokens } => {
                if let Some(max) = max_tokens {
                    write!(f, "Context length exceeded: {} (max: {})", message, max)
                } else {
                    write!(f, "Context length exceeded: {}", message)
                }
            }
            Self::NetworkError(msg) => write!(f, "Network error: {}", msg),
            Self::ServiceError(msg) => write!(f, "Service error: {}", msg),
            Self::TimeoutError(msg) => write!(f, "Timeout: {}", msg),
            Self::ContentFilteredError(msg) => write!(f, "Content filtered: {}", msg),
            Self::Other(msg) => write!(f, "Error: {}", msg),
        }
    }
}

impl std::error::Error for ProviderError {}

#[allow(dead_code)]
impl ProviderError {
    /// Returns true if this error is retryable
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimitError { .. }
                | Self::NetworkError(_)
                | Self::ServiceError(_)
                | Self::TimeoutError(_)
        )
    }

    /// Returns the recommended retry delay in seconds, if any
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimitError { retry_after_secs, .. } => *retry_after_secs,
            Self::NetworkError(_) | Self::ServiceError(_) | Self::TimeoutError(_) => Some(1),
            _ => None,
        }
    }
}

// ============================================================================
// Provider Capabilities
// ============================================================================

/// Capabilities supported by a provider
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct ProviderCapabilities {
    /// Whether the provider supports tool/function calling
    pub supports_tools: bool,
    /// Whether the provider supports streaming responses
    pub supports_streaming: bool,
    /// Whether the provider supports system messages
    pub supports_system_message: bool,
    /// Whether the provider supports vision/image inputs
    pub supports_vision: bool,
    /// Maximum context length in tokens
    pub max_context_tokens: Option<u64>,
    /// Maximum output tokens
    pub max_output_tokens: Option<u64>,
}

// ============================================================================
// Model Information
// ============================================================================

/// Information about the model being used
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct ModelInfo {
    /// Model identifier (e.g., "claude-sonnet-4-20250514")
    pub id: String,
    /// Human-readable model name
    pub name: String,
    /// Provider name
    pub provider: String,
    /// Maximum context window size in tokens
    pub context_window: Option<u64>,
    /// Maximum output tokens
    pub max_output_tokens: Option<u64>,
}

// ============================================================================
// Usage Statistics
// ============================================================================

/// Token usage statistics for a request
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    /// Number of input/prompt tokens
    pub input_tokens: u64,
    /// Number of output/completion tokens
    pub output_tokens: u64,
    /// Total tokens (input + output)
    pub total_tokens: u64,
    /// Cache read tokens (if applicable)
    pub cache_read_tokens: Option<u64>,
    /// Cache creation tokens (if applicable)
    pub cache_creation_tokens: Option<u64>,
}

impl UsageStats {
    pub fn new(input_tokens: u64, output_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens: input_tokens + output_tokens,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }
    }
}

// ============================================================================
// Chat Request/Response Types
// ============================================================================

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

#[allow(dead_code)]
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

    /// Extract all text content from the message
    pub fn all_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }

    /// Check if the message contains any tool use blocks
    pub fn has_tool_use(&self) -> bool {
        match self {
            Self::Text(_) => false,
            Self::Blocks(blocks) => blocks.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })),
        }
    }

    /// Extract all tool use blocks from the message
    pub fn tool_uses(&self) -> Vec<&ContentBlock> {
        match self {
            Self::Text(_) => vec![],
            Self::Blocks(blocks) => blocks
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .collect(),
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

/// Response from a chat completion
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ChatResponse {
    /// The response message
    pub message: ChatMessage,
    /// Token usage statistics
    pub usage: Option<UsageStats>,
    /// Stop reason (e.g., "end_turn", "tool_use", "max_tokens")
    pub stop_reason: Option<String>,
    /// Model that generated the response
    pub model: Option<String>,
}

#[allow(dead_code)]
impl ChatResponse {
    /// Create a simple response with just a message
    pub fn from_message(message: ChatMessage) -> Self {
        Self {
            message,
            usage: None,
            stop_reason: None,
            model: None,
        }
    }

    /// Check if the response ended due to tool use
    pub fn is_tool_use(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use")
            || self.message.content.has_tool_use()
    }
}

/// Streaming event from the provider
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StreamEvent {
    /// Text content delta
    TextDelta(String),
    /// Tool use started
    ToolUseStart { id: String, name: String },
    /// Tool use input delta (JSON string chunk)
    ToolUseInputDelta(String),
    /// Message complete with usage stats
    MessageComplete {
        usage: Option<UsageStats>,
        stop_reason: Option<String>,
    },
    /// Error occurred
    Error(ProviderError),
}

// For backward compatibility
#[allow(dead_code)]
impl StreamEvent {
    /// Create a simple message complete event (for backward compatibility)
    pub fn message_complete() -> Self {
        Self::MessageComplete {
            usage: None,
            stop_reason: None,
        }
    }
}

// ============================================================================
// Provider Trait
// ============================================================================

/// Provider trait for LLM backends
///
/// This trait defines the interface that all LLM providers must implement.
/// It supports both synchronous and streaming chat completions, as well as
/// capability introspection and model information.
#[async_trait]
#[allow(dead_code)]
pub trait Provider: Send + Sync {
    /// Get the provider name (e.g., "anthropic", "openai")
    fn name(&self) -> &str;

    /// Get the current model identifier
    fn model(&self) -> &str;

    /// Get provider capabilities
    fn capabilities(&self) -> ProviderCapabilities {
        // Default capabilities - providers should override
        ProviderCapabilities {
            supports_tools: true,
            supports_streaming: true,
            supports_system_message: true,
            supports_vision: false,
            max_context_tokens: None,
            max_output_tokens: None,
        }
    }

    /// Get detailed model information
    fn model_info(&self) -> ModelInfo {
        ModelInfo {
            id: self.model().to_string(),
            name: self.model().to_string(),
            provider: self.name().to_string(),
            context_window: self.capabilities().max_context_tokens,
            max_output_tokens: self.capabilities().max_output_tokens,
        }
    }

    /// Send a chat completion request and get a full response
    ///
    /// This is the primary method for non-streaming interactions.
    async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatMessage>;

    /// Send a chat completion request with full response including usage stats
    ///
    /// This method returns a ChatResponse which includes usage statistics
    /// and other metadata. The default implementation wraps the chat() method.
    async fn chat_with_usage(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<ChatResponse> {
        let message = self.chat(messages, system, tools, max_tokens).await?;
        Ok(ChatResponse::from_message(message))
    }

    /// Send a chat completion request and stream the response
    async fn chat_stream(
        &self,
        messages: Vec<ChatMessage>,
        system: Option<&str>,
        tools: Option<&[ToolDefinition]>,
        max_tokens: u32,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>>;

    /// Estimate the number of tokens in a text string
    ///
    /// This is a rough estimate. Different providers use different tokenizers.
    /// The default implementation uses a simple word-based estimate.
    fn estimate_tokens(&self, text: &str) -> u64 {
        // Rough estimate: ~4 chars per token for English text
        // This is a very rough approximation; providers should override
        (text.len() as u64 + 3) / 4
    }

    /// Estimate tokens for a list of messages
    fn estimate_message_tokens(&self, messages: &[ChatMessage]) -> u64 {
        let mut total = 0u64;
        for msg in messages {
            // Add overhead per message (role, formatting)
            total += 4;
            total += self.estimate_tokens(&msg.content.all_text());
        }
        total
    }
}

/// Check if a model requires the OpenAI Responses API instead of Chat Completions.
fn uses_responses_api(model: &str) -> bool {
    let m = model.to_lowercase();
    m.contains("codex") || m.starts_with("gpt-5")
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
            let model = config.llm.openai.model.clone();
            if uses_responses_api(&model) {
                Ok(Box::new(openai_responses::OpenAIResponsesProvider::new(
                    api_key,
                    model,
                    None, // Always use default Responses API URL
                )))
            } else {
                Ok(Box::new(openai::OpenAIProvider::new(
                    api_key,
                    model,
                    config.llm.openai.base_url.clone(),
                )))
            }
        }
        "moonshot" | "kimi" => {
            let api_key = config
                .moonshot_api_key()
                .ok_or_else(|| anyhow::anyhow!("Moonshot API key not configured"))?;
            Ok(Box::new(moonshot::MoonshotProvider::new(
                api_key,
                config.llm.moonshot.model.clone(),
                config.llm.moonshot.base_url.clone(),
            )))
        }
        "ollama" => {
            Ok(Box::new(ollama::OllamaProvider::new(
                config.llm.ollama.model.clone(),
                config.llm.ollama.base_url.clone(),
            )))
        }
        "z" | "zai" => {
            let api_key = config
                .zhipu_api_key()
                .ok_or_else(|| anyhow::anyhow!("Z API key not configured (set ZAI_API_KEY)"))?;
            Ok(Box::new(zhipu::ZhipuProvider::new(
                api_key,
                config.llm.zhipu.model.clone(),
                config.llm.zhipu.base_url.clone(),
            )))
        }
        "together" => {
            let api_key = config
                .together_api_key()
                .ok_or_else(|| anyhow::anyhow!("Together API key not configured (set TOGETHER_API_KEY)"))?;
            Ok(Box::new(together::TogetherProvider::new(
                api_key,
                config.llm.together.model.clone(),
                config.llm.together.base_url.clone(),
            )))
        }
        "mistral" => {
            let api_key = config
                .mistral_api_key()
                .ok_or_else(|| anyhow::anyhow!("Mistral API key not configured (set MISTRAL_API_KEY)"))?;
            let base_url = config.llm.mistral.base_url.clone()
                .unwrap_or_else(|| "https://api.mistral.ai/v1/chat/completions".to_string());
            Ok(Box::new(openai::OpenAIProvider::new(
                api_key,
                config.llm.mistral.model.clone(),
                Some(base_url),
            )))
        }
        "cerebras" => {
            let api_key = config
                .cerebras_api_key()
                .ok_or_else(|| anyhow::anyhow!("Cerebras API key not configured (set CEREBRAS_API_KEY)"))?;
            let base_url = config.llm.cerebras.base_url.clone()
                .unwrap_or_else(|| "https://api.cerebras.ai/v1/chat/completions".to_string());
            Ok(Box::new(openai::OpenAIProvider::new(
                api_key,
                config.llm.cerebras.model.clone(),
                Some(base_url),
            )))
        }
        _ => Err(anyhow::anyhow!("Unknown provider: {}", provider_name)),
    }
}

/// Create a provider with model override
pub fn create_provider_with_model(
    provider_name: &str,
    model: &str,
    config: &crate::config::Config,
) -> Result<Box<dyn Provider>> {
    match provider_name {
        "anthropic" => {
            let api_key = config
                .anthropic_api_key()
                .ok_or_else(|| anyhow::anyhow!("Anthropic API key not configured"))?;
            Ok(Box::new(anthropic::AnthropicProvider::new(
                api_key,
                model.to_string(),
            )))
        }
        "openai" => {
            let api_key = config
                .openai_api_key()
                .ok_or_else(|| anyhow::anyhow!("OpenAI API key not configured"))?;
            if uses_responses_api(model) {
                Ok(Box::new(openai_responses::OpenAIResponsesProvider::new(
                    api_key,
                    model.to_string(),
                    None,
                )))
            } else {
                Ok(Box::new(openai::OpenAIProvider::new(
                    api_key,
                    model.to_string(),
                    config.llm.openai.base_url.clone(),
                )))
            }
        }
        "moonshot" | "kimi" => {
            let api_key = config
                .moonshot_api_key()
                .ok_or_else(|| anyhow::anyhow!("Moonshot API key not configured"))?;
            Ok(Box::new(moonshot::MoonshotProvider::new(
                api_key,
                model.to_string(),
                config.llm.moonshot.base_url.clone(),
            )))
        }
        "ollama" => {
            Ok(Box::new(ollama::OllamaProvider::new(
                model.to_string(),
                config.llm.ollama.base_url.clone(),
            )))
        }
        "z" | "zai" => {
            let api_key = config
                .zhipu_api_key()
                .ok_or_else(|| anyhow::anyhow!("Z API key not configured (set ZAI_API_KEY)"))?;
            Ok(Box::new(zhipu::ZhipuProvider::new(
                api_key,
                model.to_string(),
                config.llm.zhipu.base_url.clone(),
            )))
        }
        "together" => {
            let api_key = config
                .together_api_key()
                .ok_or_else(|| anyhow::anyhow!("Together API key not configured (set TOGETHER_API_KEY)"))?;
            Ok(Box::new(together::TogetherProvider::new(
                api_key,
                model.to_string(),
                config.llm.together.base_url.clone(),
            )))
        }
        "mistral" => {
            let api_key = config
                .mistral_api_key()
                .ok_or_else(|| anyhow::anyhow!("Mistral API key not configured (set MISTRAL_API_KEY)"))?;
            let base_url = config.llm.mistral.base_url.clone()
                .unwrap_or_else(|| "https://api.mistral.ai/v1/chat/completions".to_string());
            Ok(Box::new(openai::OpenAIProvider::new(
                api_key,
                model.to_string(),
                Some(base_url),
            )))
        }
        "cerebras" => {
            let api_key = config
                .cerebras_api_key()
                .ok_or_else(|| anyhow::anyhow!("Cerebras API key not configured (set CEREBRAS_API_KEY)"))?;
            let base_url = config.llm.cerebras.base_url.clone()
                .unwrap_or_else(|| "https://api.cerebras.ai/v1/chat/completions".to_string());
            Ok(Box::new(openai::OpenAIProvider::new(
                api_key,
                model.to_string(),
                Some(base_url),
            )))
        }
        _ => Err(anyhow::anyhow!("Unknown provider: {}", provider_name)),
    }
}
