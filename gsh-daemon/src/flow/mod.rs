//! Flow module for multi-agent orchestration
//!
//! This module provides data structures and parsing for flow definitions,
//! which describe how multiple agents coordinate to accomplish complex tasks.

mod engine;
mod parser;

pub use engine::{FlowContext, FlowEngine, FlowEvent, NodeResult};
pub use parser::{parse_flow, parse_flow_file, FlowParseError};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A flow defines a directed graph of agent nodes that process tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flow {
    /// Unique name for this flow
    pub name: String,

    /// Human-readable description
    #[serde(default)]
    pub description: String,

    /// Version string (semver recommended)
    #[serde(default = "default_version")]
    pub version: String,

    /// The entry point node ID
    pub entry: String,

    /// All nodes in the flow, keyed by node ID
    pub nodes: HashMap<String, AgentNode>,

    /// Global coordination settings
    #[serde(default)]
    pub coordination: Coordination,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

/// An agent node in the flow graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentNode {
    /// Human-readable name for this node
    pub name: String,

    /// Description of what this agent does
    #[serde(default)]
    pub description: String,

    /// The agent type/role (e.g., "planner", "coder", "reviewer")
    #[serde(default = "default_agent_type")]
    pub agent_type: String,

    /// Custom system prompt for this agent (optional, uses default if not set)
    pub system_prompt: Option<String>,

    /// Tools this agent is allowed to use (empty = all allowed)
    #[serde(default)]
    pub allowed_tools: Vec<String>,

    /// Tools this agent is NOT allowed to use
    #[serde(default)]
    pub denied_tools: Vec<String>,

    /// Where to go after this node completes
    pub next: NextNode,

    /// Maximum iterations for this node's agent loop
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,

    /// Timeout in seconds (0 = no timeout)
    #[serde(default)]
    pub timeout_secs: u64,

    /// Input variables this node expects (for documentation/validation)
    #[serde(default)]
    pub inputs: Vec<String>,

    /// Output variables this node produces (for documentation/validation)
    #[serde(default)]
    pub outputs: Vec<String>,
}

fn default_agent_type() -> String {
    "general".to_string()
}

fn default_max_iterations() -> usize {
    10
}

/// Defines where flow control goes after a node completes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NextNode {
    /// Flow ends after this node
    End,

    /// Unconditionally go to a single node
    Single(String),

    /// Conditionally branch based on output
    Conditional {
        /// Map from condition string to target node ID
        /// Special keys: "default" for fallback, "error" for error handling
        branches: HashMap<String, String>,
    },

    /// Execute multiple nodes in parallel, then join
    Parallel {
        /// Node IDs to execute in parallel
        nodes: Vec<String>,
        /// Node ID to continue to after all parallel nodes complete
        join: String,
    },
}

/// Global coordination settings for the flow
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Coordination {
    /// Maximum total iterations across all nodes
    #[serde(default = "default_max_total_iterations")]
    pub max_total_iterations: usize,

    /// Global timeout in seconds (0 = no timeout)
    #[serde(default)]
    pub timeout_secs: u64,

    /// How to handle errors
    #[serde(default)]
    pub error_handling: ErrorHandling,

    /// Whether to allow concurrent execution of parallel branches
    #[serde(default = "default_true")]
    pub allow_parallel: bool,

    /// Shared context that persists across all nodes
    #[serde(default)]
    pub shared_context: HashMap<String, String>,
}

fn default_max_total_iterations() -> usize {
    100
}

fn default_true() -> bool {
    true
}

/// How to handle errors in the flow
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorHandling {
    /// Node to jump to on error (if any)
    pub on_error_goto: Option<String>,

    /// Whether to retry failed nodes
    #[serde(default)]
    pub retry_count: usize,

    /// Delay between retries in milliseconds
    #[serde(default = "default_retry_delay")]
    pub retry_delay_ms: u64,

    /// Whether to fail the entire flow on unhandled error
    #[serde(default = "default_true_bool")]
    pub fail_on_unhandled: bool,
}

fn default_retry_delay() -> u64 {
    1000
}

fn default_true_bool() -> bool {
    true
}

impl Flow {
    /// Validate the flow structure
    ///
    /// Checks:
    /// - Entry node exists
    /// - All referenced nodes exist
    /// - No cycles in the graph
    /// - All nodes are reachable from entry
    pub fn validate(&self) -> Result<(), FlowValidationError> {
        // Check entry node exists
        if !self.nodes.contains_key(&self.entry) {
            return Err(FlowValidationError::EntryNodeNotFound(self.entry.clone()));
        }

        // Check all node references are valid
        for (node_id, node) in &self.nodes {
            self.validate_next_references(node_id, &node.next)?;
        }

        // Check for cycles
        self.check_no_cycles()?;

        // Check all nodes are reachable
        self.check_all_reachable()?;

        // Check error handling references
        if let Some(ref error_node) = self.coordination.error_handling.on_error_goto {
            if !self.nodes.contains_key(error_node) {
                return Err(FlowValidationError::InvalidNodeReference {
                    from: "coordination.error_handling".to_string(),
                    to: error_node.clone(),
                });
            }
        }

        Ok(())
    }

    fn validate_next_references(&self, from: &str, next: &NextNode) -> Result<(), FlowValidationError> {
        match next {
            NextNode::End => Ok(()),
            NextNode::Single(target) => {
                if !self.nodes.contains_key(target) {
                    return Err(FlowValidationError::InvalidNodeReference {
                        from: from.to_string(),
                        to: target.clone(),
                    });
                }
                Ok(())
            }
            NextNode::Conditional { branches } => {
                for (_, target) in branches {
                    if !self.nodes.contains_key(target) {
                        return Err(FlowValidationError::InvalidNodeReference {
                            from: from.to_string(),
                            to: target.clone(),
                        });
                    }
                }
                Ok(())
            }
            NextNode::Parallel { nodes, join } => {
                for target in nodes {
                    if !self.nodes.contains_key(target) {
                        return Err(FlowValidationError::InvalidNodeReference {
                            from: from.to_string(),
                            to: target.clone(),
                        });
                    }
                }
                if !self.nodes.contains_key(join) {
                    return Err(FlowValidationError::InvalidNodeReference {
                        from: from.to_string(),
                        to: join.clone(),
                    });
                }
                Ok(())
            }
        }
    }

    fn check_no_cycles(&self) -> Result<(), FlowValidationError> {
        use std::collections::HashSet;

        // Use DFS with coloring: White (unvisited), Gray (visiting), Black (visited)
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Gray,
            Black,
        }

        let mut colors: HashMap<String, Color> = self
            .nodes
            .keys()
            .map(|k| (k.clone(), Color::White))
            .collect();

        fn dfs(
            node_id: &str,
            nodes: &HashMap<String, AgentNode>,
            colors: &mut HashMap<String, Color>,
            path: &mut Vec<String>,
        ) -> Result<(), Vec<String>> {
            colors.insert(node_id.to_string(), Color::Gray);
            path.push(node_id.to_string());

            let node = match nodes.get(node_id) {
                Some(n) => n,
                None => {
                    path.pop();
                    colors.insert(node_id.to_string(), Color::Black);
                    return Ok(());
                }
            };

            let targets = get_next_targets(&node.next);

            for target in targets {
                match colors.get(&target) {
                    Some(Color::Gray) => {
                        // Found a cycle - build the cycle path
                        let cycle_start = path.iter().position(|x| x == &target).unwrap();
                        let mut cycle: Vec<String> = path[cycle_start..].to_vec();
                        cycle.push(target);
                        return Err(cycle);
                    }
                    Some(Color::White) | None => {
                        if nodes.contains_key(&target) {
                            dfs(&target, nodes, colors, path)?;
                        }
                    }
                    Some(Color::Black) => {
                        // Already fully processed
                    }
                }
            }

            path.pop();
            colors.insert(node_id.to_string(), Color::Black);
            Ok(())
        }

        let mut path = Vec::new();

        // Start DFS from entry
        if let Err(cycle) = dfs(&self.entry, &self.nodes, &mut colors, &mut path) {
            return Err(FlowValidationError::CycleDetected(cycle));
        }

        // Also check any unreachable nodes for self-cycles
        for node_id in self.nodes.keys() {
            if colors.get(node_id) == Some(&Color::White) {
                path.clear();
                if let Err(cycle) = dfs(node_id, &self.nodes, &mut colors, &mut path) {
                    return Err(FlowValidationError::CycleDetected(cycle));
                }
            }
        }

        Ok(())
    }

    fn check_all_reachable(&self) -> Result<(), FlowValidationError> {
        use std::collections::HashSet;

        let mut reachable: HashSet<String> = HashSet::new();
        let mut queue: Vec<String> = vec![self.entry.clone()];

        while let Some(node_id) = queue.pop() {
            if reachable.contains(&node_id) {
                continue;
            }
            reachable.insert(node_id.clone());

            if let Some(node) = self.nodes.get(&node_id) {
                let targets = get_next_targets(&node.next);
                for target in targets {
                    if !reachable.contains(&target) && self.nodes.contains_key(&target) {
                        queue.push(target);
                    }
                }
            }
        }

        // Check which nodes are unreachable
        let unreachable: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| !reachable.contains(*k))
            .cloned()
            .collect();

        if !unreachable.is_empty() {
            return Err(FlowValidationError::UnreachableNodes(unreachable));
        }

        Ok(())
    }

    /// Get all node IDs
    pub fn node_ids(&self) -> impl Iterator<Item = &str> {
        self.nodes.keys().map(|s| s.as_str())
    }

    /// Get a node by ID
    pub fn get_node(&self, id: &str) -> Option<&AgentNode> {
        self.nodes.get(id)
    }

    /// Get the entry node
    pub fn entry_node(&self) -> Option<&AgentNode> {
        self.nodes.get(&self.entry)
    }
}

/// Helper to extract target node IDs from a NextNode
fn get_next_targets(next: &NextNode) -> Vec<String> {
    match next {
        NextNode::End => vec![],
        NextNode::Single(target) => vec![target.clone()],
        NextNode::Conditional { branches } => branches.values().cloned().collect(),
        NextNode::Parallel { nodes, join } => {
            let mut targets = nodes.clone();
            targets.push(join.clone());
            targets
        }
    }
}

/// Errors that can occur during flow validation
#[derive(Debug, Clone, thiserror::Error)]
pub enum FlowValidationError {
    #[error("Entry node '{0}' not found in flow")]
    EntryNodeNotFound(String),

    #[error("Node '{from}' references non-existent node '{to}'")]
    InvalidNodeReference { from: String, to: String },

    #[error("Cycle detected in flow: {}", .0.join(" -> "))]
    CycleDetected(Vec<String>),

    #[error("Unreachable nodes in flow: {}", .0.join(", "))]
    UnreachableNodes(Vec<String>),
}

impl NextNode {
    /// Check if this is an end node
    pub fn is_end(&self) -> bool {
        matches!(self, NextNode::End)
    }

    /// Check if this is a conditional branch
    pub fn is_conditional(&self) -> bool {
        matches!(self, NextNode::Conditional { .. })
    }

    /// Check if this is a parallel branch
    pub fn is_parallel(&self) -> bool {
        matches!(self, NextNode::Parallel { .. })
    }

    /// Get the target nodes (if any)
    pub fn targets(&self) -> Vec<&str> {
        match self {
            NextNode::End => vec![],
            NextNode::Single(target) => vec![target.as_str()],
            NextNode::Conditional { branches } => {
                branches.values().map(|s| s.as_str()).collect()
            }
            NextNode::Parallel { nodes, join } => {
                let mut targets: Vec<&str> = nodes.iter().map(|s| s.as_str()).collect();
                targets.push(join.as_str());
                targets
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_flow() -> Flow {
        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            AgentNode {
                name: "Start".to_string(),
                description: "Entry point".to_string(),
                agent_type: "planner".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::Single("process".to_string()),
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec!["plan".to_string()],
            },
        );
        nodes.insert(
            "process".to_string(),
            AgentNode {
                name: "Process".to_string(),
                description: "Main processing".to_string(),
                agent_type: "coder".to_string(),
                system_prompt: None,
                allowed_tools: vec!["bash".to_string(), "write".to_string()],
                denied_tools: vec![],
                next: NextNode::End,
                max_iterations: 10,
                timeout_secs: 60,
                inputs: vec!["plan".to_string()],
                outputs: vec![],
            },
        );

        Flow {
            name: "test-flow".to_string(),
            description: "A test flow".to_string(),
            version: "0.1.0".to_string(),
            entry: "start".to_string(),
            nodes,
            coordination: Coordination::default(),
        }
    }

    #[test]
    fn test_valid_flow() {
        let flow = simple_flow();
        assert!(flow.validate().is_ok());
    }

    #[test]
    fn test_missing_entry_node() {
        let mut flow = simple_flow();
        flow.entry = "nonexistent".to_string();
        let result = flow.validate();
        assert!(matches!(result, Err(FlowValidationError::EntryNodeNotFound(_))));
    }

    #[test]
    fn test_invalid_node_reference() {
        let mut flow = simple_flow();
        if let Some(node) = flow.nodes.get_mut("start") {
            node.next = NextNode::Single("nonexistent".to_string());
        }
        let result = flow.validate();
        assert!(matches!(result, Err(FlowValidationError::InvalidNodeReference { .. })));
    }

    #[test]
    fn test_cycle_detection() {
        let mut nodes = HashMap::new();
        nodes.insert(
            "a".to_string(),
            AgentNode {
                name: "A".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::Single("b".to_string()),
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );
        nodes.insert(
            "b".to_string(),
            AgentNode {
                name: "B".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::Single("c".to_string()),
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );
        nodes.insert(
            "c".to_string(),
            AgentNode {
                name: "C".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::Single("a".to_string()), // Cycle back to a
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );

        let flow = Flow {
            name: "cyclic".to_string(),
            description: "".to_string(),
            version: "0.1.0".to_string(),
            entry: "a".to_string(),
            nodes,
            coordination: Coordination::default(),
        };

        let result = flow.validate();
        assert!(matches!(result, Err(FlowValidationError::CycleDetected(_))));
    }

    #[test]
    fn test_unreachable_nodes() {
        let mut flow = simple_flow();
        flow.nodes.insert(
            "orphan".to_string(),
            AgentNode {
                name: "Orphan".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::End,
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );

        let result = flow.validate();
        assert!(matches!(result, Err(FlowValidationError::UnreachableNodes(_))));
    }

    #[test]
    fn test_conditional_next() {
        let mut flow = simple_flow();
        let mut branches = HashMap::new();
        branches.insert("success".to_string(), "process".to_string());
        branches.insert("default".to_string(), "process".to_string());

        if let Some(node) = flow.nodes.get_mut("start") {
            node.next = NextNode::Conditional { branches };
        }

        assert!(flow.validate().is_ok());
    }

    #[test]
    fn test_parallel_next() {
        let mut flow = simple_flow();

        // Add another node for parallel execution
        flow.nodes.insert(
            "parallel_task".to_string(),
            AgentNode {
                name: "Parallel Task".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::End,
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );

        // Add a join node
        flow.nodes.insert(
            "join".to_string(),
            AgentNode {
                name: "Join".to_string(),
                description: "".to_string(),
                agent_type: "general".to_string(),
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                next: NextNode::End,
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
            },
        );

        if let Some(node) = flow.nodes.get_mut("start") {
            node.next = NextNode::Parallel {
                nodes: vec!["process".to_string(), "parallel_task".to_string()],
                join: "join".to_string(),
            };
        }

        // Update process to end (no longer pointing to join, parallel handles that)
        if let Some(node) = flow.nodes.get_mut("process") {
            node.next = NextNode::End;
        }

        assert!(flow.validate().is_ok());
    }
}
