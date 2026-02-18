use anyhow::Result;
use std::path::PathBuf;

use crate::context::accumulator::{format_event, search_history};
use crate::observability::events::{EventKind, ObservabilityEvent};
use crate::observability::logger;
use crate::session;

/// Read-only query interface over all context data stores
pub struct ContextRetriever {
    /// Path to shell-history.jsonl
    shell_history_path: PathBuf,
    /// Path to observability log directory
    log_dir: PathBuf,
    /// Path to session directory
    session_dir: PathBuf,
}

impl ContextRetriever {
    pub fn new(shell_history_path: PathBuf, log_dir: PathBuf, session_dir: PathBuf) -> Self {
        Self {
            shell_history_path,
            log_dir,
            session_dir,
        }
    }

    /// Search shell command history
    pub fn search_shell_history(
        &self,
        pattern: Option<&str>,
        cwd: Option<&str>,
        exit_code: Option<i32>,
        last_n: usize,
    ) -> Result<String> {
        let last_n = last_n.min(50);
        let events = search_history(
            &self.shell_history_path,
            pattern,
            cwd,
            exit_code,
            last_n,
        );

        if events.is_empty() {
            return Ok("No matching shell history found.".to_string());
        }

        let mut output = format!("# Shell History ({} results)\n\n", events.len());
        for event in &events {
            output.push_str(&format_event(event));
        }
        Ok(output)
    }

    /// List recent sessions (no keyword filter)
    pub fn list_recent_sessions(&self, limit: usize) -> Result<String> {
        let limit = limit.min(10);
        let sessions = session::list_sessions(&self.session_dir)?;

        if sessions.is_empty() {
            return Ok("No saved sessions found.".to_string());
        }

        let mut output = format!("# Recent Sessions ({} total)\n\n", sessions.len());
        for info in sessions.iter().take(limit) {
            let title = info.title.as_deref().unwrap_or("(untitled)");
            let date = info.last_activity.format("%Y-%m-%d %H:%M");
            output.push_str(&format!("- {} ({}) | {} msgs | {}\n", title, date, info.message_count, info.cwd));
        }
        Ok(output)
    }

    /// Search past agent sessions by keyword
    pub fn search_sessions(
        &self,
        keyword: &str,
        limit: usize,
    ) -> Result<String> {
        let limit = limit.min(10);
        let keyword_lower = keyword.to_lowercase();

        let sessions = session::list_sessions(&self.session_dir)?;
        let mut matches = Vec::new();

        for info in sessions {
            // Load session to search messages
            let session = match session::Session::load(&info.id, self.session_dir.clone()) {
                Ok(s) => s,
                Err(_) => continue,
            };

            // Check title
            let title_match = info.title.as_ref()
                .map(|t| t.to_lowercase().contains(&keyword_lower))
                .unwrap_or(false);

            // Search through messages for keyword
            let mut matching_excerpts = Vec::new();
            for msg in &session.messages {
                let text = match &msg.content {
                    crate::provider::MessageContent::Text(t) => t.clone(),
                    crate::provider::MessageContent::Blocks(blocks) => {
                        blocks.iter().filter_map(|b| match b {
                            crate::provider::ContentBlock::Text { text } => Some(text.as_str()),
                            crate::provider::ContentBlock::ToolResult { content, .. } => Some(content.as_str()),
                            _ => None,
                        }).collect::<Vec<_>>().join(" ")
                    }
                };

                if text.to_lowercase().contains(&keyword_lower) {
                    // Extract a short excerpt around the match
                    let excerpt = extract_excerpt(&text, &keyword_lower, 150);
                    matching_excerpts.push(excerpt);
                    if matching_excerpts.len() >= 3 {
                        break; // Cap excerpts per session
                    }
                }
            }

            if title_match || !matching_excerpts.is_empty() {
                matches.push((info, matching_excerpts));
                if matches.len() >= limit {
                    break;
                }
            }
        }

        if matches.is_empty() {
            return Ok(format!("No sessions matching \"{}\" found.", keyword));
        }

        let mut output = format!("# Session Search: \"{}\" ({} results)\n\n", keyword, matches.len());
        for (info, excerpts) in &matches {
            let title = info.title.as_deref().unwrap_or("(untitled)");
            let date = info.last_activity.format("%Y-%m-%d %H:%M");
            output.push_str(&format!("## {} ({})\n", title, date));
            output.push_str(&format!("Session: {} | {} messages | cwd: {}\n", &info.id[..8], info.message_count, info.cwd));
            for excerpt in excerpts {
                output.push_str(&format!("  > {}\n", excerpt));
            }
            output.push('\n');
        }
        Ok(output)
    }

    /// Search observability logs
    pub fn search_logs(
        &self,
        pattern: Option<&str>,
        event_type: Option<&str>,
        session_id: Option<&str>,
        last_n: usize,
    ) -> Result<String> {
        let last_n = last_n.min(50);
        let pattern_lower = pattern.map(|p| p.to_lowercase());

        let log_files = logger::list_log_files(&self.log_dir)?;
        let mut all_matches = Vec::new();

        // Read log files from newest to oldest
        for log_file in log_files.iter().rev() {
            let events = match logger::read_events(log_file) {
                Ok(e) => e,
                Err(_) => continue,
            };

            for event in events.into_iter().rev() {
                if !matches_event_filter(&event, event_type, session_id, pattern_lower.as_deref()) {
                    continue;
                }
                all_matches.push(event);
                if all_matches.len() >= last_n {
                    break;
                }
            }

            if all_matches.len() >= last_n {
                break;
            }
        }

        // Reverse to get chronological order
        all_matches.reverse();

        if all_matches.is_empty() {
            return Ok("No matching log events found.".to_string());
        }

        let mut output = format!("# Log Search ({} results)\n\n", all_matches.len());
        for event in &all_matches {
            output.push_str(&format_log_event(event));
        }
        Ok(output)
    }
}

/// Check if an observability event matches the given filters
fn matches_event_filter(
    event: &ObservabilityEvent,
    event_type: Option<&str>,
    session_id: Option<&str>,
    pattern: Option<&str>,
) -> bool {
    // Session ID filter
    if let Some(sid) = session_id {
        if !event.session.starts_with(sid) {
            return false;
        }
    }

    // Event type filter
    if let Some(et) = event_type {
        let matches_type = match (&event.event, et) {
            (EventKind::BashExec { .. }, "bash_exec") => true,
            (EventKind::ToolCall { .. }, "tool_call") => true,
            (EventKind::ToolResult { .. }, "tool_result") => true,
            (EventKind::Prompt { .. }, "prompt") => true,
            (EventKind::Error { .. }, "error") => true,
            _ => false,
        };
        if !matches_type {
            return false;
        }
    } else {
        // Without event_type filter, only show interesting events
        match &event.event {
            EventKind::BashExec { .. }
            | EventKind::ToolCall { .. }
            | EventKind::ToolResult { .. }
            | EventKind::Prompt { .. }
            | EventKind::Error { .. } => {}
            _ => return false,
        }
    }

    // Pattern filter
    if let Some(p) = pattern {
        let matches_pattern = match &event.event {
            EventKind::BashExec { command, stdout, stderr, .. } => {
                command.to_lowercase().contains(p)
                    || stdout.to_lowercase().contains(p)
                    || stderr.to_lowercase().contains(p)
            }
            EventKind::ToolCall { tool, input, .. } => {
                tool.to_lowercase().contains(p)
                    || input.to_string().to_lowercase().contains(p)
            }
            EventKind::ToolResult { tool, output, .. } => {
                tool.to_lowercase().contains(p)
                    || output.to_lowercase().contains(p)
            }
            EventKind::Prompt { content } => content.to_lowercase().contains(p),
            EventKind::Error { message, .. } => message.to_lowercase().contains(p),
            _ => false,
        };
        if !matches_pattern {
            return false;
        }
    }

    true
}

/// Format a log event for display
fn format_log_event(event: &ObservabilityEvent) -> String {
    let time = event.ts.format("%Y-%m-%d %H:%M:%S");
    match &event.event {
        EventKind::BashExec { command, exit_code, stdout, stderr, duration_ms, .. } => {
            let status = if *exit_code == 0 { "OK" } else { &format!("exit {}", exit_code) };
            let duration = duration_ms.map(|d| format!(" ({}ms)", d)).unwrap_or_default();
            let out_preview = truncate_str(stdout, 200);
            let err_preview = if stderr.is_empty() { String::new() } else {
                format!("\n  stderr: {}", truncate_str(stderr, 100))
            };
            format!("[{}] bash: `{}` -> {}{}\n  output: {}{}\n\n",
                time, truncate_str(command, 80), status, duration, out_preview, err_preview)
        }
        EventKind::ToolCall { tool, input, .. } => {
            format!("[{}] tool_call: {} {}\n\n", time, tool, truncate_str(&input.to_string(), 120))
        }
        EventKind::ToolResult { tool, output, success, .. } => {
            let status = if *success { "OK" } else { "FAIL" };
            format!("[{}] tool_result: {} [{}] {}\n\n", time, tool, status, truncate_str(output, 200))
        }
        EventKind::Prompt { content } => {
            format!("[{}] prompt: {}\n\n", time, truncate_str(content, 150))
        }
        EventKind::Error { message, .. } => {
            format!("[{}] error: {}\n\n", time, truncate_str(message, 200))
        }
        _ => String::new(),
    }
}

/// Extract a short excerpt around a keyword match
fn extract_excerpt(text: &str, keyword: &str, max_len: usize) -> String {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find(keyword) {
        let start = pos.saturating_sub(max_len / 2);
        let end = (pos + keyword.len() + max_len / 2).min(text.len());
        // Find char boundaries
        let start = text.ceil_char_boundary(start);
        let end = text.floor_char_boundary(end);
        let mut excerpt = text[start..end].to_string();
        excerpt = excerpt.replace('\n', " ");
        if start > 0 { excerpt.insert_str(0, "..."); }
        if end < text.len() { excerpt.push_str("..."); }
        excerpt
    } else {
        truncate_str(text, max_len).to_string()
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    let s = s.replace('\n', " ");
    if s.len() <= max_len {
        s
    } else {
        let end = s.floor_char_boundary(max_len);
        format!("{}...", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::context::ContextAccumulator;

    #[test]
    fn test_search_shell_history() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("shell-history.jsonl");
        let log_dir = tmp.path().join("logs");
        let session_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&session_dir).unwrap();

        // Write some history
        let mut acc = ContextAccumulator::with_history(100, history_path.clone(), 10_000);
        let now = chrono::Utc::now();
        acc.preexec("cargo build".to_string(), "/project".to_string(), now);
        acc.postcmd(0, "/project".to_string(), None, now);
        acc.preexec("npm test".to_string(), "/frontend".to_string(), now);
        acc.postcmd(1, "/frontend".to_string(), None, now);

        let retriever = ContextRetriever::new(history_path, log_dir, session_dir);

        // Search all
        let result = retriever.search_shell_history(None, None, None, 50).unwrap();
        assert!(result.contains("cargo build"));
        assert!(result.contains("npm test"));
        assert!(result.contains("2 results"));

        // Search by pattern
        let result = retriever.search_shell_history(Some("cargo"), None, None, 50).unwrap();
        assert!(result.contains("cargo build"));
        assert!(!result.contains("npm test"));

        // Search for failures
        let result = retriever.search_shell_history(None, None, Some(-1), 50).unwrap();
        assert!(result.contains("npm test"));
        assert!(!result.contains("cargo build"));

        // No results
        let result = retriever.search_shell_history(Some("python"), None, None, 50).unwrap();
        assert!(result.contains("No matching"));
    }

    #[test]
    fn test_search_sessions_no_sessions() {
        let tmp = TempDir::new().unwrap();
        let retriever = ContextRetriever::new(
            tmp.path().join("history.jsonl"),
            tmp.path().join("logs"),
            tmp.path().join("sessions"),
        );
        std::fs::create_dir_all(tmp.path().join("sessions")).unwrap();

        let result = retriever.search_sessions("test", 5).unwrap();
        assert!(result.contains("No sessions"));
    }

    #[test]
    fn test_search_logs_no_logs() {
        let tmp = TempDir::new().unwrap();
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        let retriever = ContextRetriever::new(
            tmp.path().join("history.jsonl"),
            log_dir,
            tmp.path().join("sessions"),
        );

        let result = retriever.search_logs(None, None, None, 20).unwrap();
        assert!(result.contains("No matching"));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world!", 5), "hello...");
        assert_eq!(truncate_str("line1\nline2", 20), "line1 line2");
    }
}
