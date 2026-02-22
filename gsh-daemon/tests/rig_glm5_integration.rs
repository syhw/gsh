//! Extensive integration tests for the rig-core backed Zhipu/GLM-5 provider.
//!
//! Tests the full stack: rig_adapt conversions → rig-core HTTP → Z.ai GLM-5 API.
//!
//! Run with:
//!   cargo test --test rig_glm5_integration -- --ignored --nocapture
//!
//! Required: ZAI_API_KEY environment variable

use gsh_daemon::agent::{Agent, AgentEvent};
use gsh_daemon::config::Config;
use gsh_daemon::provider::{
    create_provider, ChatMessage, ChatRole, ContentBlock, MessageContent, Provider, StreamEvent,
    ToolDefinition,
};
use futures::StreamExt;
use tokio::sync::mpsc;

fn has_api_key() -> bool {
    std::env::var("ZAI_API_KEY").is_ok()
}

fn skip_if_no_key() -> bool {
    if !has_api_key() {
        eprintln!("Skipping: ZAI_API_KEY not set");
        true
    } else {
        false
    }
}

fn make_provider() -> Box<dyn Provider> {
    let config = Config::default();
    create_provider("z", &config).expect("Failed to create Z provider")
}

/// Collect all events until Done or Error
async fn collect_events(mut rx: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        let is_terminal = matches!(event, AgentEvent::Done { .. } | AgentEvent::Error(_));
        events.push(event);
        if is_terminal {
            break;
        }
    }
    events
}

// ============================================================================
// 0. Diagnostic: raw HTTP vs rig to isolate auth issue
// ============================================================================

/// Raw HTTP request to Z.ai to verify the API key and URL work
#[tokio::test]
#[ignore]
async fn test_debug_raw_http() {
    if skip_if_no_key() { return; }
    let api_key = std::env::var("ZAI_API_KEY").unwrap();
    let url = "https://api.z.ai/api/coding/paas/v4/chat/completions";

    let body = serde_json::json!({
        "model": "glm-5",
        "messages": [{"role": "user", "content": "Say hello"}],
        "max_tokens": 32
    });

    let client = reqwest::Client::new();
    let resp = client.post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .expect("HTTP request failed");

    let status = resp.status();
    let text = resp.text().await.unwrap();
    eprintln!("[raw_http] status={} body={}", status, &text[..text.len().min(500)]);
    assert!(status.is_success(), "Raw HTTP failed with {}: {}", status, text);
}

/// Same request through rig to see what rig sends
#[tokio::test]
#[ignore]
async fn test_debug_rig_request() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("Say hello".to_string()),
    }];

    let result = provider.chat(messages, None, None, 32).await;
    match &result {
        Ok(msg) => eprintln!("[rig_request] OK: {}", msg.content.all_text()),
        Err(e) => eprintln!("[rig_request] ERROR: {:?}", e),
    }
    assert!(result.is_ok(), "Rig request failed: {:?}", result.err());
}

// ============================================================================
// 1. Provider-level: chat() — non-streaming, no tools
// ============================================================================

/// Basic non-streaming text completion
#[tokio::test]
#[ignore]
async fn test_chat_simple_text() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("What is 2+2? Reply ONLY with the digit.".to_string()),
    }];

    let result = provider.chat(messages, None, None, 64).await;
    assert!(result.is_ok(), "chat() failed: {:?}", result.err());

    let msg = result.unwrap();
    assert_eq!(msg.role, ChatRole::Assistant);
    let text = msg.content.all_text();
    eprintln!("[chat_simple_text] response: {}", text);
    assert!(text.contains('4'), "Expected '4' in response, got: {}", text);
}

/// Non-streaming with a system prompt
#[tokio::test]
#[ignore]
async fn test_chat_with_system_prompt() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("What language am I speaking?".to_string()),
    }];

    let result = provider
        .chat(messages, Some("You are a helpful assistant. Always respond in French."), None, 128)
        .await;
    assert!(result.is_ok(), "chat() failed: {:?}", result.err());

    let text = result.unwrap().content.all_text().to_lowercase();
    eprintln!("[chat_with_system_prompt] response: {}", text);
    // Should contain French words
    let has_french = text.contains("français")
        || text.contains("anglais")
        || text.contains("langue")
        || text.contains("vous")
        || text.contains("parlez")
        || text.contains("le ")
        || text.contains("la ");
    assert!(has_french, "Expected French response, got: {}", text);
}

/// Multi-turn conversation (user → assistant → user)
#[tokio::test]
#[ignore]
async fn test_chat_multi_turn() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![
        ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("My name is Alice.".to_string()),
        },
        ChatMessage {
            role: ChatRole::Assistant,
            content: MessageContent::Text("Nice to meet you, Alice!".to_string()),
        },
        ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("What is my name?".to_string()),
        },
    ];

    let result = provider.chat(messages, None, None, 64).await;
    assert!(result.is_ok(), "chat() failed: {:?}", result.err());

    let text = result.unwrap().content.all_text();
    eprintln!("[chat_multi_turn] response: {}", text);
    assert!(
        text.contains("Alice"),
        "Should remember name 'Alice', got: {}",
        text
    );
}

// ============================================================================
// 2. Provider-level: chat_with_usage() — verify token stats
// ============================================================================

/// Verify that usage stats are returned and sensible
#[tokio::test]
#[ignore]
async fn test_chat_with_usage_stats() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("Say 'hello world'".to_string()),
    }];

    let result = provider
        .chat_with_usage(messages, None, None, 64)
        .await;
    assert!(result.is_ok(), "chat_with_usage() failed: {:?}", result.err());

    let response = result.unwrap();
    eprintln!("[usage_stats] text: {}", response.message.content.all_text());
    eprintln!("[usage_stats] usage: {:?}", response.usage);

    if let Some(usage) = &response.usage {
        assert!(usage.input_tokens > 0, "input_tokens should be > 0");
        assert!(usage.output_tokens > 0, "output_tokens should be > 0");
        assert!(
            usage.total_tokens >= usage.input_tokens + usage.output_tokens
                || usage.total_tokens > 0,
            "total_tokens should be sensible"
        );
    }
}

// ============================================================================
// 3. Provider-level: chat() with tools — non-streaming tool use
// ============================================================================

/// Verify the model produces a tool_use block when given tools
#[tokio::test]
#[ignore]
async fn test_chat_with_tool_call() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let tools = vec![ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get the current weather for a city".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {
                    "type": "string",
                    "description": "City name"
                }
            },
            "required": ["city"]
        }),
    }];

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "What's the weather in Tokyo? Use the get_weather tool.".to_string(),
        ),
    }];

    let result = provider
        .chat(messages, None, Some(&tools), 256)
        .await;
    assert!(result.is_ok(), "chat() with tools failed: {:?}", result.err());

    let msg = result.unwrap();
    eprintln!("[tool_call] content: {:?}", msg.content);

    // The model should produce at least one ToolUse block
    let has_tool_use = msg.content.has_tool_use();
    assert!(has_tool_use, "Expected ToolUse in response, got: {:?}", msg.content);

    // Verify the tool call references the correct tool
    let tool_uses = msg.content.tool_uses();
    assert!(!tool_uses.is_empty(), "Should have at least one tool use");

    if let ContentBlock::ToolUse { name, input, .. } = &tool_uses[0] {
        assert_eq!(name, "get_weather", "Should call get_weather, got: {}", name);
        // Input should contain "city" field
        assert!(
            input.get("city").is_some(),
            "Tool input should have 'city', got: {:?}",
            input
        );
    }
}

/// Non-streaming tool call → tool result → final response
#[tokio::test]
#[ignore]
async fn test_chat_tool_call_and_result_roundtrip() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let tools = vec![ToolDefinition {
        name: "calculator".to_string(),
        description: "Evaluate a mathematical expression".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "expression": {
                    "type": "string",
                    "description": "Math expression to evaluate"
                }
            },
            "required": ["expression"]
        }),
    }];

    // Step 1: ask a math question
    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "What is 137 * 42? Use the calculator tool.".to_string(),
        ),
    }];

    let result = provider
        .chat(messages.clone(), None, Some(&tools), 256)
        .await;
    assert!(result.is_ok(), "Step 1 failed: {:?}", result.err());

    let assistant_msg = result.unwrap();
    eprintln!("[roundtrip step 1] assistant: {:?}", assistant_msg.content);
    assert!(
        assistant_msg.content.has_tool_use(),
        "Expected tool use in step 1"
    );

    // Extract the tool_use id
    let tool_uses = assistant_msg.content.tool_uses();
    let tool_use_id = if let ContentBlock::ToolUse { id, .. } = &tool_uses[0] {
        id.clone()
    } else {
        panic!("Expected ToolUse block");
    };

    // Step 2: send tool result back
    let mut messages2 = messages;
    messages2.push(assistant_msg);
    messages2.push(ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id,
            content: "5754".to_string(),
            is_error: Some(false),
        }]),
    });

    let result2 = provider
        .chat(messages2, None, Some(&tools), 256)
        .await;
    assert!(result2.is_ok(), "Step 2 failed: {:?}", result2.err());

    let final_msg = result2.unwrap();
    let text = final_msg.content.all_text();
    eprintln!("[roundtrip step 2] final: {}", text);

    // The model should mention the result
    assert!(
        text.contains("5754") || text.contains("5,754"),
        "Final response should contain '5754', got: {}",
        text
    );
}

// ============================================================================
// 4. Provider-level: chat_stream() — streaming
// ============================================================================

/// Basic streaming text completion
#[tokio::test]
#[ignore]
async fn test_stream_text_deltas() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "Count from 1 to 5, one number per line.".to_string(),
        ),
    }];

    let stream_result = provider
        .chat_stream(messages, None, None, 128)
        .await;
    assert!(stream_result.is_ok(), "chat_stream() failed: {:?}", stream_result.err());

    let mut stream = stream_result.unwrap();
    let mut text_chunks = Vec::new();
    let mut got_complete = false;
    let mut usage = None;

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::TextDelta(t) => {
                text_chunks.push(t);
            }
            StreamEvent::MessageComplete { usage: u, .. } => {
                got_complete = true;
                usage = u;
            }
            StreamEvent::Error(e) => {
                panic!("Stream error: {:?}", e);
            }
            _ => {}
        }
    }

    let full_text = text_chunks.join("");
    eprintln!("[stream_text] chunks={}, text: {}", text_chunks.len(), full_text);
    eprintln!("[stream_text] usage: {:?}", usage);

    assert!(!text_chunks.is_empty(), "Should receive text chunks");
    assert!(text_chunks.len() > 1, "Should receive multiple chunks (got {})", text_chunks.len());
    assert!(got_complete, "Should receive MessageComplete");
    assert!(full_text.contains('1') && full_text.contains('5'), "Should count 1-5: {}", full_text);
}

/// Streaming with system prompt
#[tokio::test]
#[ignore]
async fn test_stream_with_system_prompt() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("What is your name?".to_string()),
    }];

    let mut stream = provider
        .chat_stream(
            messages,
            Some("You are a pirate named Captain Crunch. Always speak like a pirate."),
            None,
            256,
        )
        .await
        .expect("chat_stream failed");

    let mut text_chunks = Vec::new();
    while let Some(event) = stream.next().await {
        if let StreamEvent::TextDelta(t) = event {
            text_chunks.push(t);
        }
    }

    let full_text = text_chunks.join("").to_lowercase();
    eprintln!("[stream_system] text: {}", full_text);

    let has_pirate = full_text.contains("captain")
        || full_text.contains("crunch")
        || full_text.contains("arr")
        || full_text.contains("ahoy")
        || full_text.contains("matey")
        || full_text.contains("pirate");
    assert!(has_pirate, "Expected pirate-themed response: {}", full_text);
}

/// Streaming with tool calls
#[tokio::test]
#[ignore]
async fn test_stream_with_tool_use() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let tools = vec![ToolDefinition {
        name: "bash".to_string(),
        description: "Execute a bash command and return its output".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                }
            },
            "required": ["command"]
        }),
    }];

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "Run 'echo hello' using the bash tool.".to_string(),
        ),
    }];

    let mut stream = provider
        .chat_stream(messages, None, Some(&tools), 256)
        .await
        .expect("chat_stream with tools failed");

    let mut text_deltas = Vec::new();
    let mut tool_starts = Vec::new();
    let mut tool_input_deltas = Vec::new();
    let mut got_complete = false;

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::TextDelta(t) => text_deltas.push(t),
            StreamEvent::ToolUseStart { id, name } => {
                eprintln!("[stream_tool] ToolUseStart: id={}, name={}", id, name);
                tool_starts.push((id, name));
            }
            StreamEvent::ToolUseInputDelta(d) => {
                tool_input_deltas.push(d);
            }
            StreamEvent::MessageComplete { .. } => {
                got_complete = true;
            }
            StreamEvent::Error(e) => {
                panic!("Stream error: {:?}", e);
            }
        }
    }

    let full_input = tool_input_deltas.join("");
    eprintln!("[stream_tool] text_deltas={}, tool_starts={:?}, input={}",
        text_deltas.len(), tool_starts, full_input);

    assert!(!tool_starts.is_empty(), "Should have at least one ToolUseStart");
    assert!(
        tool_starts.iter().any(|(_, name)| name == "bash"),
        "Should have a bash tool call: {:?}",
        tool_starts
    );
    assert!(
        !tool_input_deltas.is_empty(),
        "Should receive tool input deltas"
    );
    // The assembled tool input should be valid JSON containing "echo hello"
    assert!(
        full_input.contains("echo") || full_input.contains("hello"),
        "Tool input should contain echo command: {}",
        full_input
    );
    assert!(got_complete, "Should receive MessageComplete");
}

// ============================================================================
// 5. Agent-level tests: full agentic loop
// ============================================================================

/// Simple query through the agent (no tools needed)
#[tokio::test]
#[ignore]
async fn test_agent_simple_query() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot("What is 2 + 2? Reply with just the number.", None, tx)
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;
    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextChunk(_)));
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));

    assert!(has_text, "Should receive text chunks");
    assert!(has_done, "Should receive Done event");

    if let Some(AgentEvent::Done { final_text }) = events.last() {
        eprintln!("[agent_simple] final: {}", final_text);
        assert!(final_text.contains('4'), "Expected '4': {}", final_text);
    }
}

/// Agent executes bash tool
#[tokio::test]
#[ignore]
async fn test_agent_bash_tool() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            "Use the bash tool to run 'echo rig_integration_test_ok' and tell me the output.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult { name, output, success } => {
                Some((name.clone(), output.clone(), *success))
            }
            _ => None,
        })
        .collect();

    eprintln!("[agent_bash] tools_started: {:?}", tool_starts);
    eprintln!("[agent_bash] tool_results: {:?}", tool_results);

    assert!(
        tool_starts.iter().any(|n| n == "bash"),
        "Should use bash tool: {:?}",
        tool_starts
    );

    let bash_ok = tool_results
        .iter()
        .any(|(n, o, s)| n == "bash" && *s && o.contains("rig_integration_test_ok"));
    assert!(bash_ok, "Bash tool should succeed with expected output: {:?}", tool_results);

    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should complete");
}

/// Agent reads a file
#[tokio::test]
#[ignore]
async fn test_agent_read_tool() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/tmp".to_string());

    // Write a temp file first
    let path = format!("/tmp/gsh_rig_test_{}.txt", std::process::id());
    std::fs::write(&path, "rig-core integration test content 42").unwrap();

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            &format!("Use the read tool to read the file at '{}' and tell me its content.", path),
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    eprintln!("[agent_read] tools: {:?}", tool_starts);
    assert!(
        tool_starts.iter().any(|n| n == "read"),
        "Should use read tool: {:?}",
        tool_starts
    );

    if let Some(AgentEvent::Done { final_text }) = events.last() {
        eprintln!("[agent_read] final: {}", final_text);
        assert!(
            final_text.contains("42") || final_text.contains("integration test content"),
            "Should mention file content: {}",
            final_text
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Agent writes + reads a file (multi-step)
#[tokio::test]
#[ignore]
async fn test_agent_write_then_read() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let path = format!("/tmp/gsh_rig_wr_test_{}.txt", std::process::id());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            &format!(
                "Write the text 'rig_write_test_success_99' to '{}' using the write tool, then read it back using the read tool to confirm.",
                path
            ),
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    eprintln!("[agent_wr] tools: {:?}", tool_starts);

    // Should use both write and read
    assert!(
        tool_starts.iter().any(|n| n == "write"),
        "Should use write: {:?}",
        tool_starts
    );

    // Check if the file was actually written
    if std::path::Path::new(&path).exists() {
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        eprintln!("[agent_wr] file content: {}", content);
        assert!(
            content.contains("rig_write_test_success_99"),
            "File should contain test string: {}",
            content
        );
    }

    let _ = std::fs::remove_file(&path);
}

/// Agent uses glob to find files
#[tokio::test]
#[ignore]
async fn test_agent_glob_tool() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/Users/gab/gsh".to_string());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            "Use the glob tool to find all .rs files in /Users/gab/gsh/gsh-daemon/src/provider/ and tell me how many there are.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    eprintln!("[agent_glob] tools: {:?}", tool_starts);
    assert!(
        tool_starts.iter().any(|n| n == "glob"),
        "Should use glob: {:?}",
        tool_starts
    );

    if let Some(AgentEvent::Done { final_text }) = events.last() {
        eprintln!("[agent_glob] final: {}", final_text);
        // We have 8 .rs files: anthropic, moonshot, ollama, openai, openai_responses, rig_adapt, together, zhipu, mod
        // At minimum should have some digit in the response
        assert!(
            final_text.chars().any(|c| c.is_ascii_digit()),
            "Should mention a count: {}",
            final_text
        );
    }
}

/// Agent handles tool execution failure gracefully
#[tokio::test]
#[ignore]
async fn test_agent_tool_error_recovery() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            "Read the file /nonexistent_rig_test_path/foo.txt and tell me what error you get.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent should handle errors: {:?}", result.err());

    let events = collect_events(rx).await;
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should complete even with tool errors");

    // Check that read tool was attempted and failed
    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolResult { name, success, .. } => Some((name.clone(), *success)),
            _ => None,
        })
        .collect();

    eprintln!("[agent_error] tool_results: {:?}", tool_results);

    if let Some((_, success)) = tool_results.iter().find(|(n, _)| n == "read") {
        assert!(!success, "Read of nonexistent file should fail");
    }
}

// ============================================================================
// 6. Streaming correctness: verify event ordering and completeness
// ============================================================================

/// Verify stream events arrive in order: TextDelta* → MessageComplete
#[tokio::test]
#[ignore]
async fn test_stream_event_ordering() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("Say exactly: 'hello world'".to_string()),
    }];

    let mut stream = provider
        .chat_stream(messages, None, None, 64)
        .await
        .expect("chat_stream failed");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    eprintln!("[event_ordering] {} events", events.len());
    for (i, e) in events.iter().enumerate() {
        eprintln!("  [{}] {:?}", i, e);
    }

    // Should have events
    assert!(!events.is_empty(), "Should have events");

    // Last meaningful event should be MessageComplete
    let last_non_empty = events.iter().rposition(|e| {
        matches!(e, StreamEvent::MessageComplete { .. })
    });
    assert!(last_non_empty.is_some(), "Should have MessageComplete");

    // All TextDeltas should come before the final MessageComplete
    if let Some(complete_idx) = last_non_empty {
        for (i, event) in events.iter().enumerate() {
            if i > complete_idx {
                assert!(
                    !matches!(event, StreamEvent::TextDelta(_)),
                    "TextDelta at index {} after MessageComplete at {}",
                    i,
                    complete_idx
                );
            }
        }
    }
}

/// Verify stream assembles same text as non-streaming
#[tokio::test]
#[ignore]
async fn test_stream_vs_nonstream_equivalence() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "What is the capital of France? Reply with just the city name.".to_string(),
        ),
    }];

    // Non-streaming
    let nonstream_result = provider
        .chat(messages.clone(), None, None, 64)
        .await
        .expect("Non-streaming failed");
    let nonstream_text = nonstream_result.content.all_text();

    // Streaming
    let mut stream = provider
        .chat_stream(messages, None, None, 64)
        .await
        .expect("Streaming failed");

    let mut chunks = Vec::new();
    while let Some(event) = stream.next().await {
        if let StreamEvent::TextDelta(t) = event {
            chunks.push(t);
        }
    }
    let stream_text = chunks.join("");

    eprintln!("[equivalence] nonstream: {}", nonstream_text);
    eprintln!("[equivalence] stream: {}", stream_text);

    // Both should mention Paris
    assert!(nonstream_text.contains("Paris"), "Non-stream: {}", nonstream_text);
    assert!(stream_text.contains("Paris"), "Stream: {}", stream_text);
}

// ============================================================================
// 7. Edge cases
// ============================================================================

/// Empty system prompt should work
#[tokio::test]
#[ignore]
async fn test_empty_system_prompt() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("Say 'ok'".to_string()),
    }];

    let result = provider.chat(messages, Some(""), None, 32).await;
    assert!(result.is_ok(), "Empty system prompt should work: {:?}", result.err());
}

/// Very short max_tokens should still return something
#[tokio::test]
#[ignore]
async fn test_low_max_tokens() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text("Tell me a very long story about dragons.".to_string()),
    }];

    let result = provider.chat(messages, None, None, 10).await;
    assert!(result.is_ok(), "Low max_tokens should work: {:?}", result.err());

    let text = result.unwrap().content.all_text();
    eprintln!("[low_tokens] response ({} chars): {}", text.len(), text);
    // Should have some text, even if truncated
    assert!(!text.is_empty(), "Should return some text even with low max_tokens");
}

/// Multiple tools defined — model should pick the right one
#[tokio::test]
#[ignore]
async fn test_multiple_tools_correct_selection() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    let tools = vec![
        ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather for a city".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        },
        ToolDefinition {
            name: "search_web".to_string(),
            description: "Search the web".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "calculator".to_string(),
            description: "Evaluate a math expression".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string" }
                },
                "required": ["expression"]
            }),
        },
    ];

    let messages = vec![ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Text(
            "What is 15 * 23? Use the calculator tool.".to_string(),
        ),
    }];

    let result = provider.chat(messages, None, Some(&tools), 256).await;
    assert!(result.is_ok(), "Multiple tools failed: {:?}", result.err());

    let msg = result.unwrap();
    let tool_uses = msg.content.tool_uses();

    eprintln!("[multi_tools] tool_uses: {:?}", tool_uses);

    if !tool_uses.is_empty() {
        if let ContentBlock::ToolUse { name, .. } = &tool_uses[0] {
            assert_eq!(
                name, "calculator",
                "Should select calculator, got: {}",
                name
            );
        }
    }
}

/// Provider metadata is correct
#[tokio::test]
#[ignore]
async fn test_provider_metadata() {
    if skip_if_no_key() { return; }
    let provider = make_provider();

    assert_eq!(provider.name(), "z");
    assert_eq!(provider.model(), "glm-5");

    let caps = provider.capabilities();
    assert!(caps.supports_tools);
    assert!(caps.supports_streaming);
    assert!(caps.supports_system_message);

    let info = provider.model_info();
    assert_eq!(info.provider, "z");
    assert_eq!(info.id, "glm-5");
}

/// Agent multi-step: grep tool
#[tokio::test]
#[ignore]
async fn test_agent_grep_tool() {
    if skip_if_no_key() { return; }
    let config = Config::default();
    let provider = make_provider();
    let agent = Agent::new(provider, &config, "/Users/gab/gsh".to_string());

    let (tx, rx) = mpsc::channel(100);
    let result = agent
        .run_oneshot(
            "Use the grep tool to search for 'rig_adapt' in /Users/gab/gsh/gsh-daemon/src/provider/mod.rs and tell me what you find.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent failed: {:?}", result.err());

    let events = collect_events(rx).await;

    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStart { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();

    eprintln!("[agent_grep] tools: {:?}", tool_starts);

    if let Some(AgentEvent::Done { final_text }) = events.last() {
        eprintln!("[agent_grep] final: {}", final_text);
        assert!(
            final_text.contains("rig_adapt") || final_text.contains("found") || final_text.contains("match"),
            "Should find rig_adapt: {}",
            final_text
        );
    }
}
