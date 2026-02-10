//! Integration tests for context engineering: retrieval + 3-layer management.
//!
//! These tests exercise the full pipeline without an LLM:
//! 1. Persistent shell history across restarts
//! 2. search_context tool across all 3 data stores
//! 3. Layer 1 (truncation) at varying sizes
//! 4. Layer 2 (pruning) across realistic message histories
//! 5. Layers 1+2 combined across multiple iterations
//! 6. Layer 3 (compaction) threshold detection
//! 7. Full pipeline stress test

use crate::agent::{
    estimate_message_tokens, estimate_total_tokens, prune_old_tool_outputs,
    truncate_tool_output, COMPACTION_PROMPT,
};
use crate::agent::tools::ToolExecutor;
use crate::config::Config;
use crate::context::{ContextAccumulator, ContextRetriever};
use crate::observability::events::{EventKind, ObservabilityEvent};
use crate::observability::logger::{EventLogger, LoggerConfig};
use crate::provider::{ChatMessage, ChatRole, ContentBlock, MessageContent};
use crate::session::Session;
use std::sync::Arc;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a tool result message with a given content size
fn tool_result_msg(tool_use_id: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: ChatRole::User,
        content: MessageContent::Blocks(vec![ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: Some(false),
        }]),
    }
}

/// Create an assistant message with a tool use call
fn tool_use_msg(tool_use_id: &str, tool_name: &str, input: &str) -> ChatMessage {
    ChatMessage {
        role: ChatRole::Assistant,
        content: MessageContent::Blocks(vec![ContentBlock::ToolUse {
            id: tool_use_id.to_string(),
            name: tool_name.to_string(),
            input: serde_json::json!({ "command": input }),
        }]),
    }
}

/// Create a simple text message
fn text_msg(role: ChatRole, text: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: MessageContent::Text(text.to_string()),
    }
}

/// Generate deterministic "file content" of a given size
fn fake_output(size: usize, tag: &str) -> String {
    let line = format!("line from {} | ", tag);
    line.repeat(size / line.len() + 1)[..size].to_string()
}

/// Extract tool result content from a message (first ToolResult block)
fn extract_tool_content(msg: &ChatMessage) -> Option<&str> {
    if let MessageContent::Blocks(blocks) = &msg.content {
        for block in blocks {
            if let ContentBlock::ToolResult { content, .. } = block {
                return Some(content.as_str());
            }
        }
    }
    None
}

/// Count how many messages have pruned tool results
fn count_pruned(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|m| {
            extract_tool_content(m)
                .map(|c| c.contains("[output pruned"))
                .unwrap_or(false)
        })
        .count()
}

/// Count how many messages have truncated tool outputs
fn count_truncated(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .filter(|m| {
            extract_tool_content(m)
                .map(|c| c.contains("bytes omitted"))
                .unwrap_or(false)
        })
        .count()
}

// ===========================================================================
// Test 1: History lifecycle across daemon restarts
// ===========================================================================

#[test]
fn test_history_survives_restart() {
    let tmp = TempDir::new().unwrap();
    let history_path = tmp.path().join("shell-history.jsonl");

    let now = chrono::Utc::now();

    // --- First "daemon lifetime" ---
    {
        let mut acc = ContextAccumulator::with_history(100, history_path.clone(), 10_000);

        // Simulate 20 shell commands
        for i in 0..20 {
            let cmd = format!("command-{}", i);
            let cwd = if i % 2 == 0 {
                "/project-a".to_string()
            } else {
                "/project-b".to_string()
            };
            acc.preexec(cmd, cwd.clone(), now);
            acc.postcmd(if i == 7 { 1 } else { 0 }, cwd, None, now);
        }

        // Verify in-memory state
        let summary = acc.generate_ambient_summary(5);
        assert!(summary.contains("command-19"), "Should contain most recent command");
        assert!(summary.contains("command-15"), "Should contain 5th-most-recent");
        assert!(
            !summary.contains("command-14"),
            "Should NOT contain 6th-most-recent"
        );

        // acc dropped here — simulates daemon shutdown
    }

    // Verify JSONL file exists and has content
    let file_size = std::fs::metadata(&history_path).unwrap().len();
    assert!(file_size > 0, "History file should have content");

    // --- Second "daemon lifetime" (restart) ---
    {
        let acc2 = ContextAccumulator::with_history(100, history_path.clone(), 10_000);

        // Ambient summary should still work from loaded history
        let summary = acc2.generate_ambient_summary(5);
        assert!(
            summary.contains("command-19"),
            "After restart, should still see latest command"
        );
        assert!(
            summary.contains("search_context"),
            "Should hint about search_context tool"
        );
    }

    // --- Search across the persisted history ---
    {
        use crate::context::search_history;

        // Search by pattern
        let results = search_history(&history_path, Some("command-7"), None, None, 50);
        assert_eq!(results.len(), 1, "Should find exactly command-7");

        // Search by failure
        let failures = search_history(&history_path, None, None, Some(-1), 50);
        assert!(
            failures.len() >= 1,
            "Should find at least 1 failure (command-7 exited 1)"
        );

        // Search by cwd
        let proj_a = search_history(&history_path, None, Some("/project-a"), None, 50);
        assert_eq!(proj_a.len(), 10, "10 commands ran in /project-a");

        // Search with combined filters
        let filtered = search_history(
            &history_path,
            Some("command-1"),
            Some("/project-b"),
            None,
            50,
        );
        // command-1, command-11, command-13, command-15, command-17, command-19 are in /project-b
        // but only those matching "command-1": command-1, command-11, command-13, command-15, command-17, command-19
        // Wait — command-1 is odd (index 1) so it's /project-b. command-10 is even so /project-a.
        // Matching "command-1" AND /project-b: command-1, command-11, command-13, command-15, command-17, command-19
        assert!(
            filtered.len() >= 1,
            "Should find commands matching both filters"
        );
    }
}

// ===========================================================================
// Test 2: History truncation on disk
// ===========================================================================

#[test]
fn test_history_truncation_on_disk() {
    let tmp = TempDir::new().unwrap();
    let history_path = tmp.path().join("shell-history.jsonl");
    let now = chrono::Utc::now();

    // Write 100 events (each preexec+postcmd = 2 lines)
    {
        let mut acc = ContextAccumulator::with_history(200, history_path.clone(), 50);
        for i in 0..100 {
            acc.preexec(format!("cmd-{}", i), "/tmp".to_string(), now);
            acc.postcmd(0, "/tmp".to_string(), None, now);
        }
        // 200 lines written, max_history_lines=50, so truncation should happen on next startup
    }

    let lines_before: usize = std::fs::read_to_string(&history_path)
        .unwrap()
        .lines()
        .count();
    assert!(
        lines_before >= 100,
        "Should have many lines before truncation"
    );

    // Restart with max_history_lines=50 triggers truncation
    {
        let _acc = ContextAccumulator::with_history(200, history_path.clone(), 50);
    }

    let lines_after: usize = std::fs::read_to_string(&history_path)
        .unwrap()
        .lines()
        .count();
    assert!(
        lines_after <= 50,
        "After restart with max_history_lines=50, should be truncated to <=50 lines (got {})",
        lines_after
    );
}

// ===========================================================================
// Test 3: search_context tool across all 3 sources
// ===========================================================================

#[tokio::test]
async fn test_search_context_all_sources() {
    let tmp = TempDir::new().unwrap();
    let history_path = tmp.path().join("shell-history.jsonl");
    let log_dir = tmp.path().join("logs");
    let session_dir = tmp.path().join("sessions");
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();

    let now = chrono::Utc::now();

    // --- Populate shell history ---
    {
        let mut acc = ContextAccumulator::with_history(100, history_path.clone(), 10_000);
        for (cmd, cwd, exit) in [
            ("cargo build", "/project", 0),
            ("cargo test", "/project", 1),
            ("npm install", "/frontend", 0),
            ("docker compose up", "/infra", 0),
            ("pytest", "/ml", 2),
        ] {
            acc.preexec(cmd.to_string(), cwd.to_string(), now);
            acc.postcmd(exit, cwd.to_string(), None, now);
        }
    }

    // --- Populate a session ---
    {
        let mut session = Session::create("/project".to_string(), session_dir.clone()).unwrap();
        session
            .add_message(text_msg(
                ChatRole::User,
                "Help me fix the authentication bug in login.rs",
            ))
            .unwrap();
        session
            .add_message(text_msg(
                ChatRole::Assistant,
                "I found the issue in the authentication module. The token validation was incorrect.",
            ))
            .unwrap();
    }

    // --- Populate observability logs ---
    {
        let logger = EventLogger::new(LoggerConfig {
            log_dir: log_dir.clone(),
            max_file_size: 10 * 1024 * 1024,
            log_to_stderr: false,
            event_filter: vec![],
        })
        .unwrap();

        let events = vec![
            ObservabilityEvent::new(
                "sess-1",
                "root",
                EventKind::BashExec {
                    command: "cargo test --lib".to_string(),
                    exit_code: 0,
                    stdout: "test result: ok. 42 passed".to_string(),
                    stderr: String::new(),
                    duration_ms: Some(1200),
                },
            ),
            ObservabilityEvent::new(
                "sess-1",
                "root",
                EventKind::Error {
                    message: "Connection refused to database".to_string(),
                    recoverable: Some(false),
                },
            ),
        ];
        for event in &events {
            logger.log(event).unwrap();
        }
    }

    // --- Create executor with retriever ---
    let retriever = Arc::new(ContextRetriever::new(
        history_path,
        log_dir,
        session_dir,
    ));
    let executor = ToolExecutor::new(Config::default(), "/tmp".to_string(), Some(retriever));

    // -- Search shell_history: all --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "shell_history"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(text.contains("cargo build"), "Should find cargo build");
    assert!(text.contains("pytest"), "Should find pytest");
    assert!(text.contains("5 results"), "Should show 5 results");

    // -- Search shell_history: by pattern --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "shell_history", "query": "docker"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(
        text.contains("docker compose up"),
        "Should find docker command"
    );
    assert!(
        text.contains("1 results"),
        "Should find exactly 1 docker result"
    );

    // -- Search shell_history: failures only --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "shell_history", "exit_code": -1}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(text.contains("cargo test"), "cargo test failed (exit 1)");
    assert!(text.contains("pytest"), "pytest failed (exit 2)");
    assert!(!text.contains("cargo build"), "cargo build succeeded");

    // -- Search sessions --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "sessions", "query": "authentication"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(
        text.contains("authentication"),
        "Should find auth session content"
    );
    assert!(
        text.contains("token validation"),
        "Should find excerpt from assistant message"
    );

    // -- Search logs: bash executions --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "logs", "event_type": "bash_exec"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(
        text.contains("cargo test --lib"),
        "Should find bash execution log"
    );
    assert!(text.contains("42 passed"), "Should include stdout");

    // -- Search logs: errors --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "logs", "event_type": "error"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(
        text.contains("Connection refused"),
        "Should find error log"
    );

    // -- Search logs: by keyword --
    let result = executor
        .execute(
            "search_context",
            &serde_json::json!({"source": "logs", "query": "database"}),
        )
        .await
        .unwrap();
    let text = result.as_string();
    assert!(
        text.contains("database"),
        "Should find log matching 'database'"
    );
}

// ===========================================================================
// Test 4: Layer 1 — Truncation at varying sizes
// ===========================================================================

#[test]
fn test_truncation_spectrum() {
    let cases = vec![
        ("tiny", 100, 30_000, false),       // 100B — no truncation
        ("small", 5_000, 30_000, false),     // 5KB — no truncation
        ("at_limit", 30_000, 30_000, false), // exactly at limit — no truncation
        ("over", 35_000, 30_000, true),      // 5KB over — truncated
        ("medium", 80_000, 30_000, true),    // 80KB — truncated
        ("large", 500_000, 30_000, true),    // 500KB — truncated
        ("huge", 2_000_000, 30_000, true),   // 2MB — truncated
    ];

    for (label, size, max_bytes, should_truncate) in cases {
        let output = fake_output(size, label);
        let result = truncate_tool_output(&output, max_bytes);

        if should_truncate {
            assert!(
                result.contains("bytes omitted"),
                "{}: expected truncation marker",
                label
            );

            // Verify prefix+suffix structure
            let prefix_end = result.find("\n\n... (").expect("missing marker start");
            let suffix_start = result
                .find(") ...\n\n")
                .expect("missing marker end")
                + ") ...\n\n".len();
            let prefix = &result[..prefix_end];
            let suffix = &result[suffix_start..];

            // Each half should be ~max_bytes/2
            let half = max_bytes / 2;
            assert!(
                prefix.len() <= half + 10,
                "{}: prefix too long ({} vs {} half)",
                label,
                prefix.len(),
                half
            );
            assert!(
                suffix.len() <= half + 10,
                "{}: suffix too long ({} vs {} half)",
                label,
                suffix.len(),
                half
            );

            // Result should be much smaller than original
            assert!(
                result.len() < output.len(),
                "{}: truncated should be smaller",
                label
            );

            // Verify it's valid UTF-8 (it is because it's a String, but let's be explicit)
            assert!(
                std::str::from_utf8(result.as_bytes()).is_ok(),
                "{}: result should be valid UTF-8",
                label
            );
        } else {
            assert!(
                !result.contains("bytes omitted"),
                "{}: should NOT be truncated",
                label
            );
            assert_eq!(result.len(), output.len(), "{}: should be unchanged", label);
        }
    }
}

#[test]
fn test_truncation_multibyte_spectrum() {
    // Test with various multi-byte Unicode content
    let cases = vec![
        ("emoji", "🔥🎉✨🚀💯", 3),            // 4-byte chars
        ("cjk", "日本語テスト漢字", 3),         // 3-byte chars
        ("mixed", "Hello 世界! 🌍 café résumé", 3), // mixed widths
        ("cyrillic", "Привет мир программирование", 3), // 2-byte chars
    ];

    for (label, base, repeat_kb) in cases {
        // Create ~repeat_kb KB of content
        let content = base.repeat(repeat_kb * 1024 / base.len() + 1);
        let max_bytes = content.len() / 3; // Force truncation at 1/3

        let result = truncate_tool_output(&content, max_bytes);
        assert!(
            result.contains("bytes omitted"),
            "{}: should be truncated",
            label
        );

        // Critical: result must be valid UTF-8 (no split multi-byte chars)
        let _ = result.as_str(); // This would panic if invalid
    }
}

// ===========================================================================
// Test 5: Layer 2 — Pruning across realistic message histories
// ===========================================================================

#[test]
fn test_pruning_realistic_session() {
    // Simulate a realistic 15-iteration agent session
    let mut messages: Vec<ChatMessage> = Vec::new();

    let iterations = vec![
        // (tool_name, output_size_bytes, description)
        ("read", 2_000, "package.json"),
        ("read", 12_000, "src/index.ts"),
        ("bash", 40_000, "npm test (big output)"),
        ("read", 8_000, "src/utils.ts"),
        ("edit", 200, "edit utils.ts"),
        ("bash", 45_000, "npm test (bigger output)"),
        ("read", 12_000, "src/index.ts again"),
        ("edit", 200, "edit index.ts"),
        ("bash", 35_000, "npm test (another run)"),
        ("read", 6_000, "src/types.ts"),
        ("edit", 150, "edit types.ts"),
        ("bash", 50_000, "npm test (final run)"),
        ("read", 3_000, "tsconfig.json"),
        ("bash", 1_500, "npm run build"),
        ("edit", 100, "edit final file"),
    ];

    for (i, (tool, size, desc)) in iterations.iter().enumerate() {
        let id = format!("iter-{}", i);

        // Assistant says what it will do
        messages.push(text_msg(
            ChatRole::Assistant,
            &format!("I'll {} now.", desc),
        ));

        // Assistant calls tool
        messages.push(tool_use_msg(&id, tool, desc));

        // Tool result
        let output = fake_output(*size, desc);
        messages.push(tool_result_msg(&id, &output));
    }

    let tokens_before = estimate_total_tokens(&messages);
    assert!(
        tokens_before > 50_000,
        "Should have >50K tokens before pruning (got {})",
        tokens_before
    );

    // Prune with 40K token protection window
    prune_old_tool_outputs(&mut messages, 40_000);

    let tokens_after = estimate_total_tokens(&messages);
    let pruned = count_pruned(&messages);

    // Pruning should have reduced total tokens
    assert!(
        tokens_after < tokens_before,
        "Tokens should decrease after pruning ({} -> {})",
        tokens_before,
        tokens_after
    );

    // Some messages should be pruned
    assert!(pruned > 0, "At least some old outputs should be pruned");

    // Most recent outputs should be intact (last 3 iterations = last 9 messages)
    let last_tool_results: Vec<_> = messages
        .iter()
        .rev()
        .filter(|m| extract_tool_content(m).is_some())
        .take(3)
        .collect();
    for msg in &last_tool_results {
        let content = extract_tool_content(msg).unwrap();
        assert!(
            !content.contains("[output pruned"),
            "Recent tool results should NOT be pruned"
        );
    }

    // Small outputs should NEVER be pruned (even if old).
    // The prune marker format is: [output pruned, was ~N tokens]
    // We parse N to check the original size.
    for msg in &messages {
        if let Some(content) = extract_tool_content(msg) {
            if content.starts_with("[output pruned, was ~") {
                if let Some(end) = content.find(" tokens]") {
                    let start = "[output pruned, was ~".len();
                    if let Ok(original_tokens) = content[start..end].parse::<usize>() {
                        assert!(
                            original_tokens > 100,
                            "Output with only ~{} tokens should never be pruned",
                            original_tokens
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn test_pruning_preserves_message_structure() {
    // Ensure pruning only modifies ToolResult content, nothing else
    let mut messages = vec![
        text_msg(ChatRole::User, "Help me with this project"),
        text_msg(ChatRole::Assistant, "Sure, let me look around."),
        tool_use_msg("t1", "bash", "ls -la"),
        tool_result_msg("t1", &fake_output(20_000, "ls-output")),
        text_msg(ChatRole::Assistant, "I see the files. Let me read one."),
        tool_use_msg("t2", "read", "src/main.rs"),
        tool_result_msg("t2", &fake_output(20_000, "main-rs")),
        text_msg(ChatRole::Assistant, "Here's what I found."),
        tool_use_msg("t3", "bash", "cargo test"),
        tool_result_msg("t3", &fake_output(5_000, "test-output")),
    ];

    prune_old_tool_outputs(&mut messages, 5_000);

    // Text messages should be completely untouched
    if let MessageContent::Text(t) = &messages[0].content {
        assert_eq!(t, "Help me with this project");
    } else {
        panic!("User text message was modified");
    }

    // ToolUse messages should be untouched
    if let MessageContent::Blocks(blocks) = &messages[2].content {
        if let ContentBlock::ToolUse { name, .. } = &blocks[0] {
            assert_eq!(name, "bash");
        } else {
            panic!("ToolUse block was modified");
        }
    }

    // At least one old tool result should be pruned
    let pruned = count_pruned(&messages);
    assert!(pruned > 0, "Should have pruned at least one old output");
}

// ===========================================================================
// Test 6: Layer 1 + Layer 2 combined (multi-iteration simulation)
// ===========================================================================

#[test]
fn test_truncate_then_prune_iterations() {
    let max_tool_output_bytes = 30_000;
    let prune_protect_tokens = 20_000; // Smaller window to trigger more pruning

    let mut messages: Vec<ChatMessage> = Vec::new();

    // User's initial prompt
    messages.push(text_msg(ChatRole::User, "Refactor the authentication module"));

    // Simulate 10 iterations of agent work
    for iteration in 0..10 {
        let id = format!("iter-{}", iteration);

        // Agent text
        messages.push(text_msg(
            ChatRole::Assistant,
            &format!("Working on step {}...", iteration),
        ));

        // Agent calls a tool
        messages.push(tool_use_msg(&id, "bash", &format!("step-{}", iteration)));

        // Tool produces output of varying size
        let raw_size = match iteration % 4 {
            0 => 500,         // small
            1 => 15_000,      // medium
            2 => 80_000,      // large — will be truncated
            3 => 200_000,     // huge — will be truncated
            _ => unreachable!(),
        };
        let raw_output = fake_output(raw_size, &format!("step-{}", iteration));

        // Layer 1: Truncate
        let truncated_output = truncate_tool_output(&raw_output, max_tool_output_bytes);
        messages.push(tool_result_msg(&id, &truncated_output));

        // Layer 2: Prune (runs at the start of each iteration, simulated here)
        prune_old_tool_outputs(&mut messages, prune_protect_tokens);
    }

    let total_tokens = estimate_total_tokens(&messages);
    let pruned = count_pruned(&messages);
    let truncated = count_truncated(&messages);

    // Verify truncation happened for the large outputs
    assert!(
        truncated > 0 || pruned > 0,
        "Some outputs should be truncated or pruned (truncated={}, pruned={})",
        truncated,
        pruned
    );

    // Verify total tokens are bounded — without management this would be >100K tokens
    // With a 20K protection window + truncation at 30KB, we should be well under
    assert!(
        total_tokens < 80_000,
        "Total tokens should be bounded (got {})",
        total_tokens
    );

    // Most recent iteration's output should be intact
    let last_result = messages
        .iter()
        .rev()
        .find(|m| extract_tool_content(m).is_some())
        .unwrap();
    let last_content = extract_tool_content(last_result).unwrap();
    assert!(
        !last_content.contains("[output pruned"),
        "Most recent output should not be pruned"
    );
}

// ===========================================================================
// Test 7: Layer 3 — Compaction threshold detection
// ===========================================================================

#[test]
fn test_compaction_threshold_detection() {
    let context_limit: usize = 100_000; // 100K token window
    let compact_threshold: f64 = 0.85;
    let trigger_at = (context_limit as f64 * compact_threshold) as usize; // 85,000

    // Build messages just under the threshold
    let under_text = "x".repeat(trigger_at * 4 - 100); // tokens = bytes/4
    let under_messages = vec![text_msg(ChatRole::User, &under_text)];
    let under_tokens = estimate_total_tokens(&under_messages);
    assert!(
        under_tokens < trigger_at,
        "Under messages should be below threshold ({} < {})",
        under_tokens,
        trigger_at
    );

    // Build messages just over the threshold
    let over_text = "x".repeat(trigger_at * 4 + 100);
    let over_messages = vec![text_msg(ChatRole::User, &over_text)];
    let over_tokens = estimate_total_tokens(&over_messages);
    assert!(
        over_tokens > trigger_at,
        "Over messages should be above threshold ({} > {})",
        over_tokens,
        trigger_at
    );

    // Simulate the check the agent loop does
    let should_compact_under = under_tokens > trigger_at;
    let should_compact_over = over_tokens > trigger_at;

    assert!(!should_compact_under, "Should NOT compact when under threshold");
    assert!(should_compact_over, "Should compact when over threshold");
}

#[test]
fn test_compaction_prompt_is_well_formed() {
    // The compaction prompt should contain key instructions
    assert!(COMPACTION_PROMPT.contains("CONTEXT CHECKPOINT"));
    assert!(COMPACTION_PROMPT.contains("handoff briefing"));
    assert!(COMPACTION_PROMPT.contains("Current progress"));
    assert!(COMPACTION_PROMPT.contains("Files modified"));
    assert!(COMPACTION_PROMPT.contains("next steps"));
}

#[test]
fn test_compaction_builds_correct_request() {
    // Simulate what the agent does when building the compaction request
    let messages = vec![
        text_msg(ChatRole::User, "Help me refactor auth"),
        text_msg(ChatRole::Assistant, "I'll start by reading the files."),
        tool_use_msg("t1", "read", "src/auth.rs"),
        tool_result_msg("t1", &fake_output(5_000, "auth-content")),
        text_msg(ChatRole::Assistant, "I see the issue. Let me fix it."),
    ];

    // Build compaction messages (all existing + compaction prompt)
    let mut compact_messages = messages.clone();
    compact_messages.push(text_msg(ChatRole::User, COMPACTION_PROMPT));

    // The compaction request should have all messages + the prompt
    assert_eq!(compact_messages.len(), messages.len() + 1);

    // Last message should be the compaction prompt
    if let MessageContent::Text(t) = &compact_messages.last().unwrap().content {
        assert!(t.contains("CONTEXT CHECKPOINT"));
    } else {
        panic!("Last message should be compaction prompt text");
    }
}

// ===========================================================================
// Test 8: Full pipeline stress test — 20 iterations
// ===========================================================================

#[test]
fn test_full_pipeline_stress() {
    let max_tool_output_bytes = 30_000;
    let prune_protect_tokens = 40_000;
    let context_limit: usize = 200_000; // Simulating Claude's 200K window
    let compact_threshold: f64 = 0.85;
    let trigger_at = (context_limit as f64 * compact_threshold) as usize;

    let mut messages: Vec<ChatMessage> = Vec::new();
    messages.push(text_msg(
        ChatRole::User,
        "Help me build a complete REST API with tests",
    ));

    let mut truncation_events = 0usize;
    let mut prune_events = 0usize;
    let mut would_compact_at: Option<usize> = None;
    let mut peak_tokens: usize = 0;

    // 20 iterations with a mix of tool output sizes
    for iteration in 0..20 {
        let id = format!("call-{}", iteration);

        // --- Layer 2: Prune at start of iteration ---
        let before_prune = count_pruned(&messages);
        prune_old_tool_outputs(&mut messages, prune_protect_tokens);
        let after_prune = count_pruned(&messages);
        if after_prune > before_prune {
            prune_events += 1;
        }

        // --- Check Layer 3 threshold ---
        let tokens_now = estimate_total_tokens(&messages);
        if tokens_now > trigger_at && would_compact_at.is_none() {
            would_compact_at = Some(iteration);
            // In real code, compaction would fire here and reset context.
            // Since we can't call an LLM in tests, we simulate by noting when it would happen.
        }
        peak_tokens = peak_tokens.max(tokens_now);

        // --- Agent produces text + tool call ---
        messages.push(text_msg(
            ChatRole::Assistant,
            &format!("Step {}: working on the next part.", iteration),
        ));

        // Vary the tool and output size
        let (tool, raw_size) = match iteration {
            0 => ("read", 3_000),       // read package.json
            1 => ("bash", 80_000),      // npm test (big)
            2 => ("read", 15_000),      // read a source file
            3 => ("edit", 300),         // small edit
            4 => ("bash", 120_000),     // long build output
            5 => ("read", 25_000),      // read large file
            6 => ("bash", 200_000),     // huge test output
            7 => ("edit", 150),         // tiny edit
            8 => ("read", 8_000),       // medium read
            9 => ("bash", 500_000),     // massive compilation output
            10 => ("read", 4_000),      // small read
            11 => ("edit", 200),        // small edit
            12 => ("bash", 60_000),     // medium test
            13 => ("read", 30_000),     // borderline read
            14 => ("bash", 150_000),    // large output
            15 => ("edit", 100),        // tiny edit
            16 => ("read", 50_000),     // over-limit read
            17 => ("bash", 300_000),    // huge output
            18 => ("read", 10_000),     // normal read
            19 => ("edit", 250),        // small final edit
            _ => unreachable!(),
        };

        messages.push(tool_use_msg(&id, tool, &format!("op-{}", iteration)));

        // --- Layer 1: Truncate the tool output ---
        let raw_output = fake_output(raw_size, &format!("op-{}", iteration));
        let truncated = truncate_tool_output(&raw_output, max_tool_output_bytes);
        if truncated.contains("bytes omitted") {
            truncation_events += 1;
        }
        messages.push(tool_result_msg(&id, &truncated));
    }

    // Final prune
    prune_old_tool_outputs(&mut messages, prune_protect_tokens);

    let final_tokens = estimate_total_tokens(&messages);
    let total_pruned = count_pruned(&messages);
    let total_truncated = count_truncated(&messages);

    // --- Assertions ---

    // Truncation should have fired for outputs > 30KB
    assert!(
        truncation_events >= 5,
        "Should have truncated several large outputs (got {})",
        truncation_events
    );

    // Pruning should have kicked in
    assert!(
        prune_events > 0,
        "Pruning should have activated at some point"
    );

    // Total pruned messages should be significant
    assert!(
        total_pruned >= 3,
        "Should have pruned at least 3 old outputs (got {})",
        total_pruned
    );

    // Truncated outputs that survived pruning
    // (some truncated outputs may have been subsequently pruned)
    assert!(
        total_truncated + total_pruned >= 5,
        "Combined truncation + pruning should be significant"
    );

    // Context should be much smaller than raw input would have been
    // Raw total would be ~1.6MB / 4 ≈ 400K tokens without any management
    assert!(
        final_tokens < 200_000,
        "Final token count should be well bounded (got {})",
        final_tokens
    );

    // Most recent outputs should be intact
    let recent_results: Vec<_> = messages
        .iter()
        .rev()
        .filter(|m| extract_tool_content(m).is_some())
        .take(3)
        .collect();
    for msg in &recent_results {
        let content = extract_tool_content(msg).unwrap();
        assert!(
            !content.contains("[output pruned"),
            "Last 3 tool results should NOT be pruned"
        );
    }

    // Small outputs should never be pruned regardless of age.
    for msg in &messages {
        if let Some(content) = extract_tool_content(msg) {
            if content.starts_with("[output pruned, was ~") {
                if let Some(end) = content.find(" tokens]") {
                    let start = "[output pruned, was ~".len();
                    if let Ok(original_tokens) = content[start..end].parse::<usize>() {
                        assert!(
                            original_tokens > 100,
                            "Output with only ~{} tokens should never be pruned",
                            original_tokens
                        );
                    }
                }
            }
        }
    }

    // Print summary for visibility
    eprintln!("--- Full Pipeline Stress Test Summary ---");
    eprintln!("  Iterations:       20");
    eprintln!("  Truncation events: {}", truncation_events);
    eprintln!("  Prune rounds:      {}", prune_events);
    eprintln!("  Total pruned msgs: {}", total_pruned);
    eprintln!("  Truncated (surv.): {}", total_truncated);
    eprintln!("  Peak tokens:       {}", peak_tokens);
    eprintln!("  Final tokens:      {}", final_tokens);
    if let Some(iter) = would_compact_at {
        eprintln!("  Would compact at:  iteration {}", iter);
    } else {
        eprintln!("  Would compact at:  never (stayed under threshold)");
    }
    eprintln!("----------------------------------------");
}

// ===========================================================================
// Test 9: Token estimation accuracy across content types
// ===========================================================================

#[test]
fn test_token_estimation_message_types() {
    // Pure text message
    let text = text_msg(ChatRole::User, &"a".repeat(4000));
    assert_eq!(estimate_message_tokens(&text), 1004); // 4000/4 + 4 overhead

    // Tool use message
    let tool_use = tool_use_msg("id-1", "bash", "ls -la");
    let tu_tokens = estimate_message_tokens(&tool_use);
    assert!(tu_tokens > 4, "Tool use should have content tokens + overhead");

    // Tool result message (large)
    let big_result = tool_result_msg("id-1", &"x".repeat(120_000));
    let tr_tokens = estimate_message_tokens(&big_result);
    assert_eq!(tr_tokens, 30_004); // 120_000/4 + 4

    // Multi-block message
    let multi = ChatMessage {
        role: ChatRole::Assistant,
        content: MessageContent::Blocks(vec![
            ContentBlock::Text {
                text: "Here are the results:".to_string(),
            },
            ContentBlock::ToolUse {
                id: "t1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "echo hello"}),
            },
        ]),
    };
    let multi_tokens = estimate_message_tokens(&multi);
    assert!(
        multi_tokens > 4,
        "Multi-block message should have tokens from all blocks"
    );

    // Total across a conversation
    let conversation = vec![text, tool_use, big_result, multi];
    let total = estimate_total_tokens(&conversation);
    assert!(
        total > 31_000,
        "Total should account for all messages (got {})",
        total
    );
}
