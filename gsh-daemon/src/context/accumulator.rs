use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

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

impl ShellEvent {
    pub fn timestamp(&self) -> &DateTime<Utc> {
        match self {
            ShellEvent::Command { timestamp, .. } => timestamp,
            ShellEvent::DirectoryChange { timestamp, .. } => timestamp,
        }
    }
}

/// Accumulates shell events and generates context strings for LLM
pub struct ContextAccumulator {
    events: VecDeque<ShellEvent>,
    max_events: usize,
    /// Pending command (preexec received, waiting for postcmd)
    pending_command: Option<PendingCommand>,
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
        if self.events.len() >= self.max_events {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    /// Get recent events (most recent first)
    pub fn recent_events(&self, count: usize) -> Vec<&ShellEvent> {
        self.events.iter().rev().take(count).collect()
    }

    /// Generate a context string for the LLM
    pub fn generate_context(&self, max_chars: usize) -> String {
        let mut context = String::new();
        let mut chars_used = 0;

        // Header
        let header = "# Recent Shell Activity\n\n";
        context.push_str(header);
        chars_used += header.len();

        // Add events from oldest to newest (for chronological order)
        for event in &self.events {
            let event_str = self.format_event(event);
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

    fn format_event(&self, event: &ShellEvent) -> String {
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

    /// Clear all events
    pub fn clear(&mut self) {
        self.events.clear();
        self.pending_command = None;
    }

    /// Number of recorded events
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
