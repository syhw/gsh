//! Conversion layer between gsh internal types and rig-core types.
//!
//! This module is the ONLY place that imports rig message/completion types.
//! All provider implementations delegate through these conversions to keep
//! the gsh-internal on-disk format (JSONL sessions) stable.

use super::{
    ChatMessage, ChatResponse, ChatRole, ContentBlock, MessageContent, ProviderError, StreamEvent,
    ToolDefinition as GshToolDefinition, UsageStats,
};
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
    ToolDefinition as RigToolDefinition, Usage,
};
use rig::message::{
    AssistantContent, Message, Text, ToolCall, ToolFunction, ToolResult, ToolResultContent,
    UserContent,
};
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamingCompletionResponse};

use futures::StreamExt;
use std::pin::Pin;

// ============================================================================
// gsh → rig conversions
// ============================================================================

/// Convert gsh ChatMessages to rig Messages.
pub fn to_rig_messages(messages: &[ChatMessage]) -> Vec<Message> {
    let mut result = Vec::new();

    for msg in messages {
        match msg.role {
            ChatRole::User => {
                let contents = to_rig_user_content(&msg.content);
                if !contents.is_empty() {
                    if let Ok(one_or_many) = OneOrMany::many(contents) {
                        result.push(Message::User {
                            content: one_or_many,
                        });
                    }
                }
            }
            ChatRole::Assistant => {
                let contents = to_rig_assistant_content(&msg.content);
                if !contents.is_empty() {
                    if let Ok(one_or_many) = OneOrMany::many(contents) {
                        result.push(Message::Assistant {
                            id: None,
                            content: one_or_many,
                        });
                    }
                }
            }
        }
    }

    result
}

fn to_rig_user_content(content: &MessageContent) -> Vec<UserContent> {
    match content {
        MessageContent::Text(s) => vec![UserContent::Text(Text { text: s.clone() })],
        MessageContent::Blocks(blocks) => {
            let mut result = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        result.push(UserContent::Text(Text { text: text.clone() }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        result.push(UserContent::ToolResult(ToolResult {
                            id: tool_use_id.clone(),
                            call_id: None,
                            content: OneOrMany::one(ToolResultContent::Text(Text {
                                text: content.clone(),
                            })),
                        }));
                    }
                    ContentBlock::ToolUse { .. } => {
                        // ToolUse blocks don't belong in user messages;
                        // skip them (they appear in assistant messages)
                    }
                }
            }
            result
        }
    }
}

fn to_rig_assistant_content(content: &MessageContent) -> Vec<AssistantContent> {
    match content {
        MessageContent::Text(s) => vec![AssistantContent::Text(Text { text: s.clone() })],
        MessageContent::Blocks(blocks) => {
            let mut result = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text { text } => {
                        result.push(AssistantContent::Text(Text { text: text.clone() }));
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        result.push(AssistantContent::ToolCall(ToolCall {
                            id: id.clone(),
                            call_id: None,
                            function: ToolFunction {
                                name: name.clone(),
                                arguments: input.clone(),
                            },
                            signature: None,
                            additional_params: None,
                        }));
                    }
                    ContentBlock::ToolResult { .. } => {
                        // ToolResult blocks don't belong in assistant messages; skip
                    }
                }
            }
            result
        }
    }
}

/// Convert gsh ToolDefinitions to rig ToolDefinitions.
pub fn to_rig_tools(tools: &[GshToolDefinition]) -> Vec<RigToolDefinition> {
    tools
        .iter()
        .map(|t| RigToolDefinition {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        })
        .collect()
}

/// Build a rig CompletionRequest from gsh types.
pub fn build_request(
    messages: &[ChatMessage],
    system: Option<&str>,
    tools: Option<&[GshToolDefinition]>,
    max_tokens: u32,
) -> CompletionRequest {
    let rig_messages = to_rig_messages(messages);
    let rig_tools = tools.map(to_rig_tools).unwrap_or_default();

    // CompletionRequest expects at least one message in chat_history.
    // The last message is typically the user prompt.
    let chat_history = if rig_messages.is_empty() {
        OneOrMany::one(Message::user(""))
    } else {
        OneOrMany::many(rig_messages).unwrap_or_else(|_| OneOrMany::one(Message::user("")))
    };

    CompletionRequest {
        preamble: system.map(|s| s.to_string()),
        chat_history,
        documents: Vec::new(),
        tools: rig_tools,
        temperature: None,
        max_tokens: Some(max_tokens as u64),
        tool_choice: None,
        additional_params: None,
    }
}

// ============================================================================
// rig → gsh conversions
// ============================================================================

/// Convert a rig CompletionResponse to a gsh ChatResponse.
pub fn from_rig_response<R>(response: CompletionResponse<R>) -> ChatResponse {
    let blocks = from_rig_assistant_contents(response.choice);

    let message = ChatMessage {
        role: ChatRole::Assistant,
        content: simplify_blocks(blocks),
    };

    ChatResponse {
        message,
        usage: Some(from_rig_usage(response.usage)),
        stop_reason: None,
        model: None,
    }
}

fn from_rig_assistant_contents(choice: OneOrMany<AssistantContent>) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    for item in choice.into_iter() {
        match item {
            AssistantContent::Text(t) => {
                if !t.text.is_empty() {
                    blocks.push(ContentBlock::Text { text: t.text });
                }
            }
            AssistantContent::ToolCall(tc) => {
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input: tc.function.arguments,
                });
            }
            _ => {
                // Reasoning, Image — skip for now
            }
        }
    }
    blocks
}

/// Simplify a vec of content blocks: if there's exactly one Text block, use MessageContent::Text.
fn simplify_blocks(blocks: Vec<ContentBlock>) -> MessageContent {
    if blocks.len() == 1 {
        if let ContentBlock::Text { ref text } = blocks[0] {
            return MessageContent::Text(text.clone());
        }
    }
    if blocks.is_empty() {
        return MessageContent::Text(String::new());
    }
    MessageContent::Blocks(blocks)
}

/// Convert rig Usage to gsh UsageStats.
pub fn from_rig_usage(usage: Usage) -> UsageStats {
    UsageStats {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cache_read_tokens: None,
        cache_creation_tokens: None,
    }
}

/// Convert a CompletionError to an anyhow::Error.
pub fn from_rig_error(err: CompletionError) -> anyhow::Error {
    anyhow::anyhow!("{}", err)
}

// ============================================================================
// Streaming adapter
// ============================================================================

/// Adapt a rig streaming response into a gsh StreamEvent stream.
///
/// This is the core streaming adapter that all providers use.
pub fn adapt_stream<R>(
    mut rig_stream: StreamingCompletionResponse<R>,
) -> Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>
where
    R: Clone
        + Unpin
        + Send
        + Sync
        + serde::Serialize
        + serde::de::DeserializeOwned
        + rig::completion::GetTokenUsage
        + 'static,
{
    let stream = async_stream::stream! {
        while let Some(chunk) = rig_stream.next().await {
            match chunk {
                Ok(content) => {
                    for event in convert_streamed_content(content) {
                        yield event;
                    }
                }
                Err(e) => {
                    yield StreamEvent::Error(ProviderError::Other(e.to_string()));
                }
            }
        }
    };

    Box::pin(stream)
}

fn convert_streamed_content<R>(content: StreamedAssistantContent<R>) -> Vec<StreamEvent>
where
    R: rig::completion::GetTokenUsage,
{
    match content {
        StreamedAssistantContent::Text(t) => {
            if t.text.is_empty() {
                vec![]
            } else {
                vec![StreamEvent::TextDelta(t.text)]
            }
        }
        StreamedAssistantContent::ToolCall(tc) => {
            let mut events = vec![StreamEvent::ToolUseStart {
                id: tc.id,
                name: tc.function.name.clone(),
            }];
            let args_str = serde_json::to_string(&tc.function.arguments).unwrap_or_default();
            if !args_str.is_empty() {
                events.push(StreamEvent::ToolUseInputDelta(args_str));
            }
            events
        }
        StreamedAssistantContent::ToolCallDelta { id, content } => {
            match content {
                rig::streaming::ToolCallDeltaContent::Name(name) => {
                    vec![StreamEvent::ToolUseStart { id, name }]
                }
                rig::streaming::ToolCallDeltaContent::Delta(s) => {
                    if s.is_empty() {
                        vec![]
                    } else {
                        vec![StreamEvent::ToolUseInputDelta(s)]
                    }
                }
            }
        }
        StreamedAssistantContent::Final(r) => {
            let usage = r.token_usage().map(from_rig_usage);
            vec![StreamEvent::MessageComplete {
                usage,
                stop_reason: None,
            }]
        }
        _ => {
            // Reasoning, ReasoningDelta — skip for now
            vec![]
        }
    }
}

// ============================================================================
// Generic provider implementation helper
// ============================================================================

/// Execute a non-streaming completion through rig and convert to gsh ChatResponse.
pub async fn rig_chat_with_usage<M: CompletionModel>(
    model: &M,
    messages: Vec<ChatMessage>,
    system: Option<&str>,
    tools: Option<&[GshToolDefinition]>,
    max_tokens: u32,
) -> anyhow::Result<ChatResponse> {
    let request = build_request(&messages, system, tools, max_tokens);
    let response = model.completion(request).await.map_err(from_rig_error)?;
    Ok(from_rig_response(response))
}

/// Execute a streaming completion through rig and return a gsh StreamEvent stream.
pub async fn rig_chat_stream<M>(
    model: &M,
    messages: Vec<ChatMessage>,
    system: Option<&str>,
    tools: Option<&[GshToolDefinition]>,
    max_tokens: u32,
) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>>
where
    M: CompletionModel,
    M::StreamingResponse: Clone
        + Unpin
        + Send
        + Sync
        + serde::Serialize
        + serde::de::DeserializeOwned
        + rig::completion::GetTokenUsage
        + 'static,
{
    let request = build_request(&messages, system, tools, max_tokens);
    let stream = model.stream(request).await.map_err(from_rig_error)?;
    Ok(adapt_stream(stream))
}
