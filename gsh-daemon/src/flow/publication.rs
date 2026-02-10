//! Publication store for multi-agent coordination
//!
//! Implements a publication/review pattern where agents can:
//! - Publish findings
//! - Review and grade other agents' publications
//! - Cite prior work (with reverse citation tracking)
//! - Revise publications (version chains with supersedes links)
//! - Build consensus through peer review (with weighted scoring)

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
    /// ID of the publication this one supersedes (revision chain)
    pub supersedes: Option<PublicationId>,
    /// ID of the publication that superseded this one
    pub superseded_by: Option<PublicationId>,
    /// Version number (1 = original, increments on revision)
    pub version: u32,
}

impl Publication {
    /// Calculate consensus score based on reviews
    pub fn consensus_score(&self) -> i32 {
        self.reviews.iter().map(|r| r.grade.score()).sum()
    }

    /// Calculate weighted consensus score including citation impact
    pub fn weighted_consensus_score(&self, citation_count: usize, citation_weight: f64) -> f64 {
        self.consensus_score() as f64 + (citation_count as f64 * citation_weight)
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

    /// Check if this publication has been superseded by a newer version
    pub fn is_superseded(&self) -> bool {
        self.superseded_by.is_some()
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

/// Internal storage guarded by a single RwLock
struct StoreInner {
    /// All publications, indexed by ID
    publications: HashMap<PublicationId, Publication>,
    /// Reverse citation index: cited_id -> Vec<citing_id>
    cited_by: HashMap<PublicationId, Vec<PublicationId>>,
}

/// Thread-safe publication store
pub struct PublicationStore {
    inner: RwLock<StoreInner>,
    /// Counter for generating unique IDs
    next_id: AtomicU64,
    /// Consensus threshold for filtering
    consensus_threshold: usize,
    /// Whether agents can review their own publications
    allow_self_review: bool,
    /// Weight per citation for weighted consensus scoring
    citation_weight: f64,
}

impl PublicationStore {
    /// Create a new publication store
    pub fn new(consensus_threshold: usize) -> Self {
        Self {
            inner: RwLock::new(StoreInner {
                publications: HashMap::new(),
                cited_by: HashMap::new(),
            }),
            next_id: AtomicU64::new(1),
            consensus_threshold,
            allow_self_review: false,
            citation_weight: 0.5,
        }
    }

    /// Set whether self-review is allowed (builder pattern)
    pub fn with_self_review(mut self, allow: bool) -> Self {
        self.allow_self_review = allow;
        self
    }

    /// Set citation weight (builder pattern)
    pub fn with_citation_weight(mut self, weight: f64) -> Self {
        self.citation_weight = weight;
        self
    }

    /// Get the consensus threshold
    pub fn consensus_threshold(&self) -> usize {
        self.consensus_threshold
    }

    /// Get the citation weight
    pub fn citation_weight(&self) -> f64 {
        self.citation_weight
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
            supersedes: None,
            superseded_by: None,
            version: 1,
        };

        let mut store = self.inner.write().await;
        store.publications.insert(id.clone(), publication);

        id
    }

    /// Add a citation to a publication
    pub async fn cite(
        &self,
        publication_id: &str,
        cited_id: &str,
    ) -> Result<(), PublicationError> {
        let mut store = self.inner.write().await;

        // Verify cited publication exists
        if !store.publications.contains_key(cited_id) {
            return Err(PublicationError::NotFound(cited_id.to_string()));
        }

        // Add forward citation
        let pub_entry = store.publications
            .get_mut(publication_id)
            .ok_or_else(|| PublicationError::NotFound(publication_id.to_string()))?;

        if !pub_entry.citations.contains(&cited_id.to_string()) {
            pub_entry.citations.push(cited_id.to_string());

            // Maintain reverse citation index
            store.cited_by
                .entry(cited_id.to_string())
                .or_default()
                .push(publication_id.to_string());
        }

        Ok(())
    }

    /// Get publications that cite the given publication
    pub async fn cited_by(&self, id: &str) -> Vec<PublicationId> {
        let store = self.inner.read().await;
        store.cited_by.get(id).cloned().unwrap_or_default()
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
        let mut store = self.inner.write().await;

        let publication = store.publications
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

    /// Revise a publication, creating a new version that supersedes the original
    pub async fn revise(
        &self,
        original_id: &str,
        author: &str,
        node_id: &str,
        title: &str,
        content: &str,
        tags: Vec<String>,
    ) -> Result<PublicationId, PublicationError> {
        let new_id = format!("pub_{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        let mut store = self.inner.write().await;

        let original = store.publications
            .get(original_id)
            .ok_or_else(|| PublicationError::NotFound(original_id.to_string()))?;

        // Only the original author can revise
        if original.author != author {
            return Err(PublicationError::NotAuthor {
                publication_id: original_id.to_string(),
                author: author.to_string(),
            });
        }

        let new_version = original.version + 1;
        let carried_citations = original.citations.clone();

        let new_pub = Publication {
            id: new_id.clone(),
            author: author.to_string(),
            node_id: node_id.to_string(),
            title: title.to_string(),
            content: content.to_string(),
            tags,
            citations: carried_citations.clone(),
            published_at: Utc::now(),
            reviews: Vec::new(),
            supersedes: Some(original_id.to_string()),
            superseded_by: None,
            version: new_version,
        };

        // Mark the original as superseded
        if let Some(orig) = store.publications.get_mut(original_id) {
            orig.superseded_by = Some(new_id.clone());
        }

        // Update reverse citation index for carried citations
        for cited_id in &carried_citations {
            store.cited_by
                .entry(cited_id.clone())
                .or_default()
                .push(new_id.clone());
        }

        store.publications.insert(new_id.clone(), new_pub);

        Ok(new_id)
    }

    /// Get a publication by ID
    pub async fn get(&self, id: &str) -> Option<Publication> {
        let store = self.inner.read().await;
        store.publications.get(id).cloned()
    }

    /// Search publications by query (searches title, content, tags)
    pub async fn search(&self, query: &str) -> Vec<Publication> {
        let store = self.inner.read().await;
        let query_lower = query.to_lowercase();

        store.publications.values()
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
        let store = self.inner.read().await;
        store.publications.values()
            .filter(|p| p.author == author)
            .cloned()
            .collect()
    }

    /// Get publications by node
    pub async fn by_node(&self, node_id: &str) -> Vec<Publication> {
        let store = self.inner.read().await;
        store.publications.values()
            .filter(|p| p.node_id == node_id)
            .cloned()
            .collect()
    }

    /// Get all publications
    pub async fn all(&self) -> Vec<Publication> {
        let store = self.inner.read().await;
        store.publications.values().cloned().collect()
    }

    /// Get publications that meet consensus threshold (excludes superseded)
    pub async fn consensus_publications(&self) -> Vec<Publication> {
        let store = self.inner.read().await;
        store.publications.values()
            .filter(|p| !p.is_superseded() && p.meets_consensus(self.consensus_threshold))
            .cloned()
            .collect()
    }

    /// Get publications from specific nodes that meet consensus (excludes superseded)
    pub async fn consensus_from_nodes(&self, node_ids: &[String]) -> Vec<Publication> {
        let store = self.inner.read().await;
        store.publications.values()
            .filter(|p| {
                !p.is_superseded()
                    && node_ids.contains(&p.node_id)
                    && p.meets_consensus(self.consensus_threshold)
            })
            .cloned()
            .collect()
    }

    /// Format publications for agent context (includes citation and revision info)
    pub async fn format_for_context(&self, publications: &[Publication]) -> String {
        let store = self.inner.read().await;
        let mut output = String::new();

        for pub_item in publications {
            output.push_str(&format!("\n--- Publication {} (v{}) ---\n", pub_item.id, pub_item.version));
            output.push_str(&format!("Title: {}\n", pub_item.title));
            output.push_str(&format!("Author: {} (node: {})\n", pub_item.author, pub_item.node_id));
            output.push_str(&format!("Tags: {}\n", pub_item.tags.join(", ")));
            output.push_str(&format!(
                "Reviews: {} accepts, {} rejects (score: {})\n",
                pub_item.accept_count(),
                pub_item.reject_count(),
                pub_item.consensus_score()
            ));

            // Show citation info
            let cited_by_count = store.cited_by.get(&pub_item.id).map(|v| v.len()).unwrap_or(0);
            if cited_by_count > 0 {
                output.push_str(&format!("Cited by: {} publication(s)\n", cited_by_count));
            }

            // Show revision info
            if let Some(ref supersedes) = pub_item.supersedes {
                output.push_str(&format!("Supersedes: {}\n", supersedes));
            }
            if let Some(ref superseded_by) = pub_item.superseded_by {
                output.push_str(&format!("[SUPERSEDED by {}]\n", superseded_by));
            }

            output.push_str(&format!("\n{}\n", pub_item.content));

            if !pub_item.citations.is_empty() {
                output.push_str(&format!("Cites: {}\n", pub_item.citations.join(", ")));
            }
        }

        output
    }

    /// Generate a citation graph for all or a specific publication
    pub async fn citation_graph(&self, publication_id: Option<&str>) -> String {
        let store = self.inner.read().await;
        let mut output = String::new();

        let pubs_to_show: Vec<&Publication> = if let Some(id) = publication_id {
            store.publications.get(id).into_iter().collect()
        } else {
            store.publications.values().collect()
        };

        for pub_item in pubs_to_show {
            let cited_by = store.cited_by.get(&pub_item.id);
            let cited_by_str = match cited_by {
                Some(ids) if !ids.is_empty() => format!("cited by: {}", ids.join(", ")),
                _ => "(no citations)".to_string(),
            };
            output.push_str(&format!(
                "{} \"{}\" <- {}\n",
                pub_item.id, pub_item.title, cited_by_str
            ));

            if !pub_item.citations.is_empty() {
                output.push_str(&format!("  cites: {}\n", pub_item.citations.join(", ")));
            }
        }

        if output.is_empty() {
            output = "No publications found.\n".to_string();
        }

        output
    }

    /// Summarize consensus publications with weighted scores
    pub async fn summarize_consensus(&self) -> String {
        let store = self.inner.read().await;
        let mut output = String::new();

        let mut consensus_pubs: Vec<&Publication> = store.publications.values()
            .filter(|p| !p.is_superseded() && p.meets_consensus(self.consensus_threshold))
            .collect();

        if consensus_pubs.is_empty() {
            return "No publications have reached consensus yet.\n".to_string();
        }

        // Sort by weighted score (descending)
        consensus_pubs.sort_by(|a, b| {
            let a_cited = store.cited_by.get(&a.id).map(|v| v.len()).unwrap_or(0);
            let b_cited = store.cited_by.get(&b.id).map(|v| v.len()).unwrap_or(0);
            let a_score = a.weighted_consensus_score(a_cited, self.citation_weight);
            let b_score = b.weighted_consensus_score(b_cited, self.citation_weight);
            b_score.partial_cmp(&a_score).unwrap_or(std::cmp::Ordering::Equal)
        });

        output.push_str(&format!(
            "=== Consensus Summary ({} publications) ===\n\n",
            consensus_pubs.len()
        ));

        for (i, pub_item) in consensus_pubs.iter().enumerate() {
            let cited_count = store.cited_by.get(&pub_item.id).map(|v| v.len()).unwrap_or(0);
            let weighted = pub_item.weighted_consensus_score(cited_count, self.citation_weight);
            output.push_str(&format!(
                "{}. [{}] {} (score: {:.1}, reviews: {} accept/{} reject, citations: {})\n",
                i + 1,
                pub_item.id,
                pub_item.title,
                weighted,
                pub_item.accept_count(),
                pub_item.reject_count(),
                cited_count,
            ));
        }

        output
    }

    /// Get summary statistics
    pub async fn stats(&self) -> PublicationStats {
        let store = self.inner.read().await;

        let total = store.publications.len();
        let with_consensus = store.publications
            .values()
            .filter(|p| !p.is_superseded() && p.meets_consensus(self.consensus_threshold))
            .count();
        let total_reviews: usize = store.publications.values().map(|p| p.reviews.len()).sum();
        let total_citations: usize = store.cited_by.values().map(|v| v.len()).sum();

        PublicationStats {
            total_publications: total,
            publications_with_consensus: with_consensus,
            total_reviews,
            total_citations,
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
    pub total_citations: usize,
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

    #[error("Only the original author can revise publication {publication_id} (attempted by {author})")]
    NotAuthor {
        publication_id: String,
        author: String,
    },
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
        assert_eq!(pub_item.version, 1);
        assert!(pub_item.supersedes.is_none());
        assert!(pub_item.superseded_by.is_none());
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

    // --- New tests for citation tracking ---

    #[tokio::test]
    async fn test_cite_and_cited_by() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "Finding 1", "C1", vec![]).await;
        let id2 = store.publish("a2", "n2", "Finding 2", "C2", vec![]).await;
        let id3 = store.publish("a3", "n3", "Finding 3", "C3", vec![]).await;

        // id2 cites id1, id3 cites id1
        store.cite(&id2, &id1).await.unwrap();
        store.cite(&id3, &id1).await.unwrap();

        // Check forward citations
        let pub2 = store.get(&id2).await.unwrap();
        assert_eq!(pub2.citations, vec![id1.clone()]);

        // Check reverse citations
        let cited_by = store.cited_by(&id1).await;
        assert_eq!(cited_by.len(), 2);
        assert!(cited_by.contains(&id2));
        assert!(cited_by.contains(&id3));

        // id1 has no citations pointing to it from itself
        let cited_by_2 = store.cited_by(&id2).await;
        assert!(cited_by_2.is_empty());
    }

    #[tokio::test]
    async fn test_weighted_consensus_score() {
        let pub_item = Publication {
            id: "pub_1".to_string(),
            author: "a1".to_string(),
            node_id: "n1".to_string(),
            title: "Test".to_string(),
            content: "Content".to_string(),
            tags: vec![],
            citations: vec![],
            published_at: Utc::now(),
            reviews: vec![
                Review {
                    reviewer: "r1".to_string(),
                    node_id: "n2".to_string(),
                    grade: Grade::Accept,
                    comment: "Good".to_string(),
                    reviewed_at: Utc::now(),
                },
                Review {
                    reviewer: "r2".to_string(),
                    node_id: "n3".to_string(),
                    grade: Grade::StrongAccept,
                    comment: "Great".to_string(),
                    reviewed_at: Utc::now(),
                },
            ],
            supersedes: None,
            superseded_by: None,
            version: 1,
        };

        // consensus_score = 1 + 2 = 3
        assert_eq!(pub_item.consensus_score(), 3);

        // With 2 citations at weight 0.5: 3.0 + 2*0.5 = 4.0
        let weighted = pub_item.weighted_consensus_score(2, 0.5);
        assert!((weighted - 4.0).abs() < f64::EPSILON);
    }

    // --- New tests for revisions ---

    #[tokio::test]
    async fn test_revise_publication() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "Finding v1", "Original content", vec!["tag1".to_string()]).await;

        // Add a citation to the original
        let id_other = store.publish("a2", "n2", "Other", "Other content", vec![]).await;
        store.cite(&id1, &id_other).await.unwrap();

        // Revise
        let id2 = store.revise(&id1, "a1", "n1", "Finding v2", "Updated content", vec!["tag1".to_string(), "revised".to_string()]).await.unwrap();

        // Check original is superseded
        let orig = store.get(&id1).await.unwrap();
        assert_eq!(orig.superseded_by, Some(id2.clone()));
        assert!(orig.is_superseded());

        // Check new version
        let revised = store.get(&id2).await.unwrap();
        assert_eq!(revised.version, 2);
        assert_eq!(revised.supersedes, Some(id1.clone()));
        assert!(!revised.is_superseded());
        // Citations should be carried forward
        assert_eq!(revised.citations, vec![id_other.clone()]);
    }

    #[tokio::test]
    async fn test_revise_not_author() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "Finding", "Content", vec![]).await;

        let result = store.revise(&id1, "a2", "n2", "Revised", "New content", vec![]).await;
        assert!(matches!(result, Err(PublicationError::NotAuthor { .. })));
    }

    #[tokio::test]
    async fn test_superseded_excluded_from_consensus() {
        let store = PublicationStore::new(1);

        let id1 = store.publish("a1", "n1", "Finding v1", "Content", vec![]).await;
        store.review(&id1, "r1", "n2", Grade::Accept, "Good").await.unwrap();

        // id1 has consensus now
        assert_eq!(store.consensus_publications().await.len(), 1);

        // Revise id1 -> id2
        let id2 = store.revise(&id1, "a1", "n1", "Finding v2", "Better content", vec![]).await.unwrap();

        // id1 is superseded, so it should be excluded even though it has reviews
        let consensus = store.consensus_publications().await;
        assert!(consensus.iter().all(|p| p.id != id1));

        // id2 has no reviews yet, so no consensus
        assert!(!consensus.iter().any(|p| p.id == id2));

        // After reviewing id2, it should appear in consensus
        store.review(&id2, "r1", "n2", Grade::Accept, "Better").await.unwrap();
        let consensus = store.consensus_publications().await;
        assert_eq!(consensus.len(), 1);
        assert_eq!(consensus[0].id, id2);
    }

    #[tokio::test]
    async fn test_citation_graph() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "SQL Injection", "C1", vec![]).await;
        let id2 = store.publish("a2", "n2", "XSS Bug", "C2", vec![]).await;
        let id3 = store.publish("a3", "n3", "Summary", "C3", vec![]).await;

        store.cite(&id3, &id1).await.unwrap();
        store.cite(&id3, &id2).await.unwrap();

        let graph = store.citation_graph(None).await;
        assert!(graph.contains(&id1));
        assert!(graph.contains(&id2));
        assert!(graph.contains(&id3));
        assert!(graph.contains("cited by"));

        // Single publication graph
        let graph_single = store.citation_graph(Some(&id1)).await;
        assert!(graph_single.contains(&id1));
        assert!(graph_single.contains("cited by"));
    }

    #[tokio::test]
    async fn test_summarize_consensus() {
        let store = PublicationStore::new(1);

        let id1 = store.publish("a1", "n1", "High Priority", "Content", vec![]).await;
        let id2 = store.publish("a2", "n2", "Low Priority", "Content", vec![]).await;

        store.review(&id1, "r1", "n3", Grade::StrongAccept, "Critical").await.unwrap();
        store.review(&id2, "r1", "n3", Grade::Accept, "OK").await.unwrap();

        // id2 cites id1, boosting id1's weighted score
        store.cite(&id2, &id1).await.unwrap();

        let summary = store.summarize_consensus().await;
        assert!(summary.contains("High Priority"));
        assert!(summary.contains("Low Priority"));
        assert!(summary.contains("Consensus Summary"));
    }

    #[tokio::test]
    async fn test_stats_include_citations() {
        let store = PublicationStore::new(2);

        let id1 = store.publish("a1", "n1", "F1", "C1", vec![]).await;
        let id2 = store.publish("a2", "n2", "F2", "C2", vec![]).await;

        store.cite(&id2, &id1).await.unwrap();

        let stats = store.stats().await;
        assert_eq!(stats.total_publications, 2);
        assert_eq!(stats.total_citations, 1);
    }
}
