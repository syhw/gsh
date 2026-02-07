//! Publication store for multi-agent coordination
//!
//! Implements a publication/review pattern where agents can:
//! - Publish findings
//! - Review and grade other agents' publications
//! - Cite prior work
//! - Build consensus through peer review

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock;

/// Unique identifier for a publication
pub type PublicationId = String;

/// A publication submitted by an agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publication {
    /// Unique identifier
    pub id: PublicationId,
    /// Agent that created this publication
    pub author: String,
    /// Node ID where this was created
    pub node_id: String,
    /// Title/summary of the finding
    pub title: String,
    /// Full content of the finding
    pub content: String,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Publications this one cites
    pub citations: Vec<PublicationId>,
    /// When this was published
    pub published_at: DateTime<Utc>,
    /// Reviews received
    pub reviews: Vec<Review>,
}

impl Publication {
    /// Calculate consensus score based on reviews
    pub fn consensus_score(&self) -> i32 {
        self.reviews.iter().map(|r| r.grade.score()).sum()
    }

    /// Count of ACCEPT or STRONG_ACCEPT reviews
    pub fn accept_count(&self) -> usize {
        self.reviews
            .iter()
            .filter(|r| matches!(r.grade, Grade::Accept | Grade::StrongAccept))
            .count()
    }

    /// Count of REJECT or STRONG_REJECT reviews
    pub fn reject_count(&self) -> usize {
        self.reviews
            .iter()
            .filter(|r| matches!(r.grade, Grade::Reject | Grade::StrongReject))
            .count()
    }

    /// Check if publication meets consensus threshold
    pub fn meets_consensus(&self, threshold: usize) -> bool {
        self.accept_count() >= threshold
    }
}

/// A review of a publication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    /// Reviewer agent ID
    pub reviewer: String,
    /// Node ID where review was made
    pub node_id: String,
    /// Grade given
    pub grade: Grade,
    /// Review comment/reasoning
    pub comment: String,
    /// When the review was submitted
    pub reviewed_at: DateTime<Utc>,
}

/// Grades for publication review
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Grade {
    StrongAccept,
    Accept,
    Neutral,
    Reject,
    StrongReject,
}

impl Grade {
    /// Convert grade to numeric score
    pub fn score(&self) -> i32 {
        match self {
            Grade::StrongAccept => 2,
            Grade::Accept => 1,
            Grade::Neutral => 0,
            Grade::Reject => -1,
            Grade::StrongReject => -2,
        }
    }

    /// Parse grade from string
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().replace(['-', '_'], "_").as_str() {
            "STRONG_ACCEPT" | "STRONGACCEPT" => Some(Grade::StrongAccept),
            "ACCEPT" => Some(Grade::Accept),
            "NEUTRAL" => Some(Grade::Neutral),
            "REJECT" => Some(Grade::Reject),
            "STRONG_REJECT" | "STRONGREJECT" => Some(Grade::StrongReject),
            _ => None,
        }
    }
}

impl std::fmt::Display for Grade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Grade::StrongAccept => write!(f, "STRONG_ACCEPT"),
            Grade::Accept => write!(f, "ACCEPT"),
            Grade::Neutral => write!(f, "NEUTRAL"),
            Grade::Reject => write!(f, "REJECT"),
            Grade::StrongReject => write!(f, "STRONG_REJECT"),
        }
    }
}

/// Thread-safe publication store
pub struct PublicationStore {
    /// All publications, indexed by ID
    publications: RwLock<HashMap<PublicationId, Publication>>,
    /// Counter for generating unique IDs
    next_id: AtomicU64,
    /// Consensus threshold for filtering
    consensus_threshold: usize,
    /// Whether agents can review their own publications
    allow_self_review: bool,
}

impl PublicationStore {
    /// Create a new publication store
    pub fn new(consensus_threshold: usize) -> Self {
        Self {
            publications: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            consensus_threshold,
            allow_self_review: false,
        }
    }

    /// Set whether self-review is allowed (builder pattern)
    pub fn with_self_review(mut self, allow: bool) -> Self {
        self.allow_self_review = allow;
        self
    }

    /// Get the consensus threshold
    pub fn consensus_threshold(&self) -> usize {
        self.consensus_threshold
    }

    /// Publish a new finding
    pub async fn publish(
        &self,
        author: &str,
        node_id: &str,
        title: &str,
        content: &str,
        tags: Vec<String>,
    ) -> PublicationId {
        let id = format!("pub_{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        let publication = Publication {
            id: id.clone(),
            author: author.to_string(),
            node_id: node_id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            tags,
            citations: Vec::new(),
            published_at: Utc::now(),
            reviews: Vec::new(),
        };

        let mut pubs = self.publications.write().await;
        pubs.insert(id.clone(), publication);

        id
    }

    /// Add a citation to a publication
    pub async fn cite(
        &self,
        publication_id: &str,
        cited_id: &str,
    ) -> Result<(), PublicationError> {
        let mut pubs = self.publications.write().await;

        // Verify cited publication exists
        if !pubs.contains_key(cited_id) {
            return Err(PublicationError::NotFound(cited_id.to_string()));
        }

        // Add citation
        let pub_entry = pubs
            .get_mut(publication_id)
            .ok_or_else(|| PublicationError::NotFound(publication_id.to_string()))?;

        if !pub_entry.citations.contains(&cited_id.to_string()) {
            pub_entry.citations.push(cited_id.to_string());
        }

        Ok(())
    }

    /// Review a publication
    pub async fn review(
        &self,
        publication_id: &str,
        reviewer: &str,
        node_id: &str,
        grade: Grade,
        comment: &str,
    ) -> Result<(), PublicationError> {
        let mut pubs = self.publications.write().await;

        let publication = pubs
            .get_mut(publication_id)
            .ok_or_else(|| PublicationError::NotFound(publication_id.to_string()))?;

        // Check if this reviewer already reviewed
        if publication.reviews.iter().any(|r| r.reviewer == reviewer) {
            return Err(PublicationError::AlreadyReviewed {
                publication_id: publication_id.to_string(),
                reviewer: reviewer.to_string(),
            });
        }

        // Can't review own publication (unless allow_self_review is set)
        if publication.author == reviewer && !self.allow_self_review {
            return Err(PublicationError::SelfReview);
        }

        publication.reviews.push(Review {
            reviewer: reviewer.to_string(),
            node_id: node_id.to_string(),
            grade,
            comment: comment.to_string(),
            reviewed_at: Utc::now(),
        });

        Ok(())
    }

    /// Get a publication by ID
    pub async fn get(&self, id: &str) -> Option<Publication> {
        let pubs = self.publications.read().await;
        pubs.get(id).cloned()
    }

    /// Search publications by query (searches title, content, tags)
    pub async fn search(&self, query: &str) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        let query_lower = query.to_lowercase();

        pubs.values()
            .filter(|p| {
                p.title.to_lowercase().contains(&query_lower)
                    || p.content.to_lowercase().contains(&query_lower)
                    || p.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
            })
            .cloned()
            .collect()
    }

    /// Get publications by author
    pub async fn by_author(&self, author: &str) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        pubs.values()
            .filter(|p| p.author == author)
            .cloned()
            .collect()
    }

    /// Get publications by node
    pub async fn by_node(&self, node_id: &str) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        pubs.values()
            .filter(|p| p.node_id == node_id)
            .cloned()
            .collect()
    }

    /// Get all publications
    pub async fn all(&self) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        pubs.values().cloned().collect()
    }

    /// Get publications that meet consensus threshold
    pub async fn consensus_publications(&self) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        pubs.values()
            .filter(|p| p.meets_consensus(self.consensus_threshold))
            .cloned()
            .collect()
    }

    /// Get publications from specific nodes that meet consensus
    pub async fn consensus_from_nodes(&self, node_ids: &[String]) -> Vec<Publication> {
        let pubs = self.publications.read().await;
        pubs.values()
            .filter(|p| {
                node_ids.contains(&p.node_id) && p.meets_consensus(self.consensus_threshold)
            })
            .cloned()
            .collect()
    }

    /// Format publications for agent context
    pub async fn format_for_context(&self, publications: &[Publication]) -> String {
        let mut output = String::new();

        for pub_item in publications {
            output.push_str(&format!("\n--- Publication {} ---\n", pub_item.id));
            output.push_str(&format!("Title: {}\n", pub_item.title));
            output.push_str(&format!("Author: {} (node: {})\n", pub_item.author, pub_item.node_id));
            output.push_str(&format!("Tags: {}\n", pub_item.tags.join(", ")));
            output.push_str(&format!(
                "Reviews: {} accepts, {} rejects (score: {})\n",
                pub_item.accept_count(),
                pub_item.reject_count(),
                pub_item.consensus_score()
            ));
            output.push_str(&format!("\n{}\n", pub_item.content));

            if !pub_item.citations.is_empty() {
                output.push_str(&format!("Cites: {}\n", pub_item.citations.join(", ")));
            }
        }

        output
    }

    /// Get summary statistics
    pub async fn stats(&self) -> PublicationStats {
        let pubs = self.publications.read().await;

        let total = pubs.len();
        let with_consensus = pubs
            .values()
            .filter(|p| p.meets_consensus(self.consensus_threshold))
            .count();
        let total_reviews: usize = pubs.values().map(|p| p.reviews.len()).sum();

        PublicationStats {
            total_publications: total,
            publications_with_consensus: with_consensus,
            total_reviews,
            consensus_threshold: self.consensus_threshold,
        }
    }
}

/// Statistics about the publication store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicationStats {
    pub total_publications: usize,
    pub publications_with_consensus: usize,
    pub total_reviews: usize,
    pub consensus_threshold: usize,
}

/// Errors that can occur with publications
#[derive(Debug, Clone, thiserror::Error)]
pub enum PublicationError {
    #[error("Publication not found: {0}")]
    NotFound(String),

    #[error("Reviewer {reviewer} already reviewed publication {publication_id}")]
    AlreadyReviewed {
        publication_id: String,
        reviewer: String,
    },

    #[error("Cannot review own publication")]
    SelfReview,

    #[error("Invalid grade: {0}")]
    InvalidGrade(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_publish_and_get() {
        let store = PublicationStore::new(2);

        let id = store
            .publish("agent1", "node1", "Finding 1", "Content here", vec!["security".to_string()])
            .await;

        let pub_item = store.get(&id).await.unwrap();
        assert_eq!(pub_item.title, "Finding 1");
        assert_eq!(pub_item.author, "agent1");
    }

    #[tokio::test]
    async fn test_review() {
        let store = PublicationStore::new(2);

        let id = store
            .publish("agent1", "node1", "Finding", "Content", vec![])
            .await;

        store
            .review(&id, "reviewer1", "node2", Grade::Accept, "Good work")
            .await
            .unwrap();

        store
            .review(&id, "reviewer2", "node2", Grade::StrongAccept, "Excellent")
            .await
            .unwrap();

        let pub_item = store.get(&id).await.unwrap();
        assert_eq!(pub_item.reviews.len(), 2);
        assert_eq!(pub_item.accept_count(), 2);
        assert!(pub_item.meets_consensus(2));
    }

    #[tokio::test]
    async fn test_self_review_rejected() {
        let store = PublicationStore::new(2);

        let id = store
            .publish("agent1", "node1", "Finding", "Content", vec![])
            .await;

        let result = store
            .review(&id, "agent1", "node1", Grade::Accept, "Self review")
            .await;

        assert!(matches!(result, Err(PublicationError::SelfReview)));
    }

    #[tokio::test]
    async fn test_search() {
        let store = PublicationStore::new(2);

        store
            .publish("a1", "n1", "SQL Injection", "Found SQL injection in login", vec!["security".to_string()])
            .await;

        store
            .publish("a2", "n2", "Performance Issue", "Slow query in reports", vec!["performance".to_string()])
            .await;

        let results = store.search("sql").await;
        assert_eq!(results.len(), 1);
        assert!(results[0].title.contains("SQL"));

        let results = store.search("security").await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn test_consensus_filtering() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "Finding 1", "C1", vec![]).await;
        let id2 = store.publish("a2", "n2", "Finding 2", "C2", vec![]).await;

        // id1 gets 2 accepts
        store.review(&id1, "r1", "n3", Grade::Accept, "").await.unwrap();
        store.review(&id1, "r2", "n3", Grade::Accept, "").await.unwrap();

        // id2 gets only 1 accept
        store.review(&id2, "r1", "n3", Grade::Accept, "").await.unwrap();

        let consensus = store.consensus_publications().await;
        assert_eq!(consensus.len(), 1);
        assert_eq!(consensus[0].id, id1);
    }

    #[test]
    fn test_grade_parsing() {
        assert_eq!(Grade::from_str("ACCEPT"), Some(Grade::Accept));
        assert_eq!(Grade::from_str("strong-accept"), Some(Grade::StrongAccept));
        assert_eq!(Grade::from_str("STRONG_REJECT"), Some(Grade::StrongReject));
        assert_eq!(Grade::from_str("invalid"), None);
    }
}
