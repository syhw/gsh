//! TOML parser for flow definitions
//!
//! This module handles parsing flow definitions from TOML files.
//! It supports the full flow schema and provides detailed error messages.

use super::{AgentNode, Coordination, ErrorHandling, Flow, FlowValidationError, NextNode};
use std::collections::HashMap;
use std::path::Path;

/// Errors that can occur during flow parsing
#[derive(Debug, thiserror::Error)]
pub enum FlowParseError {
    #[error("Failed to read flow file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    TomlError(#[from] toml::de::Error),

    #[error("Flow validation failed: {0}")]
    ValidationError(#[from] FlowValidationError),

    #[error("Missing required field: {0}")]
    #[allow(dead_code)]
    MissingField(String),

    #[error("Invalid next node specification in node '{node}': {message}")]
    InvalidNextSpec { node: String, message: String },
}

/// Intermediate TOML structure for parsing
#[derive(Debug, serde::Deserialize)]
struct FlowToml {
    /// Flow metadata
    flow: FlowMetadata,

    /// Coordination settings (optional)
    #[serde(default)]
    coordination: CoordinationToml,

    /// Nodes are defined as [nodes.<id>] sections
    #[serde(default)]
    nodes: HashMap<String, NodeToml>,
}

#[derive(Debug, serde::Deserialize)]
struct FlowMetadata {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_version")]
    version: String,
    entry: String,
}

fn default_version() -> String {
    "0.1.0".to_string()
}

#[derive(Debug, serde::Deserialize, Default)]
struct CoordinationToml {
    #[serde(default = "default_max_total_iterations")]
    max_total_iterations: usize,
    #[serde(default)]
    timeout_secs: u64,
    #[serde(default = "default_true")]
    allow_parallel: bool,
    #[serde(default)]
    shared_context: HashMap<String, String>,
    #[serde(default)]
    error_handling: ErrorHandlingToml,
}

fn default_max_total_iterations() -> usize {
    100
}

fn default_true() -> bool {
    true
}

#[derive(Debug, serde::Deserialize, Default)]
struct ErrorHandlingToml {
    on_error_goto: Option<String>,
    #[serde(default)]
    retry_count: usize,
    #[serde(default = "default_retry_delay")]
    retry_delay_ms: u64,
    #[serde(default = "default_true")]
    fail_on_unhandled: bool,
}

fn default_retry_delay() -> u64 {
    1000
}

#[derive(Debug, serde::Deserialize)]
struct NodeToml {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_agent_type")]
    agent_type: String,
    #[serde(default)]
    role: Option<String>,
    system_prompt: Option<String>,
    #[serde(default)]
    allowed_tools: Vec<String>,
    #[serde(default)]
    denied_tools: Vec<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default = "default_max_iterations")]
    max_iterations: usize,
    #[serde(default)]
    timeout_secs: u64,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    outputs: Vec<String>,

    // Next node specification - can be various forms
    #[serde(default)]
    next: Option<NextNodeToml>,
}

fn default_agent_type() -> String {
    "general".to_string()
}

fn default_max_iterations() -> usize {
    10
}

/// Flexible representation of next node that can be parsed from TOML
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum NextNodeToml {
    /// Simple string: "end" or a node ID
    Simple(String),

    /// Structured specification
    Structured(NextNodeStructured),
}

#[derive(Debug, serde::Deserialize)]
struct NextNodeStructured {
    /// For conditional: map of condition -> node
    #[serde(default)]
    branches: HashMap<String, String>,

    /// For parallel: list of nodes to run in parallel
    #[serde(default)]
    parallel: Vec<String>,

    /// For parallel: the join node
    join: Option<String>,
}

/// Parse a flow from a TOML string
pub fn parse_flow(toml_content: &str) -> Result<Flow, FlowParseError> {
    let flow_toml: FlowToml = toml::from_str(toml_content)?;
    convert_flow(flow_toml)
}

/// Parse a flow from a TOML file
pub fn parse_flow_file(path: impl AsRef<Path>) -> Result<Flow, FlowParseError> {
    let content = std::fs::read_to_string(path)?;
    parse_flow(&content)
}

fn convert_flow(flow_toml: FlowToml) -> Result<Flow, FlowParseError> {
    let mut nodes = HashMap::new();

    for (node_id, node_toml) in flow_toml.nodes {
        let next = convert_next_node(&node_id, node_toml.next)?;

        nodes.insert(
            node_id,
            AgentNode {
                name: node_toml.name,
                description: node_toml.description,
                agent_type: node_toml.agent_type,
                role: node_toml.role,
                system_prompt: node_toml.system_prompt,
                allowed_tools: node_toml.allowed_tools,
                denied_tools: node_toml.denied_tools,
                provider: node_toml.provider,
                model: node_toml.model,
                next,
                max_iterations: node_toml.max_iterations,
                timeout_secs: node_toml.timeout_secs,
                inputs: node_toml.inputs,
                outputs: node_toml.outputs,
            },
        );
    }

    let coordination = Coordination {
        max_total_iterations: flow_toml.coordination.max_total_iterations,
        timeout_secs: flow_toml.coordination.timeout_secs,
        allow_parallel: flow_toml.coordination.allow_parallel,
        shared_context: flow_toml.coordination.shared_context,
        error_handling: ErrorHandling {
            on_error_goto: flow_toml.coordination.error_handling.on_error_goto,
            retry_count: flow_toml.coordination.error_handling.retry_count,
            retry_delay_ms: flow_toml.coordination.error_handling.retry_delay_ms,
            fail_on_unhandled: flow_toml.coordination.error_handling.fail_on_unhandled,
        },
    };

    let flow = Flow {
        name: flow_toml.flow.name,
        description: flow_toml.flow.description,
        version: flow_toml.flow.version,
        entry: flow_toml.flow.entry,
        nodes,
        coordination,
    };

    // Validate the flow
    flow.validate()?;

    Ok(flow)
}

fn convert_next_node(node_id: &str, next_toml: Option<NextNodeToml>) -> Result<NextNode, FlowParseError> {
    match next_toml {
        None => Ok(NextNode::End),
        Some(NextNodeToml::Simple(s)) => {
            if s.eq_ignore_ascii_case("end") {
                Ok(NextNode::End)
            } else {
                Ok(NextNode::Single(s))
            }
        }
        Some(NextNodeToml::Structured(structured)) => {
            // Check if it's a parallel spec
            if !structured.parallel.is_empty() {
                let join = structured.join.ok_or_else(|| FlowParseError::InvalidNextSpec {
                    node: node_id.to_string(),
                    message: "Parallel specification requires a 'join' node".to_string(),
                })?;
                Ok(NextNode::Parallel {
                    nodes: structured.parallel,
                    join,
                })
            }
            // Check if it's a conditional spec
            else if !structured.branches.is_empty() {
                Ok(NextNode::Conditional {
                    branches: structured.branches,
                })
            }
            // Empty structured = end
            else {
                Ok(NextNode::End)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_flow() {
        let toml = r#"
[flow]
name = "simple-flow"
description = "A simple test flow"
version = "1.0.0"
entry = "start"

[nodes.start]
name = "Start Node"
description = "Entry point"
agent_type = "planner"
next = "process"

[nodes.process]
name = "Process Node"
description = "Main processing"
agent_type = "coder"
allowed_tools = ["bash", "write"]
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        assert_eq!(flow.name, "simple-flow");
        assert_eq!(flow.version, "1.0.0");
        assert_eq!(flow.entry, "start");
        assert_eq!(flow.nodes.len(), 2);

        let start = flow.get_node("start").unwrap();
        assert_eq!(start.name, "Start Node");
        assert_eq!(start.agent_type, "planner");
        assert!(matches!(start.next, NextNode::Single(ref s) if s == "process"));

        let process = flow.get_node("process").unwrap();
        assert_eq!(process.allowed_tools, vec!["bash", "write"]);
        assert!(matches!(process.next, NextNode::End));
    }

    #[test]
    fn test_parse_conditional_flow() {
        let toml = r#"
[flow]
name = "conditional-flow"
entry = "start"

[nodes.start]
name = "Start"
agent_type = "planner"
[nodes.start.next]
branches = { success = "process", error = "error_handler", default = "fallback" }

[nodes.process]
name = "Process"
next = "end"

[nodes.error_handler]
name = "Error Handler"
next = "end"

[nodes.fallback]
name = "Fallback"
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        let start = flow.get_node("start").unwrap();

        if let NextNode::Conditional { branches } = &start.next {
            assert_eq!(branches.get("success"), Some(&"process".to_string()));
            assert_eq!(branches.get("error"), Some(&"error_handler".to_string()));
            assert_eq!(branches.get("default"), Some(&"fallback".to_string()));
        } else {
            panic!("Expected conditional next");
        }
    }

    #[test]
    fn test_parse_parallel_flow() {
        let toml = r#"
[flow]
name = "parallel-flow"
entry = "start"

[nodes.start]
name = "Start"
[nodes.start.next]
parallel = ["worker1", "worker2"]
join = "join"

[nodes.worker1]
name = "Worker 1"
next = "end"

[nodes.worker2]
name = "Worker 2"
next = "end"

[nodes.join]
name = "Join"
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        let start = flow.get_node("start").unwrap();

        if let NextNode::Parallel { nodes, join } = &start.next {
            assert!(nodes.contains(&"worker1".to_string()));
            assert!(nodes.contains(&"worker2".to_string()));
            assert_eq!(join, "join");
        } else {
            panic!("Expected parallel next");
        }
    }

    #[test]
    fn test_parse_with_coordination() {
        let toml = r#"
[flow]
name = "coordinated-flow"
entry = "start"

[coordination]
max_total_iterations = 50
timeout_secs = 300
allow_parallel = true

[coordination.error_handling]
on_error_goto = "error_handler"
retry_count = 3
retry_delay_ms = 2000

[coordination.shared_context]
project_name = "test"
environment = "dev"

[nodes.start]
name = "Start"
next = "error_handler"

[nodes.error_handler]
name = "Error Handler"
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        assert_eq!(flow.coordination.max_total_iterations, 50);
        assert_eq!(flow.coordination.timeout_secs, 300);
        assert!(flow.coordination.allow_parallel);
        assert_eq!(
            flow.coordination.error_handling.on_error_goto,
            Some("error_handler".to_string())
        );
        assert_eq!(flow.coordination.error_handling.retry_count, 3);
        assert_eq!(flow.coordination.error_handling.retry_delay_ms, 2000);
        assert_eq!(
            flow.coordination.shared_context.get("project_name"),
            Some(&"test".to_string())
        );
    }

    #[test]
    fn test_parse_with_system_prompt() {
        let toml = r#"
[flow]
name = "custom-prompt-flow"
entry = "start"

[nodes.start]
name = "Start"
system_prompt = """
You are a specialized coding agent.
Focus on writing clean, maintainable code.
"""
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        let start = flow.get_node("start").unwrap();
        assert!(start.system_prompt.is_some());
        assert!(start.system_prompt.as_ref().unwrap().contains("specialized coding agent"));
    }

    #[test]
    fn test_parse_with_tool_restrictions() {
        let toml = r#"
[flow]
name = "restricted-flow"
entry = "start"

[nodes.start]
name = "Start"
allowed_tools = ["read", "grep", "glob"]
denied_tools = []
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        let start = flow.get_node("start").unwrap();
        assert_eq!(start.allowed_tools, vec!["read", "grep", "glob"]);
    }

    #[test]
    fn test_parse_invalid_toml() {
        let toml = r#"
[flow
name = "broken"
"#;

        let result = parse_flow(toml);
        assert!(matches!(result, Err(FlowParseError::TomlError(_))));
    }

    #[test]
    fn test_parse_missing_entry() {
        let toml = r#"
[flow]
name = "no-entry"
entry = "nonexistent"

[nodes.start]
name = "Start"
next = "end"
"#;

        let result = parse_flow(toml);
        assert!(matches!(result, Err(FlowParseError::ValidationError(_))));
    }

    #[test]
    fn test_parse_cycle_detection() {
        let toml = r#"
[flow]
name = "cyclic"
entry = "a"

[nodes.a]
name = "A"
next = "b"

[nodes.b]
name = "B"
next = "a"
"#;

        let result = parse_flow(toml);
        assert!(matches!(result, Err(FlowParseError::ValidationError(
            FlowValidationError::CycleDetected(_)
        ))));
    }

    #[test]
    fn test_parse_inputs_outputs() {
        let toml = r#"
[flow]
name = "io-flow"
entry = "start"

[nodes.start]
name = "Start"
inputs = ["user_query"]
outputs = ["plan", "requirements"]
next = "end"
"#;

        let flow = parse_flow(toml).unwrap();
        let start = flow.get_node("start").unwrap();
        assert_eq!(start.inputs, vec!["user_query"]);
        assert_eq!(start.outputs, vec!["plan", "requirements"]);
    }

    #[test]
    fn test_default_values() {
        let toml = r#"
[flow]
name = "minimal"
entry = "start"

[nodes.start]
name = "Start"
"#;

        let flow = parse_flow(toml).unwrap();
        assert_eq!(flow.version, "0.1.0");
        assert!(flow.description.is_empty());

        let start = flow.get_node("start").unwrap();
        assert_eq!(start.agent_type, "general");
        assert_eq!(start.max_iterations, 10);
        assert_eq!(start.timeout_secs, 0);
        assert!(start.allowed_tools.is_empty());
        assert!(start.denied_tools.is_empty());
        assert!(matches!(start.next, NextNode::End));
    }
}
