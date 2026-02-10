use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

/// Represents a shell event captured by the hooks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellEvent {
    /// A command was executed
    Command {
        command: String,
        cwd: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
        timestamp: DateTime<Utc>,
    },
    /// Directory changed
    DirectoryChange {
        from: String,
        to: String,
        timestamp: DateTime<Utc>,
    },
}

#[allow(dead_code)]
impl ShellEvent {
    pub fn timestamp(&self) -> &DateTime<Utc> {
        match self {
            ShellEvent::Command { timestamp, .. } => timestamp,
            ShellEvent::DirectoryChange { timestamp, .. } => timestamp,
        }
    }
}

/// Appends ShellEvents to a JSONL file on disk
struct ShellHistoryWriter {
    writer: BufWriter<File>,
}

impl ShellHistoryWriter {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn append(&mut self, event: &ShellEvent) -> std::io::Result<()> {
        let json = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        writeln!(self.writer, "{}", json)?;
        self.writer.flush()
    }
}

/// Load the last `max_lines` events from a JSONL file
fn load_history(path: &Path, max_lines: usize) -> Vec<ShellEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut events = VecDeque::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        if let Ok(event) = serde_json::from_str::<ShellEvent>(&line) {
            if events.len() >= max_lines {
                events.pop_front();
            }
            events.push_back(event);
        }
    }

    events.into()
}

/// Truncate a JSONL history file to the last `keep` lines
pub fn truncate_history_file(path: &Path, keep: usize) -> std::io::Result<()> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(()), // file doesn't exist, nothing to truncate
    };
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().filter_map(|l| l.ok()).collect();

    if lines.len() <= keep {
        return Ok(());
    }

    // Keep only the last `keep` lines
    let to_keep = &lines[lines.len() - keep..];
    let tmp_path = path.with_extension("jsonl.tmp");
    {
        let mut writer = BufWriter::new(File::create(&tmp_path)?);
        for line in to_keep {
            writeln!(writer, "{}", line)?;
        }
        writer.flush()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Accumulates shell events and generates context strings for LLM
pub struct ContextAccumulator {
    events: VecDeque<ShellEvent>,
    max_events: usize,
    /// Pending command (preexec received, waiting for postcmd)
    pending_command: Option<PendingCommand>,
    /// Optional persistent writer for shell history
    history_writer: Option<ShellHistoryWriter>,
    /// Path to the history file (for search)
    history_path: Option<PathBuf>,
}

struct PendingCommand {
    command: String,
    cwd: String,
    timestamp: DateTime<Utc>,
}

impl ContextAccumulator {
    pub fn new(max_events: usize) -> Self {
        Self {
            events: VecDeque::with_capacity(max_events),
            max_events,
            pending_command: None,
            history_writer: None,
            history_path: None,
        }
    }

    /// Create with persistent history file
    ///
    /// Loads existing events from disk into the in-memory buffer and opens
    /// the file for appending new events. Truncates the file if it exceeds
    /// `max_history_lines`.
    pub fn with_history(max_events: usize, history_path: PathBuf, max_history_lines: usize) -> Self {
        // Ensure parent directory exists
        if let Some(parent) = history_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Truncate if too large
        if let Err(e) = truncate_history_file(&history_path, max_history_lines) {
            tracing::warn!("Failed to truncate shell history: {}", e);
        }

        // Load recent events into memory
        let events: VecDeque<ShellEvent> = load_history(&history_path, max_events).into();
        tracing::info!("Loaded {} shell events from history", events.len());

        // Open writer for appending
        let history_writer = match ShellHistoryWriter::open(&history_path) {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!("Failed to open shell history for writing: {}", e);
                None
            }
        };

        Self {
            events,
            max_events,
            pending_command: None,
            history_writer,
            history_path: Some(history_path),
        }
    }

    /// Record a preexec event (command about to execute)
    pub fn preexec(&mut self, command: String, cwd: String, timestamp: DateTime<Utc>) {
        // If there's a pending command that never got a postcmd, finalize it
        if let Some(pending) = self.pending_command.take() {
            self.push_event(ShellEvent::Command {
                command: pending.command,
                cwd: pending.cwd,
                exit_code: None,
                duration_ms: None,
                timestamp: pending.timestamp,
            });
        }

        self.pending_command = Some(PendingCommand {
            command,
            cwd,
            timestamp,
        });
    }

    /// Record a postcmd event (command finished)
    pub fn postcmd(&mut self, exit_code: i32, cwd: String, duration_ms: Option<u64>, timestamp: DateTime<Utc>) {
        if let Some(pending) = self.pending_command.take() {
            self.push_event(ShellEvent::Command {
                command: pending.command,
                cwd: pending.cwd,
                exit_code: Some(exit_code),
                duration_ms,
                timestamp: pending.timestamp,
            });
        } else {
            // Postcmd without preexec - this can happen on shell startup
            // We'll record a placeholder event
            self.push_event(ShellEvent::Command {
                command: String::new(),
                cwd,
                exit_code: Some(exit_code),
                duration_ms,
                timestamp,
            });
        }
    }

    /// Record a directory change
    pub fn chpwd(&mut self, from: String, to: String, timestamp: DateTime<Utc>) {
        self.push_event(ShellEvent::DirectoryChange {
            from,
            to,
            timestamp,
        });
    }

    fn push_event(&mut self, event: ShellEvent) {
        // Persist to disk
        if let Some(ref mut writer) = self.history_writer {
            if let Err(e) = writer.append(&event) {
                tracing::warn!("Failed to persist shell event: {}", e);
            }
        }

        // Keep in-memory buffer bounded
        if self.events.len() >= self.max_events {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Get recent events (most recent first)
    #[allow(dead_code)]
    pub fn recent_events(&self, count: usize) -> Vec<&ShellEvent> {
        self.events.iter().rev().take(count).collect()
    }

    /// Generate a context string for the LLM (full dump, legacy)
    pub fn generate_context(&self, max_chars: usize) -> String {
        let mut context = String::new();
        let mut chars_used = 0;

        // Header
        let header = "# Recent Shell Activity\n\n";
        context.push_str(header);
        chars_used += header.len();

        // Add events from oldest to newest (for chronological order)
        for event in &self.events {
            let event_str = format_event(event);
            if chars_used + event_str.len() > max_chars {
                break;
            }
            context.push_str(&event_str);
            chars_used += event_str.len();
        }

        if context.len() == header.len() {
            context.push_str("(No recent shell activity recorded)\n");
        }

        context
    }

    /// Generate a minimal ambient summary (last N commands) for JIT context loading
    pub fn generate_ambient_summary(&self, n: usize) -> String {
        let mut summary = String::from("# Recent Shell Activity\n\n");

        // Collect last N non-empty commands
        let recent: Vec<_> = self.events.iter().rev()
            .filter(|e| matches!(e, ShellEvent::Command { command, .. } if !command.is_empty()))
            .take(n)
            .collect();

        if recent.is_empty() {
            summary.push_str("(No recent shell activity)\n");
        } else {
            // Reverse back to chronological order
            for event in recent.into_iter().rev() {
                summary.push_str(&format_event(event));
            }
        }

        summary.push_str("\nUse the `search_context` tool for more shell history, past sessions, or logs.\n");
        summary
    }

    /// Get the history file path (for use by ContextRetriever)
    pub fn history_path(&self) -> Option<&Path> {
        self.history_path.as_deref()
    }

    /// Clear all events
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.events.clear();
        self.pending_command = None;
    }

    /// Number of recorded events
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if empty
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Format a single shell event for display
pub fn format_event(event: &ShellEvent) -> String {
    match event {
        ShellEvent::Command {
            command,
            cwd,
            exit_code,
            duration_ms,
            timestamp,
        } => {
            let status = match exit_code {
                Some(0) => "OK".to_string(),
                Some(code) => format!("exit {}", code),
                None => "?".to_string(),
            };
            let duration = duration_ms
                .map(|d| format!(" ({}ms)", d))
                .unwrap_or_default();
            let time = timestamp.format("%H:%M:%S");

            if command.is_empty() {
                return String::new();
            }

            format!(
                "[{}] {} $ {}\n  -> {}{}\n\n",
                time, cwd, command, status, duration
            )
        }
        ShellEvent::DirectoryChange { from, to, timestamp } => {
            let time = timestamp.format("%H:%M:%S");
            format!("[{}] cd: {} -> {}\n\n", time, from, to)
        }
    }
}

/// Search shell history from the on-disk JSONL file
pub fn search_history(
    path: &Path,
    pattern: Option<&str>,
    cwd_filter: Option<&str>,
    exit_code_filter: Option<i32>,
    last_n: usize,
) -> Vec<ShellEvent> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut matches = VecDeque::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) if !l.trim().is_empty() => l,
            _ => continue,
        };
        let event: ShellEvent = match serde_json::from_str(&line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let matched = match &event {
            ShellEvent::Command { command, cwd, exit_code, .. } => {
                // Pattern filter: substring match on command
                let pattern_ok = pattern
                    .map(|p| command.to_lowercase().contains(&p.to_lowercase()))
                    .unwrap_or(true);
                // CWD filter: prefix match
                let cwd_ok = cwd_filter
                    .map(|f| cwd.starts_with(f))
                    .unwrap_or(true);
                // Exit code filter: -1 means "any non-zero"
                let exit_ok = match exit_code_filter {
                    Some(-1) => exit_code.map(|c| c != 0).unwrap_or(false),
                    Some(code) => *exit_code == Some(code),
                    None => true,
                };
                pattern_ok && cwd_ok && exit_ok
            }
            ShellEvent::DirectoryChange { from, to, .. } => {
                // Pattern matches directory names
                let pattern_ok = pattern
                    .map(|p| {
                        let p = p.to_lowercase();
                        from.to_lowercase().contains(&p) || to.to_lowercase().contains(&p)
                    })
                    .unwrap_or(true);
                // CWD filter matches either from or to
                let cwd_ok = cwd_filter
                    .map(|f| from.starts_with(f) || to.starts_with(f))
                    .unwrap_or(true);
                // Exit code filter doesn't apply to directory changes
                let exit_ok = exit_code_filter.is_none();
                pattern_ok && cwd_ok && exit_ok
            }
        };

        if matched {
            if matches.len() >= last_n {
                matches.pop_front();
            }
            matches.push_back(event);
        }
    }

    matches.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_command_lifecycle() {
        let mut acc = ContextAccumulator::new(10);
        let now = Utc::now();

        acc.preexec("ls -la".to_string(), "/home/user".to_string(), now);
        acc.postcmd(0, "/home/user".to_string(), Some(50), now);

        assert_eq!(acc.len(), 1);

        let events = acc.recent_events(1);
        match &events[0] {
            ShellEvent::Command { command, exit_code, .. } => {
                assert_eq!(command, "ls -la");
                assert_eq!(*exit_code, Some(0));
            }
            _ => panic!("Expected Command event"),
        }
    }

    #[test]
    fn test_max_events() {
        let mut acc = ContextAccumulator::new(3);
        let now = Utc::now();

        for i in 0..5 {
            acc.preexec(format!("cmd{}", i), "/".to_string(), now);
            acc.postcmd(0, "/".to_string(), None, now);
        }

        assert_eq!(acc.len(), 3);

        let events = acc.recent_events(3);
        match &events[0] {
            ShellEvent::Command { command, .. } => {
                assert_eq!(command, "cmd4");
            }
            _ => panic!("Expected Command event"),
        }
    }

    #[test]
    fn test_persistent_history_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("shell-history.jsonl");

        // Write some events
        {
            let mut acc = ContextAccumulator::with_history(10, history_path.clone(), 10_000);
            let now = Utc::now();
            acc.preexec("cargo build".to_string(), "/project".to_string(), now);
            acc.postcmd(0, "/project".to_string(), Some(5000), now);
            acc.preexec("cargo test".to_string(), "/project".to_string(), now);
            acc.postcmd(1, "/project".to_string(), Some(3000), now);
            assert_eq!(acc.len(), 2);
        }

        // Load from a fresh accumulator — events should survive
        {
            let acc = ContextAccumulator::with_history(10, history_path, 10_000);
            assert_eq!(acc.len(), 2);

            let events = acc.recent_events(2);
            match &events[0] {
                ShellEvent::Command { command, exit_code, .. } => {
                    assert_eq!(command, "cargo test");
                    assert_eq!(*exit_code, Some(1));
                }
                _ => panic!("Expected Command event"),
            }
        }
    }

    #[test]
    fn test_ambient_summary() {
        let mut acc = ContextAccumulator::new(100);
        let now = Utc::now();

        for i in 0..20 {
            acc.preexec(format!("cmd{}", i), "/".to_string(), now);
            acc.postcmd(0, "/".to_string(), None, now);
        }

        let summary = acc.generate_ambient_summary(5);
        assert!(summary.contains("cmd15"));
        assert!(summary.contains("cmd19"));
        assert!(!summary.contains("cmd14")); // Only last 5
        assert!(summary.contains("search_context"));
    }

    #[test]
    fn test_search_history() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("shell-history.jsonl");

        // Write events
        let mut acc = ContextAccumulator::with_history(100, history_path.clone(), 10_000);
        let now = Utc::now();
        acc.preexec("cargo build".to_string(), "/project".to_string(), now);
        acc.postcmd(0, "/project".to_string(), None, now);
        acc.preexec("npm test".to_string(), "/frontend".to_string(), now);
        acc.postcmd(1, "/frontend".to_string(), None, now);
        acc.preexec("cargo test".to_string(), "/project".to_string(), now);
        acc.postcmd(0, "/project".to_string(), None, now);

        // Search by pattern
        let results = search_history(&history_path, Some("cargo"), None, None, 50);
        assert_eq!(results.len(), 2);

        // Search by cwd
        let results = search_history(&history_path, None, Some("/frontend"), None, 50);
        assert_eq!(results.len(), 1);

        // Search for failures
        let results = search_history(&history_path, None, None, Some(-1), 50);
        assert_eq!(results.len(), 1);
        match &results[0] {
            ShellEvent::Command { command, .. } => assert_eq!(command, "npm test"),
            _ => panic!("Expected Command"),
        }

        // Search with last_n cap
        let results = search_history(&history_path, None, None, None, 2);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_truncate_history_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.jsonl");

        // Write 20 events
        {
            let mut acc = ContextAccumulator::with_history(100, path.clone(), 100_000);
            let now = Utc::now();
            for i in 0..20 {
                acc.preexec(format!("cmd{}", i), "/".to_string(), now);
                acc.postcmd(0, "/".to_string(), None, now);
            }
        }

        // Truncate to last 5
        truncate_history_file(&path, 5).unwrap();

        let events = load_history(&path, 100);
        assert_eq!(events.len(), 5);
        match &events[0] {
            ShellEvent::Command { command, .. } => assert_eq!(command, "cmd15"),
            _ => panic!("Expected Command"),
        }
    }
}
