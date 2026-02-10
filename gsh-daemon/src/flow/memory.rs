//! Agent memory store for flow-based coordination
//!
//! Provides per-node memory that persists across agent instances within a flow.
//! Agents can store insights via the `remember` tool and retrieve them via `recall`.
//! Review feedback is automatically stored as memory for future instances.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

/// Source of a memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemorySource {
    /// Explicitly stored by an agent via the remember tool
    Agent(String),
    /// Automatically stored from review feedback
    ReviewFeedback,
}

/// A single memory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Short label/key for the memory
    pub key: String,
    /// Full content of the memory
    pub content: String,
    /// How this memory was created
    pub source: MemorySource,
    /// When this memory was stored
    pub created_at: DateTime<Utc>,
}

/// Thread-safe per-node memory store
pub struct MemoryStore {
    /// Memories organized by node_id
    memories: RwLock<HashMap<String, Vec<Memory>>>,
}

impl MemoryStore {
    /// Create a new empty memory store
    pub fn new() -> Self {
        Self {
            memories: RwLock::new(HashMap::new()),
        }
    }

    /// Store a memory for a specific node
    pub async fn remember(&self, node_id: &str, key: &str, content: &str, source: MemorySource) {
        let memory = Memory {
            key: key.to_string(),
            content: content.to_string(),
            source,
            created_at: Utc::now(),
        };

        let mut store = self.memories.write().await;
        store.entry(node_id.to_string()).or_default().push(memory);
    }

    /// Search memories for a node by keyword (matches key and content)
    pub async fn recall(&self, node_id: &str, query: &str) -> Vec<Memory> {
        let store = self.memories.read().await;
        let query_lower = query.to_lowercase();

        store
            .get(node_id)
            .map(|memories| {
                memories
                    .iter()
                    .filter(|m| {
                        m.key.to_lowercase().contains(&query_lower)
                            || m.content.to_lowercase().contains(&query_lower)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all memories for a node
    pub async fn recall_all(&self, node_id: &str) -> Vec<Memory> {
        let store = self.memories.read().await;
        store.get(node_id).cloned().unwrap_or_default()
    }

    /// Format memories for injection into an agent prompt
    pub async fn format_for_prompt(&self, node_id: &str) -> Option<String> {
        let store = self.memories.read().await;
        let memories = store.get(node_id)?;

        if memories.is_empty() {
            return None;
        }

        let mut output = String::from("--- Memories from previous instances ---\n");
        for memory in memories {
            let source_tag = match &memory.source {
                MemorySource::Agent(agent) => format!("agent:{}", agent),
                MemorySource::ReviewFeedback => "review-feedback".to_string(),
            };
            output.push_str(&format!("[{}] ({}): {}\n", memory.key, source_tag, memory.content));
        }
        output.push('\n');

        Some(output)
    }

    /// Get count of memories for a node
    pub async fn count(&self, node_id: &str) -> usize {
        let store = self.memories.read().await;
        store.get(node_id).map(|m| m.len()).unwrap_or(0)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_remember_and_recall_all() {
        let store = MemoryStore::new();

        store.remember("node1", "auth-pattern", "Use JWT for auth", MemorySource::Agent("agent-1".to_string())).await;
        store.remember("node1", "error-handling", "Always wrap in Result", MemorySource::Agent("agent-2".to_string())).await;

        let memories = store.recall_all("node1").await;
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].key, "auth-pattern");
        assert_eq!(memories[1].key, "error-handling");
    }

    #[tokio::test]
    async fn test_recall_with_query() {
        let store = MemoryStore::new();

        store.remember("node1", "auth-pattern", "Use JWT for auth", MemorySource::Agent("a1".to_string())).await;
        store.remember("node1", "db-pattern", "Use connection pooling", MemorySource::Agent("a2".to_string())).await;
        store.remember("node1", "review-feedback", "Needs better error handling in auth module", MemorySource::ReviewFeedback).await;

        // Search by key
        let results = store.recall("node1", "auth").await;
        assert_eq!(results.len(), 2); // matches "auth-pattern" key and "auth module" content

        // Search by content
        let results = store.recall("node1", "pooling").await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "db-pattern");
    }

    #[tokio::test]
    async fn test_node_isolation() {
        let store = MemoryStore::new();

        store.remember("node1", "key1", "value1", MemorySource::Agent("a1".to_string())).await;
        store.remember("node2", "key2", "value2", MemorySource::Agent("a2".to_string())).await;

        let node1_memories = store.recall_all("node1").await;
        assert_eq!(node1_memories.len(), 1);
        assert_eq!(node1_memories[0].key, "key1");

        let node2_memories = store.recall_all("node2").await;
        assert_eq!(node2_memories.len(), 1);
        assert_eq!(node2_memories[0].key, "key2");

        // Non-existent node has no memories
        let node3_memories = store.recall_all("node3").await;
        assert!(node3_memories.is_empty());
    }

    #[tokio::test]
    async fn test_format_for_prompt() {
        let store = MemoryStore::new();

        // No memories -> None
        assert!(store.format_for_prompt("node1").await.is_none());

        store.remember("node1", "pattern", "Use strategy pattern here", MemorySource::Agent("agent-1".to_string())).await;
        store.remember("node1", "feedback", "Could be more modular", MemorySource::ReviewFeedback).await;

        let formatted = store.format_for_prompt("node1").await.unwrap();
        assert!(formatted.contains("Memories from previous instances"));
        assert!(formatted.contains("[pattern] (agent:agent-1)"));
        assert!(formatted.contains("[feedback] (review-feedback)"));
    }

    #[tokio::test]
    async fn test_count() {
        let store = MemoryStore::new();

        assert_eq!(store.count("node1").await, 0);

        store.remember("node1", "k1", "v1", MemorySource::Agent("a".to_string())).await;
        store.remember("node1", "k2", "v2", MemorySource::Agent("a".to_string())).await;

        assert_eq!(store.count("node1").await, 2);
        assert_eq!(store.count("node2").await, 0);
    }
}
