//! Integration tests for the agentic loop with real LLM providers
//!
//! These tests require API keys to be set in environment variables.
//! They are ignored by default and can be run with:
//!
//!   cargo test --test agent_integration -- --ignored
//!
//! Required environment variables:
//! - ZAI_API_KEY: For Z/Zhipu GLM-4.7 tests
//! - TOGETHER_API_KEY: For Together.AI tests (Kimi K2.5)
//!
//! Note: Tool-use quality varies by model. Some tests may fail if the model
//! doesn't properly follow tool schemas.

use gsh_daemon::agent::{Agent, AgentEvent};
use gsh_daemon::config::Config;
use gsh_daemon::provider::create_provider;
use tokio::sync::mpsc;

/// Helper to check if Z API key is available
fn has_zai_api_key() -> bool {
    std::env::var("ZAI_API_KEY").is_ok()
}

/// Helper to check if Together API key is available
fn has_together_api_key() -> bool {
    std::env::var("TOGETHER_API_KEY").is_ok()
}

/// Collect all events from the agent until Done or Error
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

/// Test: Simple query without tool use
/// Verifies the agent can get a response from the Z provider
#[tokio::test]
#[ignore] // Requires ZAI_API_KEY
async fn test_z_provider_simple_query() {
    if !has_zai_api_key() {
        eprintln!("Skipping test: ZAI_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("z", &config).expect("Failed to create Z provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    let result = agent
        .run_oneshot("What is 2 + 2? Reply with just the number.", None, tx)
        .await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    // Should have at least some text chunks and a Done event
    let has_text = events.iter().any(|e| matches!(e, AgentEvent::TextChunk(_)));
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));

    assert!(has_text, "Should receive text chunks");
    assert!(has_done, "Should receive Done event");

    // The final text should contain "4"
    if let Some(AgentEvent::Done { final_text }) = events.last() {
        assert!(
            final_text.contains("4"),
            "Response should contain '4', got: {}",
            final_text
        );
    }
}

/// Test: Agentic flow with bash tool execution
/// Verifies the agent attempts to execute a bash command
/// Note: Success depends on the model's ability to follow tool schemas correctly
#[tokio::test]
#[ignore] // Requires ZAI_API_KEY
async fn test_z_provider_bash_tool_execution() {
    if !has_zai_api_key() {
        eprintln!("Skipping test: ZAI_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("z", &config).expect("Failed to create Z provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    // Ask the agent to run a command - this should trigger tool use
    let result = agent
        .run_oneshot(
            "Use the bash tool to run 'echo hello_from_gsh_test' and tell me what it outputs. The bash tool requires a 'command' parameter.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    // Check for tool execution events
    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolStart { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolResult { name, output, success } = e {
                Some((name.clone(), output.clone(), *success))
            } else {
                None
            }
        })
        .collect();

    // Should have attempted to use the bash tool
    assert!(
        tool_starts.iter().any(|n| n == "bash"),
        "Should have started bash tool, got: {:?}",
        tool_starts
    );

    // Check if any bash call succeeded
    let bash_succeeded = tool_results.iter().any(|(n, _, s)| n == "bash" && *s);

    if bash_succeeded {
        // The tool output should contain our test string
        let bash_output = tool_results
            .iter()
            .find(|(n, _, s)| n == "bash" && *s)
            .map(|(_, o, _)| o.clone())
            .unwrap_or_default();

        assert!(
            bash_output.contains("hello_from_gsh_test"),
            "Bash output should contain test string, got: {}",
            bash_output
        );
    } else {
        // Model didn't format tool call correctly, but at least it tried
        eprintln!(
            "Note: Model attempted bash tool but with incorrect format: {:?}",
            tool_results
        );
    }

    // Should have completed
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should complete with Done event");
}

/// Test: Multi-step agentic flow with file operations
/// Verifies the agent attempts to write and read files
#[tokio::test]
#[ignore] // Requires ZAI_API_KEY
async fn test_z_provider_file_operations() {
    if !has_zai_api_key() {
        eprintln!("Skipping test: ZAI_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("z", &config).expect("Failed to create Z provider");

    // Use a unique temp file
    let test_file = format!("/tmp/gsh_test_{}.txt", std::process::id());
    let test_content = "Hello from GSH integration test!";

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    // Ask the agent to write to a file - be explicit about parameters
    let query = format!(
        "Use the write tool to write the text '{}' to the file '{}'. The write tool requires 'path' and 'content' parameters.",
        test_content, test_file
    );

    let result = agent.run_oneshot(&query, None, tx).await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    // Check that write tool was attempted
    let tools_used: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolStart { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolResult { name, output, success } = e {
                Some((name.clone(), output.clone(), *success))
            } else {
                None
            }
        })
        .collect();

    // Should have attempted write tool
    assert!(
        tools_used.iter().any(|n| n == "write"),
        "Should have used write tool, got: {:?}",
        tools_used
    );

    // Check if write succeeded
    let write_succeeded = tool_results.iter().any(|(n, _, s)| n == "write" && *s);

    if write_succeeded {
        // Verify the file was actually created
        let file_content = std::fs::read_to_string(&test_file);
        assert!(
            file_content.is_ok(),
            "File should exist at {}",
            test_file
        );
        assert!(
            file_content.unwrap().contains(test_content),
            "File should contain test content"
        );
    } else {
        eprintln!(
            "Note: Model attempted write tool but with incorrect format: {:?}",
            tool_results
        );
    }

    // Should have completed
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should complete with Done event");

    // Cleanup
    let _ = std::fs::remove_file(&test_file);
}

/// Test: Error handling when tool execution fails
/// Verifies the agent gracefully handles tool failures
#[tokio::test]
#[ignore] // Requires ZAI_API_KEY
async fn test_z_provider_tool_error_handling() {
    if !has_zai_api_key() {
        eprintln!("Skipping test: ZAI_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("z", &config).expect("Failed to create Z provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    // Ask the agent to read a file that doesn't exist
    let result = agent
        .run_oneshot(
            "Use the read tool with path='/nonexistent/path/that/does/not/exist.txt' and tell me what happens. The read tool requires a 'path' parameter.",
            None,
            tx,
        )
        .await;

    // Should still complete (agent should handle the error gracefully)
    assert!(result.is_ok(), "Agent should complete even with tool errors: {:?}", result);

    let events = collect_events(rx).await;

    // Check for Done event
    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    let has_error = events.iter().any(|e| matches!(e, AgentEvent::Error(_)));

    // Either completed successfully or hit max iterations
    assert!(
        has_done || has_error,
        "Should complete with Done or Error event, got: {:?}",
        events.iter().map(|e| format!("{:?}", e)).collect::<Vec<_>>()
    );

    // Check tool results
    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolResult { name, output, success } = e {
                Some((name.clone(), output.clone(), *success))
            } else {
                None
            }
        })
        .collect();

    // If read tool was used, check if it reported an error
    if tool_results.iter().any(|(n, _, _)| n == "read") {
        let read_failed = tool_results.iter().any(|(n, _, s)| n == "read" && !s);
        if read_failed {
            eprintln!("Good: read tool correctly reported file not found");
        }
    }
}

/// Test: Glob tool for file discovery
#[tokio::test]
#[ignore] // Requires ZAI_API_KEY
async fn test_z_provider_glob_tool() {
    if !has_zai_api_key() {
        eprintln!("Skipping test: ZAI_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("z", &config).expect("Failed to create Z provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    // Ask the agent to find files
    let result = agent
        .run_oneshot(
            "Use the glob tool to find all .rs files in /Users/gab/gsh_claude/gsh-daemon/src/provider/ and count them.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    // Check that glob tool was used
    let tools_used: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolStart { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    assert!(
        tools_used.iter().any(|n| n == "glob"),
        "Should have used glob tool, got: {:?}",
        tools_used
    );

    // Final text should mention some count
    if let Some(AgentEvent::Done { final_text }) = events.last() {
        // We know there are multiple .rs files (anthropic.rs, openai.rs, etc.)
        let mentions_files = final_text.contains("file")
            || final_text.chars().any(|c| c.is_ascii_digit());

        assert!(
            mentions_files,
            "Response should mention files or counts: {}",
            final_text
        );
    }
}

// ============================================================================
// Together.AI (Kimi K2.5) Tests
// ============================================================================

/// Test: Simple query with Together.AI provider
#[tokio::test]
#[ignore] // Requires TOGETHER_API_KEY
async fn test_together_provider_simple_query() {
    if !has_together_api_key() {
        eprintln!("Skipping test: TOGETHER_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("together", &config).expect("Failed to create Together provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    let result = agent
        .run_oneshot("What is 2 + 2? Reply with just the number.", None, tx)
        .await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should receive Done event");

    if let Some(AgentEvent::Done { final_text }) = events.last() {
        assert!(
            final_text.contains("4"),
            "Response should contain '4', got: {}",
            final_text
        );
    }
}

/// Test: Together.AI with bash tool execution
#[tokio::test]
#[ignore] // Requires TOGETHER_API_KEY
async fn test_together_provider_bash_tool() {
    if !has_together_api_key() {
        eprintln!("Skipping test: TOGETHER_API_KEY not set");
        return;
    }

    let config = Config::default();
    let provider = create_provider("together", &config).expect("Failed to create Together provider");

    let agent = Agent::new(provider, &config, "/tmp".to_string());

    let (tx, rx) = mpsc::channel(100);

    let result = agent
        .run_oneshot(
            "Use the bash tool to run 'echo kimi_test_success' and tell me the output. The bash tool takes a 'command' parameter.",
            None,
            tx,
        )
        .await;

    assert!(result.is_ok(), "Agent should complete successfully: {:?}", result);

    let events = collect_events(rx).await;

    // Check for tool attempts
    let tool_starts: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolStart { name, .. } = e {
                Some(name.clone())
            } else {
                None
            }
        })
        .collect();

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| {
            if let AgentEvent::ToolResult { name, output, success } = e {
                Some((name.clone(), output.clone(), *success))
            } else {
                None
            }
        })
        .collect();

    // Should have attempted bash
    assert!(
        tool_starts.iter().any(|n| n == "bash"),
        "Should have started bash tool, got: {:?}",
        tool_starts
    );

    // Check results
    let bash_succeeded = tool_results.iter().any(|(n, o, s)| {
        n == "bash" && *s && o.contains("kimi_test_success")
    });

    if bash_succeeded {
        eprintln!("Success: Together/Kimi correctly executed bash command");
    } else {
        eprintln!(
            "Note: Together/Kimi attempted bash but results: {:?}",
            tool_results
        );
    }

    let has_done = events.iter().any(|e| matches!(e, AgentEvent::Done { .. }));
    assert!(has_done, "Should complete with Done event");
}
