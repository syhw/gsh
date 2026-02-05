//! Structured JSONL logging for observability
//!
//! Logs events to a JSONL file for analysis, debugging, and replay.
//! Each line is a complete JSON object representing an event.

use super::events::{EventKind, ObservabilityEvent};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, error};

/// Configuration for the event logger
#[derive(Debug, Clone)]
pub struct LoggerConfig {
    /// Directory to store log files
    pub log_dir: PathBuf,
    /// Maximum file size before rotation (bytes)
    pub max_file_size: u64,
    /// Whether to also log to stderr
    pub log_to_stderr: bool,
    /// Only log events of these types (empty = all)
    pub event_filter: Vec<String>,
}

impl Default for LoggerConfig {
    fn default() -> Self {
        let log_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("gsh")
            .join("logs");

        Self {
            log_dir,
            max_file_size: 10 * 1024 * 1024, // 10MB
            log_to_stderr: false,
            event_filter: vec![],
        }
    }
}

/// Event logger that writes to JSONL files
pub struct EventLogger {
    config: LoggerConfig,
    writer: Arc<Mutex<Option<BufWriter<File>>>>,
    current_file: Arc<Mutex<Option<PathBuf>>>,
    bytes_written: Arc<Mutex<u64>>,
}

impl EventLogger {
    /// Create a new logger with the given configuration
    pub fn new(config: LoggerConfig) -> Result<Self> {
        // Ensure log directory exists
        std::fs::create_dir_all(&config.log_dir)
            .with_context(|| format!("Failed to create log directory: {:?}", config.log_dir))?;

        let logger = Self {
            config,
            writer: Arc::new(Mutex::new(None)),
            current_file: Arc::new(Mutex::new(None)),
            bytes_written: Arc::new(Mutex::new(0)),
        };

        // Open initial file
        logger.rotate_if_needed()?;

        Ok(logger)
    }

    /// Create a logger with default configuration
    pub fn with_defaults() -> Result<Self> {
        Self::new(LoggerConfig::default())
    }

    /// Get the current log file path
    pub fn current_file(&self) -> Option<PathBuf> {
        self.current_file.lock().unwrap().clone()
    }

    /// Log an event
    pub fn log(&self, event: &ObservabilityEvent) -> Result<()> {
        // Check event filter
        if !self.config.event_filter.is_empty() {
            let event_type = match &event.event {
                EventKind::Prompt { .. } => "prompt",
                EventKind::Spawn { .. } => "spawn",
                EventKind::Start { .. } => "start",
                EventKind::Text { .. } => "text",
                EventKind::ToolCall { .. } => "tool_call",
                EventKind::ToolResult { .. } => "tool_result",
                EventKind::Complete { .. } => "complete",
                EventKind::Error { .. } => "error",
                EventKind::Usage { .. } => "usage",
                EventKind::Paused => "paused",
                EventKind::Resumed => "resumed",
                EventKind::Inject { .. } => "inject",
                EventKind::Redirect { .. } => "redirect",
                EventKind::FlowStart { .. } => "flow_start",
                EventKind::FlowComplete { .. } => "flow_complete",
                EventKind::Iteration { .. } => "iteration",
                EventKind::BashExec { .. } => "bash_exec",
            };

            if !self.config.event_filter.iter().any(|f| f == event_type) {
                return Ok(());
            }
        }

        // Serialize event
        let json = serde_json::to_string(event)?;
        let line = format!("{}\n", json);
        let line_bytes = line.as_bytes();

        // Check if rotation is needed
        self.rotate_if_needed()?;

        // Write to file
        {
            let mut writer_guard = self.writer.lock().unwrap();
            if let Some(ref mut writer) = *writer_guard {
                writer.write_all(line_bytes)?;
                writer.flush()?;
            }
        }

        // Update bytes written
        {
            let mut bytes = self.bytes_written.lock().unwrap();
            *bytes += line_bytes.len() as u64;
        }

        // Optionally log to stderr
        if self.config.log_to_stderr {
            eprintln!("[gsh] {}", json);
        }

        Ok(())
    }

    /// Rotate to a new file if needed
    fn rotate_if_needed(&self) -> Result<()> {
        let should_rotate = {
            let bytes = self.bytes_written.lock().unwrap();
            let writer = self.writer.lock().unwrap();
            writer.is_none() || *bytes >= self.config.max_file_size
        };

        if should_rotate {
            self.rotate()?;
        }

        Ok(())
    }

    /// Force rotation to a new file
    fn rotate(&self) -> Result<()> {
        // Generate new filename with timestamp
        let now = chrono::Utc::now();
        let filename = format!("gsh-{}.jsonl", now.format("%Y%m%d-%H%M%S"));
        let path = self.config.log_dir.join(&filename);

        debug!("Rotating log to: {:?}", path);

        // Open new file
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open log file: {:?}", path))?;

        let writer = BufWriter::new(file);

        // Update state
        {
            let mut writer_guard = self.writer.lock().unwrap();
            *writer_guard = Some(writer);
        }
        {
            let mut current = self.current_file.lock().unwrap();
            *current = Some(path);
        }
        {
            let mut bytes = self.bytes_written.lock().unwrap();
            *bytes = 0;
        }

        Ok(())
    }

    /// Create a channel-based logger for async contexts
    pub fn spawn_async(self) -> (mpsc::Sender<ObservabilityEvent>, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::channel::<ObservabilityEvent>(1000);
        let logger = Arc::new(self);

        let handle = tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                if let Err(e) = logger.log(&event) {
                    error!("Failed to log event: {}", e);
                }
            }
            debug!("Event logger shutting down");
        });

        (tx, handle)
    }
}

/// Read events from a JSONL log file
pub fn read_events(path: &Path) -> Result<Vec<ObservabilityEvent>> {
    use std::io::{BufRead, BufReader};

    let file = File::open(path).with_context(|| format!("Failed to open log file: {:?}", path))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("Failed to read line {}", line_num + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: ObservabilityEvent = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse event at line {}: {}", line_num + 1, line))?;
        events.push(event);
    }

    Ok(events)
}

/// Find all log files in the log directory
pub fn list_log_files(log_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    if !log_dir.exists() {
        return Ok(files);
    }

    for entry in std::fs::read_dir(log_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            files.push(path);
        }
    }

    // Sort by filename (which includes timestamp)
    files.sort();

    Ok(files)
}

/// Get the most recent log file
pub fn latest_log_file(log_dir: &Path) -> Result<Option<PathBuf>> {
    let files = list_log_files(log_dir)?;
    Ok(files.into_iter().last())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_logger_creation() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let logger = EventLogger::new(config).unwrap();
        assert!(logger.current_file().is_some());
    }

    #[test]
    fn test_event_logging() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            ..Default::default()
        };

        let logger = EventLogger::new(config).unwrap();

        let event = ObservabilityEvent::new(
            "session-123",
            "root",
            EventKind::Prompt {
                content: "Hello, world!".to_string(),
            },
        );

        logger.log(&event).unwrap();

        // Read back the event
        let log_file = logger.current_file().unwrap();
        let events = read_events(&log_file).unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session, "session-123");
        assert_eq!(events[0].agent, "root");
    }

    #[test]
    fn test_event_filter() {
        let dir = tempdir().unwrap();
        let config = LoggerConfig {
            log_dir: dir.path().to_path_buf(),
            event_filter: vec!["tool_call".to_string()],
            ..Default::default()
        };

        let logger = EventLogger::new(config).unwrap();

        // This should be filtered out
        let prompt_event = ObservabilityEvent::new(
            "session-123",
            "root",
            EventKind::Prompt {
                content: "Hello".to_string(),
            },
        );

        // This should be logged
        let tool_event = ObservabilityEvent::new(
            "session-123",
            "root",
            EventKind::ToolCall {
                tool: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            },
        );

        logger.log(&prompt_event).unwrap();
        logger.log(&tool_event).unwrap();

        // Read back events
        let log_file = logger.current_file().unwrap();
        let events = read_events(&log_file).unwrap();

        assert_eq!(events.len(), 1);
        matches!(&events[0].event, EventKind::ToolCall { .. });
    }
}
