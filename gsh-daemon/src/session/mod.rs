//! Session management with JSONL persistence
//!
//! Each session is stored as a single JSONL file:
//! - Line 1: SessionMetadata JSON
//! - Lines 2+: provider::ChatMessage JSON (one per line)

use crate::provider::{ChatMessage, ChatRole, MessageContent};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, warn};
use uuid::Uuid;

/// Session metadata, persisted as the first line of the JSONL file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
    pub cwd: String,
    pub title: Option<String>,
}

/// A chat session with message history and disk persistence
pub struct Session {
    pub metadata: SessionMetadata,
    /// Full conversation history using provider types (preserves tool use structure)
    pub messages: Vec<ChatMessage>,
    /// Append-only writer for the JSONL file
    writer: Option<BufWriter<File>>,
    /// Directory where session files are stored
    session_dir: PathBuf,
}

impl Session {
    /// Create a new session with persistence
    pub fn create(cwd: String, session_dir: PathBuf) -> Result<Self> {
        let id = Uuid::new_v4().to_string();
        Self::create_with_id(id, cwd, session_dir)
    }

    /// Create a new session with a specific ID
    fn create_with_id(id: String, cwd: String, session_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&session_dir)
            .with_context(|| format!("Failed to create session directory: {:?}", session_dir))?;

        let now = Utc::now();
        let metadata = SessionMetadata {
            id: id.clone(),
            created_at: now,
            last_activity: now,
            message_count: 0,
            cwd,
            title: None,
        };

        let file_path = session_dir.join(format!("{}.jsonl", id));
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_path)
            .with_context(|| format!("Failed to create session file: {:?}", file_path))?;

        let mut writer = BufWriter::new(file);

        // Write metadata as first line
        let meta_json = serde_json::to_string(&metadata)?;
        writeln!(writer, "{}", meta_json)?;
        writer.flush()?;

        debug!("Created session {} at {:?}", id, file_path);

        Ok(Self {
            metadata,
            messages: Vec::new(),
            writer: Some(writer),
            session_dir,
        })
    }

    /// Load an existing session from disk
    pub fn load(session_id: &str, session_dir: PathBuf) -> Result<Self> {
        let file_path = session_dir.join(format!("{}.jsonl", session_id));

        if !file_path.exists() {
            anyhow::bail!("Session file not found: {:?}", file_path);
        }

        let file = File::open(&file_path)
            .with_context(|| format!("Failed to open session file: {:?}", file_path))?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // First line is metadata
        let meta_line = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("Empty session file: {:?}", file_path))?
            .with_context(|| format!("Failed to read metadata from: {:?}", file_path))?;

        let metadata: SessionMetadata = serde_json::from_str(&meta_line)
            .with_context(|| format!("Failed to parse session metadata: {}", meta_line))?;

        // Remaining lines are messages
        let mut messages = Vec::new();
        for (line_num, line) in lines.enumerate() {
            let line = line.with_context(|| {
                format!("Failed to read line {} from: {:?}", line_num + 2, file_path)
            })?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ChatMessage>(&line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    warn!(
                        "Skipping malformed message at line {} in {:?}: {}",
                        line_num + 2,
                        file_path,
                        e
                    );
                }
            }
        }

        // Rebuild metadata from actual messages
        let mut metadata = metadata;
        metadata.message_count = messages.len();
        if metadata.title.is_none() {
            // Derive title from first user message
            for msg in &messages {
                if let ChatRole::User = msg.role {
                    let text = msg.text_content();
                    if !text.is_empty() {
                        metadata.title = Some(text.chars().take(80).collect());
                        break;
                    }
                }
            }
        }
        if let Some(last) = messages.last() {
            // Use the last message time as last_activity if available
            let _ = last; // messages don't have timestamps, keep metadata value
        }

        debug!(
            "Loaded session {} with {} messages from {:?}",
            session_id,
            messages.len(),
            file_path
        );

        // Open file for appending new messages
        let file = OpenOptions::new()
            .append(true)
            .open(&file_path)
            .with_context(|| format!("Failed to open session file for append: {:?}", file_path))?;
        let writer = BufWriter::new(file);

        Ok(Self {
            metadata,
            messages,
            writer: Some(writer),
            session_dir,
        })
    }

    /// Add a message to the session and persist to disk
    pub fn add_message(&mut self, msg: ChatMessage) -> Result<()> {
        // Auto-generate title from first user message
        if self.metadata.title.is_none() {
            if let ChatRole::User = msg.role {
                let text = msg.text_content();
                if !text.is_empty() {
                    let title: String = text.chars().take(80).collect();
                    self.metadata.title = Some(title);
                }
            }
        }

        // Persist to JSONL
        if let Some(ref mut writer) = self.writer {
            let json = serde_json::to_string(&msg)?;
            writeln!(writer, "{}", json)?;
            writer.flush()?;
        }

        self.messages.push(msg);
        self.metadata.message_count = self.messages.len();
        self.metadata.last_activity = Utc::now();

        Ok(())
    }

    /// Get the session ID
    pub fn id(&self) -> &str {
        &self.metadata.id
    }

    /// Close the session (flush writer)
    pub fn close(&mut self) {
        if let Some(ref mut writer) = self.writer {
            let _ = writer.flush();
        }
        self.writer = None;
    }

    /// Rewrite the metadata line (first line of the file)
    #[allow(dead_code)]
    pub fn save_metadata(&self) -> Result<()> {
        let file_path = self.session_dir.join(format!("{}.jsonl", self.metadata.id));
        if !file_path.exists() {
            return Ok(());
        }

        // Read all lines
        let content = fs::read_to_string(&file_path)?;
        let mut lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            return Ok(());
        }

        // Replace first line with updated metadata
        let meta_json = serde_json::to_string(&self.metadata)?;
        lines[0] = &meta_json;

        // Rewrite file
        let mut file = File::create(&file_path)?;
        for line in &lines {
            writeln!(file, "{}", line)?;
        }

        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

/// List all saved sessions by reading metadata from JSONL files
pub fn list_sessions(session_dir: &Path) -> Result<Vec<SessionMetadata>> {
    let mut sessions = Vec::new();

    if !session_dir.exists() {
        return Ok(sessions);
    }

    for entry in fs::read_dir(session_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
            match read_session_metadata(&path) {
                Ok(meta) => sessions.push(meta),
                Err(e) => {
                    warn!("Failed to read session metadata from {:?}: {}", path, e);
                }
            }
        }
    }

    // Sort by last_activity descending (most recent first)
    sessions.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));

    Ok(sessions)
}

/// Read metadata from a session JSONL file.
/// Reads the metadata line and scans messages for title and accurate count.
fn read_session_metadata(path: &Path) -> Result<SessionMetadata> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let first_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("Empty session file"))??;
    let mut metadata: SessionMetadata = serde_json::from_str(&first_line)?;

    // Count messages and derive title if missing
    let mut count = 0;
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        count += 1;
        if metadata.title.is_none() {
            if let Ok(msg) = serde_json::from_str::<ChatMessage>(&line) {
                if let ChatRole::User = msg.role {
                    let text = msg.text_content();
                    if !text.is_empty() {
                        metadata.title = Some(text.chars().take(80).collect());
                    }
                }
            }
        }
    }
    metadata.message_count = count;

    Ok(metadata)
}

/// Clean up old sessions based on max count and max age
pub fn cleanup_sessions(session_dir: &Path, max_sessions: usize, max_age_days: u64) -> Result<usize> {
    let sessions = list_sessions(session_dir)?;
    let mut deleted = 0;

    let cutoff = Utc::now() - chrono::Duration::days(max_age_days as i64);

    for (i, session) in sessions.iter().enumerate() {
        let too_many = i >= max_sessions;
        let too_old = session.last_activity < cutoff;

        if too_many || too_old {
            if let Err(e) = delete_session(&session.id, session_dir) {
                warn!("Failed to clean up session {}: {}", &session.id[..8], e);
            } else {
                debug!(
                    "Cleaned up session {} ({})",
                    &session.id[..8],
                    if too_old { "expired" } else { "over limit" }
                );
                deleted += 1;
            }
        }
    }

    if deleted > 0 {
        debug!("Session cleanup: deleted {} session(s)", deleted);
    }

    Ok(deleted)
}

/// Delete a session file from disk
pub fn delete_session(session_id: &str, session_dir: &Path) -> Result<()> {
    let file_path = session_dir.join(format!("{}.jsonl", session_id));
    if file_path.exists() {
        fs::remove_file(&file_path)
            .with_context(|| format!("Failed to delete session file: {:?}", file_path))?;
        debug!("Deleted session {} from {:?}", session_id, file_path);
    }
    Ok(())
}

/// Helper trait extension for ChatMessage to extract text content
trait ChatMessageExt {
    fn text_content(&self) -> String;
}

impl ChatMessageExt for ChatMessage {
    fn text_content(&self) -> String {
        match &self.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Blocks(blocks) => {
                use crate::provider::ContentBlock;
                blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ChatRole, ContentBlock, MessageContent};
    use tempfile::tempdir;

    #[test]
    fn test_create_and_load_session() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();

        // Create session
        let mut session = Session::create("/home/user".to_string(), session_dir.clone()).unwrap();
        let session_id = session.id().to_string();

        // Add messages
        session
            .add_message(ChatMessage {
                role: ChatRole::User,
                content: MessageContent::Text("Hello!".to_string()),
            })
            .unwrap();
        session
            .add_message(ChatMessage {
                role: ChatRole::Assistant,
                content: MessageContent::Text("Hi there!".to_string()),
            })
            .unwrap();

        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.metadata.message_count, 2);
        assert_eq!(session.metadata.title, Some("Hello!".to_string()));

        // Close and reload
        session.close();

        let loaded = Session::load(&session_id, session_dir).unwrap();
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.metadata.id, session_id);
        assert_eq!(loaded.metadata.cwd, "/home/user");
        assert_eq!(loaded.metadata.title, Some("Hello!".to_string()));
    }

    #[test]
    fn test_session_preserves_tool_use() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();

        let mut session = Session::create("/tmp".to_string(), session_dir.clone()).unwrap();
        let session_id = session.id().to_string();

        // Add a message with tool use blocks
        session
            .add_message(ChatMessage {
                role: ChatRole::Assistant,
                content: MessageContent::Blocks(vec![
                    ContentBlock::Text {
                        text: "Let me check.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-1".to_string(),
                        name: "bash".to_string(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ]),
            })
            .unwrap();

        session.close();

        // Reload and verify structure preserved
        let loaded = Session::load(&session_id, session_dir).unwrap();
        assert_eq!(loaded.messages.len(), 1);

        if let MessageContent::Blocks(blocks) = &loaded.messages[0].content {
            assert_eq!(blocks.len(), 2);
            matches!(&blocks[0], ContentBlock::Text { text } if text == "Let me check.");
            matches!(&blocks[1], ContentBlock::ToolUse { name, .. } if name == "bash");
        } else {
            panic!("Expected Blocks content");
        }
    }

    #[test]
    fn test_list_sessions() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();

        // Create multiple sessions
        let mut s1 = Session::create("/a".to_string(), session_dir.clone()).unwrap();
        s1.add_message(ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("First session".to_string()),
        })
        .unwrap();
        s1.close();

        let mut s2 = Session::create("/b".to_string(), session_dir.clone()).unwrap();
        s2.add_message(ChatMessage {
            role: ChatRole::User,
            content: MessageContent::Text("Second session".to_string()),
        })
        .unwrap();
        s2.close();

        let sessions = list_sessions(&session_dir).unwrap();
        assert_eq!(sessions.len(), 2);
        // Most recent first
        assert_eq!(sessions[0].title, Some("Second session".to_string()));
    }

    #[test]
    fn test_delete_session() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();

        let mut session = Session::create("/tmp".to_string(), session_dir.clone()).unwrap();
        let session_id = session.id().to_string();
        session.close();

        assert!(session_dir.join(format!("{}.jsonl", session_id)).exists());

        delete_session(&session_id, &session_dir).unwrap();

        assert!(!session_dir.join(format!("{}.jsonl", session_id)).exists());
    }

    #[test]
    fn test_list_empty_dir() {
        let dir = tempdir().unwrap();
        let sessions = list_sessions(dir.path()).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_nonexistent_dir() {
        let sessions = list_sessions(Path::new("/nonexistent/path")).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_load_nonexistent_session() {
        let dir = tempdir().unwrap();
        let result = Session::load("nonexistent-id", dir.path().to_path_buf());
        assert!(result.is_err());
    }

    #[test]
    fn test_session_title_from_first_user_message() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();

        let mut session = Session::create("/tmp".to_string(), session_dir).unwrap();

        // Assistant message first - should not set title
        session
            .add_message(ChatMessage {
                role: ChatRole::Assistant,
                content: MessageContent::Text("Welcome!".to_string()),
            })
            .unwrap();
        assert_eq!(session.metadata.title, None);

        // User message sets title
        session
            .add_message(ChatMessage {
                role: ChatRole::User,
                content: MessageContent::Text("How do I configure gsh?".to_string()),
            })
            .unwrap();
        assert_eq!(
            session.metadata.title,
            Some("How do I configure gsh?".to_string())
        );

        // Second user message doesn't change title
        session
            .add_message(ChatMessage {
                role: ChatRole::User,
                content: MessageContent::Text("Never mind".to_string()),
            })
            .unwrap();
        assert_eq!(
            session.metadata.title,
            Some("How do I configure gsh?".to_string())
        );
    }
}
