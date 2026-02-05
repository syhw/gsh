//! Publication tools for agents in publication coordination mode
//!
//! These tools allow agents to:
//! - Publish findings
//! - Cite other agents' work
//! - Review and grade publications
//! - Search for relevant publications

use super::publication::{Grade, PublicationStore};
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
    agent_id: String,
    node_id: String,
}

impl PublicationToolHandler {
    pub fn new(store: Arc<PublicationStore>, agent_id: String, node_id: String) -> Self {
        Self {
            store,
            agent_id,
            node_id,
        }
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
            Ok(()) => PublicationToolResult::ok(format!(
                "Submitted {} review for {}",
                grade, input.publication_id
            )),
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
            let threshold = 2; // TODO: Get from store
            results.retain(|p| p.meets_consensus(threshold));
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
                    "total_reviews": stats.total_reviews
                }
            }),
        )
    }

    /// Check if a tool name is a publication tool
    pub fn is_publication_tool(name: &str) -> bool {
        matches!(
            name,
            "publish" | "cite" | "review" | "search_publications" | "get_publication" | "list_publications"
        )
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
        assert_eq!(defs.len(), 6);

        let names: Vec<&str> = defs
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"publish"));
        assert!(names.contains(&"review"));
        assert!(names.contains(&"search_publications"));
    }
}
