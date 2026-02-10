//! Publication tools for agents in publication coordination mode
//!
//! These tools allow agents to:
//! - Publish findings
//! - Cite other agents' work
//! - Review and grade publications
//! - Search for relevant publications
//! - View citation graphs
//! - Revise their own publications
//! - Remember and recall insights

use super::memory::{MemorySource, MemoryStore};
use super::publication::{Grade, PublicationStore};
use crate::agent::CustomToolHandler;
use crate::provider::ToolDefinition;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool definitions for publication mode agents
pub fn publication_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "publish",
            "description": "Publish a finding or result for other agents to review. Use this to share discoveries, conclusions, or important information.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A concise title summarizing the finding (max 100 chars)"
                    },
                    "content": {
                        "type": "string",
                        "description": "The full content of your finding. Be thorough and include supporting evidence."
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Tags to categorize this publication (e.g., ['security', 'vulnerability', 'high-severity'])"
                    }
                },
                "required": ["title", "content"]
            }
        }),
        json!({
            "name": "cite",
            "description": "Cite another publication to reference prior work. This helps build consensus and shows agreement or disagreement with other findings.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "publication_id": {
                        "type": "string",
                        "description": "The ID of your publication that will cite another (e.g., 'pub_5')"
                    },
                    "cited_id": {
                        "type": "string",
                        "description": "The ID of the publication being cited (e.g., 'pub_3')"
                    }
                },
                "required": ["publication_id", "cited_id"]
            }
        }),
        json!({
            "name": "review",
            "description": "Review and grade another agent's publication. Provide constructive feedback and a grade.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "publication_id": {
                        "type": "string",
                        "description": "The ID of the publication to review"
                    },
                    "grade": {
                        "type": "string",
                        "enum": ["STRONG_ACCEPT", "ACCEPT", "NEUTRAL", "REJECT", "STRONG_REJECT"],
                        "description": "Your grade for the publication:\n- STRONG_ACCEPT: Excellent, highly valuable\n- ACCEPT: Good, should be included\n- NEUTRAL: Neither particularly good nor bad\n- REJECT: Has issues, needs improvement\n- STRONG_REJECT: Fundamentally flawed or incorrect"
                    },
                    "comment": {
                        "type": "string",
                        "description": "Your review comment explaining the grade and providing feedback"
                    }
                },
                "required": ["publication_id", "grade", "comment"]
            }
        }),
        json!({
            "name": "search_publications",
            "description": "Search for publications by query. Searches titles, content, and tags.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query (searches title, content, and tags)"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "get_publication",
            "description": "Get the full details of a specific publication by ID.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "publication_id": {
                        "type": "string",
                        "description": "The ID of the publication to retrieve"
                    }
                },
                "required": ["publication_id"]
            }
        }),
        json!({
            "name": "list_publications",
            "description": "List all publications, optionally filtered by author or node.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "author": {
                        "type": "string",
                        "description": "Filter by author agent ID (optional)"
                    },
                    "node": {
                        "type": "string",
                        "description": "Filter by flow node ID (optional)"
                    },
                    "consensus_only": {
                        "type": "boolean",
                        "description": "Only return publications that have reached consensus (optional)"
                    }
                }
            }
        }),
        json!({
            "name": "citation_graph",
            "description": "Get the citation graph showing which publications cite which. Useful for understanding relationships between findings.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "publication_id": {
                        "type": "string",
                        "description": "Get citation info for a specific publication (optional, omit for full graph)"
                    }
                }
            }
        }),
        json!({
            "name": "revise",
            "description": "Revise a publication you authored. Creates a new version that supersedes the original. The original will be marked as superseded and excluded from consensus.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "publication_id": {
                        "type": "string",
                        "description": "The ID of your publication to revise"
                    },
                    "title": {
                        "type": "string",
                        "description": "Updated title"
                    },
                    "content": {
                        "type": "string",
                        "description": "Updated content"
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Updated tags"
                    }
                },
                "required": ["publication_id", "title", "content"]
            }
        }),
        json!({
            "name": "remember",
            "description": "Store a key insight or lesson learned for future reference. Memories persist across agent instances within this flow node.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Short label for the memory (e.g., 'auth-pattern', 'error-handling')"
                    },
                    "content": {
                        "type": "string",
                        "description": "The insight or lesson to remember"
                    }
                },
                "required": ["key", "content"]
            }
        }),
        json!({
            "name": "recall",
            "description": "Search your memories for insights relevant to the current task. Returns memories stored by previous agent instances on this node.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search keyword to filter memories (optional, omit for all memories)"
                    }
                }
            }
        }),
    ]
}

/// Input for the publish tool
#[derive(Debug, Deserialize)]
pub struct PublishInput {
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Input for the cite tool
#[derive(Debug, Deserialize)]
pub struct CiteInput {
    pub publication_id: String,
    pub cited_id: String,
}

/// Input for the review tool
#[derive(Debug, Deserialize)]
pub struct ReviewInput {
    pub publication_id: String,
    pub grade: String,
    pub comment: String,
}

/// Input for the search_publications tool
#[derive(Debug, Deserialize)]
pub struct SearchInput {
    pub query: String,
}

/// Input for the get_publication tool
#[derive(Debug, Deserialize)]
pub struct GetPublicationInput {
    pub publication_id: String,
}

/// Input for the list_publications tool
#[derive(Debug, Deserialize)]
pub struct ListPublicationsInput {
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub consensus_only: bool,
}

/// Input for the citation_graph tool
#[derive(Debug, Deserialize)]
pub struct CitationGraphInput {
    #[serde(default)]
    pub publication_id: Option<String>,
}

/// Input for the revise tool
#[derive(Debug, Deserialize)]
pub struct ReviseInput {
    pub publication_id: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Input for the remember tool
#[derive(Debug, Deserialize)]
pub struct RememberInput {
    pub key: String,
    pub content: String,
}

/// Input for the recall tool
#[derive(Debug, Deserialize)]
pub struct RecallInput {
    #[serde(default)]
    pub query: Option<String>,
}

/// Result of a publication tool operation
#[derive(Debug, Serialize)]
pub struct PublicationToolResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl PublicationToolResult {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
            publication_id: None,
            data: None,
        }
    }

    pub fn ok_with_id(message: impl Into<String>, id: String) -> Self {
        Self {
            success: true,
            message: message.into(),
            publication_id: Some(id),
            data: None,
        }
    }

    pub fn ok_with_data(message: impl Into<String>, data: Value) -> Self {
        Self {
            success: true,
            message: message.into(),
            publication_id: None,
            data: Some(data),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: message.into(),
            publication_id: None,
            data: None,
        }
    }
}

/// Handler for publication tools
pub struct PublicationToolHandler {
    store: Arc<PublicationStore>,
    memory_store: Option<Arc<MemoryStore>>,
    agent_id: String,
    node_id: String,
}

impl PublicationToolHandler {
    pub fn new(store: Arc<PublicationStore>, agent_id: String, node_id: String) -> Self {
        Self {
            store,
            memory_store: None,
            agent_id,
            node_id,
        }
    }

    /// Set the memory store (builder pattern)
    pub fn with_memory(mut self, memory_store: Arc<MemoryStore>) -> Self {
        self.memory_store = Some(memory_store);
        self
    }

    /// Execute a publication tool
    pub async fn execute(&self, tool_name: &str, input: &Value) -> PublicationToolResult {
        match tool_name {
            "publish" => self.handle_publish(input).await,
            "cite" => self.handle_cite(input).await,
            "review" => self.handle_review(input).await,
            "search_publications" => self.handle_search(input).await,
            "get_publication" => self.handle_get(input).await,
            "list_publications" => self.handle_list(input).await,
            "citation_graph" => self.handle_citation_graph(input).await,
            "revise" => self.handle_revise(input).await,
            "remember" => self.handle_remember(input).await,
            "recall" => self.handle_recall(input).await,
            _ => PublicationToolResult::error(format!("Unknown publication tool: {}", tool_name)),
        }
    }

    async fn handle_publish(&self, input: &Value) -> PublicationToolResult {
        let input: PublishInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        let id = self
            .store
            .publish(
                &self.agent_id,
                &self.node_id,
                &input.title,
                &input.content,
                input.tags,
            )
            .await;

        PublicationToolResult::ok_with_id(
            format!("Published '{}' with ID {}", input.title, id),
            id,
        )
    }

    async fn handle_cite(&self, input: &Value) -> PublicationToolResult {
        let input: CiteInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        match self
            .store
            .cite(&input.publication_id, &input.cited_id)
            .await
        {
            Ok(()) => PublicationToolResult::ok(format!(
                "Added citation from {} to {}",
                input.publication_id, input.cited_id
            )),
            Err(e) => PublicationToolResult::error(e.to_string()),
        }
    }

    async fn handle_review(&self, input: &Value) -> PublicationToolResult {
        let input: ReviewInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        let grade = match Grade::from_str(&input.grade) {
            Some(g) => g,
            None => {
                return PublicationToolResult::error(format!(
                    "Invalid grade '{}'. Must be one of: STRONG_ACCEPT, ACCEPT, NEUTRAL, REJECT, STRONG_REJECT",
                    input.grade
                ))
            }
        };

        // Get the publication's node_id before reviewing (for auto-storing feedback)
        let pub_node_id = self.store.get(&input.publication_id).await.map(|p| p.node_id.clone());

        match self
            .store
            .review(
                &input.publication_id,
                &self.agent_id,
                &self.node_id,
                grade,
                &input.comment,
            )
            .await
        {
            Ok(()) => {
                // Auto-store review feedback as memory on the reviewed publication's node
                if let (Some(ref memory_store), Some(pub_node)) = (&self.memory_store, pub_node_id) {
                    let key = format!("review-{}-{}", input.publication_id, grade);
                    memory_store.remember(
                        &pub_node,
                        &key,
                        &input.comment,
                        MemorySource::ReviewFeedback,
                    ).await;
                }

                PublicationToolResult::ok(format!(
                    "Submitted {} review for {}",
                    grade, input.publication_id
                ))
            }
            Err(e) => PublicationToolResult::error(e.to_string()),
        }
    }

    async fn handle_search(&self, input: &Value) -> PublicationToolResult {
        let input: SearchInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        let results = self.store.search(&input.query).await;
        let formatted = self.store.format_for_context(&results).await;

        PublicationToolResult::ok_with_data(
            format!("Found {} publications matching '{}'", results.len(), input.query),
            json!({
                "count": results.len(),
                "publications": formatted
            }),
        )
    }

    async fn handle_get(&self, input: &Value) -> PublicationToolResult {
        let input: GetPublicationInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        match self.store.get(&input.publication_id).await {
            Some(pub_item) => {
                let formatted = self.store.format_for_context(&[pub_item.clone()]).await;
                PublicationToolResult::ok_with_data(
                    format!("Found publication {}", input.publication_id),
                    json!({
                        "publication": formatted,
                        "reviews": pub_item.reviews.len(),
                        "consensus_score": pub_item.consensus_score()
                    }),
                )
            }
            None => PublicationToolResult::error(format!(
                "Publication '{}' not found",
                input.publication_id
            )),
        }
    }

    async fn handle_list(&self, input: &Value) -> PublicationToolResult {
        let input: ListPublicationsInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        let mut results = if let Some(ref author) = input.author {
            self.store.by_author(author).await
        } else if let Some(ref node) = input.node {
            self.store.by_node(node).await
        } else if input.consensus_only {
            self.store.consensus_publications().await
        } else {
            self.store.all().await
        };

        // If consensus_only and we had other filters, filter again
        if input.consensus_only && (input.author.is_some() || input.node.is_some()) {
            let threshold = self.store.consensus_threshold();
            results.retain(|p| !p.is_superseded() && p.meets_consensus(threshold));
        }

        let formatted = self.store.format_for_context(&results).await;
        let stats = self.store.stats().await;

        PublicationToolResult::ok_with_data(
            format!("Found {} publications", results.len()),
            json!({
                "count": results.len(),
                "publications": formatted,
                "stats": {
                    "total": stats.total_publications,
                    "with_consensus": stats.publications_with_consensus,
                    "total_reviews": stats.total_reviews,
                    "total_citations": stats.total_citations
                }
            }),
        )
    }

    async fn handle_citation_graph(&self, input: &Value) -> PublicationToolResult {
        let input: CitationGraphInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        let graph = self.store.citation_graph(input.publication_id.as_deref()).await;

        PublicationToolResult::ok_with_data(
            "Citation graph retrieved",
            json!({ "graph": graph }),
        )
    }

    async fn handle_revise(&self, input: &Value) -> PublicationToolResult {
        let input: ReviseInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        match self
            .store
            .revise(
                &input.publication_id,
                &self.agent_id,
                &self.node_id,
                &input.title,
                &input.content,
                input.tags,
            )
            .await
        {
            Ok(new_id) => PublicationToolResult::ok_with_id(
                format!(
                    "Revised {} -> new version {} '{}'",
                    input.publication_id, new_id, input.title
                ),
                new_id,
            ),
            Err(e) => PublicationToolResult::error(e.to_string()),
        }
    }

    async fn handle_remember(&self, input: &Value) -> PublicationToolResult {
        let input: RememberInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        match &self.memory_store {
            Some(store) => {
                store.remember(
                    &self.node_id,
                    &input.key,
                    &input.content,
                    MemorySource::Agent(self.agent_id.clone()),
                ).await;
                PublicationToolResult::ok(format!("Stored memory '{}'", input.key))
            }
            None => PublicationToolResult::error("Memory store not available in this flow"),
        }
    }

    async fn handle_recall(&self, input: &Value) -> PublicationToolResult {
        let input: RecallInput = match serde_json::from_value(input.clone()) {
            Ok(i) => i,
            Err(e) => return PublicationToolResult::error(format!("Invalid input: {}", e)),
        };

        match &self.memory_store {
            Some(store) => {
                let memories = if let Some(ref query) = input.query {
                    store.recall(&self.node_id, query).await
                } else {
                    store.recall_all(&self.node_id).await
                };

                if memories.is_empty() {
                    PublicationToolResult::ok("No memories found")
                } else {
                    let formatted: Vec<Value> = memories.iter().map(|m| {
                        json!({
                            "key": m.key,
                            "content": m.content,
                            "source": match &m.source {
                                MemorySource::Agent(id) => format!("agent:{}", id),
                                MemorySource::ReviewFeedback => "review-feedback".to_string(),
                            }
                        })
                    }).collect();

                    PublicationToolResult::ok_with_data(
                        format!("Found {} memories", memories.len()),
                        json!({ "memories": formatted }),
                    )
                }
            }
            None => PublicationToolResult::error("Memory store not available in this flow"),
        }
    }

    /// Check if a tool name is a publication tool
    pub fn is_publication_tool(name: &str) -> bool {
        matches!(
            name,
            "publish" | "cite" | "review" | "search_publications" | "get_publication"
                | "list_publications" | "citation_graph" | "revise" | "remember" | "recall"
        )
    }
}

/// Implement CustomToolHandler trait for integration with Agent
#[async_trait]
impl CustomToolHandler for PublicationToolHandler {
    fn handles(&self, tool_name: &str) -> bool {
        Self::is_publication_tool(tool_name)
    }

    async fn execute(&self, tool_name: &str, input: &Value) -> crate::agent::CustomToolResult {
        let result = self.execute(tool_name, input).await;
        crate::agent::CustomToolResult {
            output: serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.message.clone()),
            success: result.success,
        }
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        publication_tool_definitions()
            .into_iter()
            .map(|def| ToolDefinition {
                name: def["name"].as_str().unwrap_or("").to_string(),
                description: def["description"].as_str().unwrap_or("").to_string(),
                input_schema: def["input_schema"].clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_publish_tool() {
        let store = Arc::new(PublicationStore::new(2));
        let handler = PublicationToolHandler::new(store, "agent1".to_string(), "node1".to_string());

        let result = handler
            .execute(
                "publish",
                &json!({
                    "title": "Test Finding",
                    "content": "This is a test finding",
                    "tags": ["test", "example"]
                }),
            )
            .await;

        assert!(result.success);
        assert!(result.publication_id.is_some());
    }

    #[tokio::test]
    async fn test_review_tool() {
        let store = Arc::new(PublicationStore::new(2));

        // First publish something
        let pub_id = store
            .publish("author1", "node1", "Test", "Content", vec![])
            .await;

        // Then review it
        let handler = PublicationToolHandler::new(store, "reviewer1".to_string(), "node2".to_string());

        let result = handler
            .execute(
                "review",
                &json!({
                    "publication_id": pub_id,
                    "grade": "ACCEPT",
                    "comment": "Good work!"
                }),
            )
            .await;

        assert!(result.success);
    }

    #[tokio::test]
    async fn test_search_tool() {
        let store = Arc::new(PublicationStore::new(2));

        // Publish some findings
        store
            .publish("a1", "n1", "SQL Injection Found", "Found SQL injection in login", vec!["security".to_string()])
            .await;
        store
            .publish("a2", "n2", "XSS Vulnerability", "Found XSS in comments", vec!["security".to_string()])
            .await;

        let handler = PublicationToolHandler::new(store, "searcher".to_string(), "node3".to_string());

        let result = handler
            .execute("search_publications", &json!({ "query": "SQL" }))
            .await;

        assert!(result.success);
        assert!(result.data.is_some());
        let data = result.data.unwrap();
        assert_eq!(data["count"], 1);
    }

    #[tokio::test]
    async fn test_self_review_blocked() {
        let store = Arc::new(PublicationStore::new(2));

        // Publish as agent1
        let pub_id = store
            .publish("agent1", "node1", "Test", "Content", vec![])
            .await;

        // Try to review own publication
        let handler = PublicationToolHandler::new(store, "agent1".to_string(), "node1".to_string());

        let result = handler
            .execute(
                "review",
                &json!({
                    "publication_id": pub_id,
                    "grade": "STRONG_ACCEPT",
                    "comment": "Self review"
                }),
            )
            .await;

        assert!(!result.success);
        assert!(result.message.contains("Cannot review own publication"));
    }

    #[test]
    fn test_tool_definitions() {
        let defs = publication_tool_definitions();
        assert_eq!(defs.len(), 10); // 6 original + citation_graph + revise + remember + recall

        let names: Vec<&str> = defs
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"publish"));
        assert!(names.contains(&"review"));
        assert!(names.contains(&"search_publications"));
        assert!(names.contains(&"citation_graph"));
        assert!(names.contains(&"revise"));
        assert!(names.contains(&"remember"));
        assert!(names.contains(&"recall"));
    }

    #[tokio::test]
    async fn test_citation_graph_tool() {
        let store = Arc::new(PublicationStore::new(2));

        let id1 = store.publish("a1", "n1", "Finding A", "C1", vec![]).await;
        let id2 = store.publish("a2", "n2", "Finding B", "C2", vec![]).await;
        store.cite(&id2, &id1).await.unwrap();

        let handler = PublicationToolHandler::new(store, "agent".to_string(), "node".to_string());

        let result = handler.execute("citation_graph", &json!({})).await;
        assert!(result.success);
        let data = result.data.unwrap();
        let graph = data["graph"].as_str().unwrap();
        assert!(graph.contains("Finding A"));
        assert!(graph.contains("cited by"));
    }

    #[tokio::test]
    async fn test_revise_tool() {
        let store = Arc::new(PublicationStore::new(2));

        let pub_id = store.publish("agent1", "node1", "Draft", "Draft content", vec![]).await;

        let handler = PublicationToolHandler::new(store.clone(), "agent1".to_string(), "node1".to_string());

        let result = handler
            .execute(
                "revise",
                &json!({
                    "publication_id": pub_id,
                    "title": "Final Version",
                    "content": "Improved content",
                    "tags": ["revised"]
                }),
            )
            .await;

        assert!(result.success);
        assert!(result.publication_id.is_some());
        let new_id = result.publication_id.unwrap();

        // Verify original is superseded
        let orig = store.get(&pub_id).await.unwrap();
        assert!(orig.is_superseded());
        assert_eq!(orig.superseded_by, Some(new_id.clone()));

        // Verify new version
        let revised = store.get(&new_id).await.unwrap();
        assert_eq!(revised.version, 2);
        assert_eq!(revised.title, "Final Version");
    }

    #[tokio::test]
    async fn test_revise_tool_not_author() {
        let store = Arc::new(PublicationStore::new(2));

        let pub_id = store.publish("agent1", "node1", "Draft", "Content", vec![]).await;

        // Try to revise as different agent
        let handler = PublicationToolHandler::new(store, "agent2".to_string(), "node2".to_string());

        let result = handler
            .execute(
                "revise",
                &json!({
                    "publication_id": pub_id,
                    "title": "Hijacked",
                    "content": "Not yours"
                }),
            )
            .await;

        assert!(!result.success);
        assert!(result.message.contains("original author"));
    }

    #[tokio::test]
    async fn test_remember_recall_tools() {
        let store = Arc::new(PublicationStore::new(2));
        let memory_store = Arc::new(MemoryStore::new());

        let handler = PublicationToolHandler::new(store, "agent1".to_string(), "node1".to_string())
            .with_memory(memory_store.clone());

        // Remember something
        let result = handler
            .execute(
                "remember",
                &json!({ "key": "pattern", "content": "Use dependency injection" }),
            )
            .await;
        assert!(result.success);

        // Recall it
        let result = handler.execute("recall", &json!({ "query": "injection" })).await;
        assert!(result.success);
        let data = result.data.unwrap();
        let memories = data["memories"].as_array().unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0]["key"], "pattern");

        // Recall all
        let result = handler.execute("recall", &json!({})).await;
        assert!(result.success);
        let data = result.data.unwrap();
        let memories = data["memories"].as_array().unwrap();
        assert_eq!(memories.len(), 1);
    }

    #[tokio::test]
    async fn test_remember_without_memory_store() {
        let store = Arc::new(PublicationStore::new(2));
        let handler = PublicationToolHandler::new(store, "agent1".to_string(), "node1".to_string());
        // No memory store attached

        let result = handler
            .execute("remember", &json!({ "key": "k", "content": "v" }))
            .await;
        assert!(!result.success);
        assert!(result.message.contains("not available"));
    }

    #[tokio::test]
    async fn test_review_auto_stores_feedback() {
        let store = Arc::new(PublicationStore::new(2));
        let memory_store = Arc::new(MemoryStore::new());

        // Publish as agent1
        let pub_id = store.publish("agent1", "node1", "Finding", "Content", vec![]).await;

        // Review as agent2 with memory store
        let handler = PublicationToolHandler::new(store, "agent2".to_string(), "node2".to_string())
            .with_memory(memory_store.clone());

        let result = handler
            .execute(
                "review",
                &json!({
                    "publication_id": pub_id,
                    "grade": "ACCEPT",
                    "comment": "Good but needs more error handling"
                }),
            )
            .await;
        assert!(result.success);

        // Check that feedback was stored as memory on the publication's node
        let memories = memory_store.recall_all("node1").await;
        assert_eq!(memories.len(), 1);
        assert!(memories[0].content.contains("error handling"));
        assert!(matches!(memories[0].source, MemorySource::ReviewFeedback));
    }

    #[tokio::test]
    async fn test_list_with_consensus_filter_excludes_superseded() {
        let store = Arc::new(PublicationStore::new(1));

        let id1 = store.publish("a1", "n1", "F1", "C1", vec![]).await;
        store.review(&id1, "r1", "n2", Grade::Accept, "OK").await.unwrap();

        // Revise id1
        let _id2 = store.revise(&id1, "a1", "n1", "F1 v2", "Better", vec![]).await.unwrap();

        let handler = PublicationToolHandler::new(store, "agent".to_string(), "node".to_string());

        // List with consensus_only should exclude superseded id1
        let result = handler
            .execute("list_publications", &json!({ "consensus_only": true }))
            .await;
        assert!(result.success);
        let data = result.data.unwrap();
        assert_eq!(data["count"], 0); // id1 superseded, id2 has no reviews
    }
}
