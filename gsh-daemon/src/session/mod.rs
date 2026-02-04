use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents a chat message in a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

/// A chat session with message history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub cwd: String,
}

impl Session {
    pub fn new(cwd: String) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            created_at: now,
            last_activity: now,
            cwd,
        }
    }

    pub fn with_id(id: String, cwd: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            messages: Vec::new(),
            created_at: now,
            last_activity: now,
            cwd,
        }
    }

    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message {
            role: MessageRole::User,
            content,
            timestamp: Utc::now(),
        });
        self.last_activity = Utc::now();
    }

    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message {
            role: MessageRole::Assistant,
            content,
            timestamp: Utc::now(),
        });
        self.last_activity = Utc::now();
    }

    #[allow(dead_code)]
    pub fn add_system_message(&mut self, content: String) {
        self.messages.push(Message {
            role: MessageRole::System,
            content,
            timestamp: Utc::now(),
        });
    }

    #[allow(dead_code)]
    pub fn update_cwd(&mut self, cwd: String) {
        self.cwd = cwd;
    }

    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }
}
