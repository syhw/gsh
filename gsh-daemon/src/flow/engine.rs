//! Flow execution engine
//!
//! Executes multi-agent flows by orchestrating agents across tmux sessions,
//! handling conditional routing, parallel execution, and context passing.

use super::{CoordinationMode, Flow, MemoryStore, NextNode, PublicationStore, PublicationToolHandler};
use crate::agent::{Agent, AgentEvent, CustomToolHandler};
use crate::config::Config;
use crate::provider::{self};
use crate::tmux::{AgentSessionConfig, TmuxManager};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

/// Events emitted during flow execution
#[derive(Debug, Clone, Serialize)]
pub enum FlowEvent {
    /// Flow execution started
    FlowStarted {
        flow_name: String,
        entry_node: String,
    },
    /// A node started execution
    NodeStarted {
        node_id: String,
        node_name: String,
        agent_type: String,
        tmux_session: Option<String>,
    },
    /// Text output from a node
    NodeText {
        node_id: String,
        text: String,
    },
    /// Tool usage in a node
    NodeToolUse {
        node_id: String,
        tool: String,
        input: serde_json::Value,
    },
    /// Tool result in a node
    NodeToolResult {
        node_id: String,
        tool: String,
        output: String,
        success: bool,
    },
    /// A node completed execution
    NodeCompleted {
        node_id: String,
        output: String,
        next_node: Option<String>,
    },
    /// Parallel execution started
    ParallelStarted {
        node_ids: Vec<String>,
    },
    /// Parallel execution completed
    ParallelCompleted {
        results: HashMap<String, String>,
        join_node: String,
    },
    /// Flow completed successfully
    FlowCompleted {
        final_output: String,
    },
    /// Flow failed with error
    FlowError {
        node_id: Option<String>,
        error: String,
    },
    // --- Publication mode events ---
    /// A publication was submitted
    PublicationCreated {
        node_id: String,
        publication_id: String,
        title: String,
        author: String,
    },
    /// A publication was reviewed
    PublicationReviewed {
        publication_id: String,
        reviewer: String,
        grade: String,
        consensus_score: i32,
    },
    /// A publication was cited
    PublicationCited {
        publication_id: String,
        cited_id: String,
        node_id: String,
    },
    /// A publication reached consensus
    PublicationConsensus {
        publication_id: String,
        title: String,
        accept_count: usize,
    },
}

/// Result of a single node execution
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct NodeResult {
    pub node_id: String,
    pub output: String,
    pub success: bool,
}

/// Context passed between nodes in a flow
#[derive(Debug, Clone, Default)]
pub struct FlowContext {
    /// Original user input
    pub input: String,
    /// Outputs from each completed node
    pub node_outputs: HashMap<String, String>,
    /// Shared variables
    pub variables: HashMap<String, String>,
    /// Current working directory
    pub cwd: String,
}

impl FlowContext {
    pub fn new(input: String, cwd: String) -> Self {
        Self {
            input,
            cwd,
            node_outputs: HashMap::new(),
            variables: HashMap::new(),
        }
    }

    /// Build context string for agent prompt
    pub fn build_prompt(&self, node_id: &str) -> String {
        let mut prompt = format!("User request: {}\n", self.input);

        if !self.node_outputs.is_empty() {
            prompt.push_str("\n--- Previous agent outputs ---\n");
            for (id, output) in &self.node_outputs {
                if id != node_id {
                    prompt.push_str(&format!("\n[{}]:\n{}\n", id, output));
                }
            }
        }

        if !self.variables.is_empty() {
            prompt.push_str("\n--- Shared context ---\n");
            for (key, value) in &self.variables {
                prompt.push_str(&format!("{}: {}\n", key, value));
            }
        }

        prompt
    }
}

/// The flow execution engine
pub struct FlowEngine {
    config: Config,
    tmux_manager: TmuxManager,
    /// Provider overrides per node (node_id -> provider_name)
    provider_overrides: HashMap<String, String>,
    /// Model overrides per node (node_id -> model_name)
    model_overrides: HashMap<String, String>,
    /// Publication store for publication coordination mode
    publication_store: Option<Arc<PublicationStore>>,
    /// Memory store for agent memory persistence across instances
    memory_store: Option<Arc<MemoryStore>>,
    /// MCP tool handler (shared across all agents in this flow)
    mcp_handler: Option<Arc<dyn CustomToolHandler>>,
}

impl FlowEngine {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            tmux_manager: TmuxManager::new(),
            provider_overrides: HashMap::new(),
            model_overrides: HashMap::new(),
            publication_store: None,
            memory_store: None,
            mcp_handler: None,
        }
    }

    /// Create a flow engine with publication mode support
    pub fn with_publication_mode(config: Config, consensus_threshold: usize, allow_self_review: bool) -> Self {
        Self {
            config,
            tmux_manager: TmuxManager::new(),
            provider_overrides: HashMap::new(),
            model_overrides: HashMap::new(),
            publication_store: Some(Arc::new(
                PublicationStore::new(consensus_threshold).with_self_review(allow_self_review),
            )),
            memory_store: Some(Arc::new(MemoryStore::new())),
            mcp_handler: None,
        }
    }

    /// Set an MCP tool handler for all agents in this flow
    pub fn with_mcp_handler(mut self, handler: Arc<dyn CustomToolHandler>) -> Self {
        self.mcp_handler = Some(handler);
        self
    }

    /// Get the publication store (if in publication mode)
    pub fn publication_store(&self) -> Option<Arc<PublicationStore>> {
        self.publication_store.clone()
    }

    /// Set provider override for a specific node
    #[allow(dead_code)]
    pub fn set_node_provider(&mut self, node_id: &str, provider: &str) {
        self.provider_overrides.insert(node_id.to_string(), provider.to_string());
    }

    /// Set model override for a specific node
    #[allow(dead_code)]
    pub fn set_node_model(&mut self, node_id: &str, model: &str) {
        self.model_overrides.insert(node_id.to_string(), model.to_string());
    }

    /// Resolve role for a node
    /// Checks in order: node.role, node.agent_type as role, built-in roles
    fn resolve_role(&self, node: &super::AgentNode) -> Result<Option<super::roles::Role>> {
        use super::roles::{Role, RoleRegistry};

        // First check explicit role reference
        if let Some(ref role_name) = node.role {
            // Try built-in role first
            if let Some(builtin) = Role::builtin(role_name) {
                return Ok(Some(builtin));
            }

            // Try loading from file
            let mut registry = RoleRegistry::new();
            match registry.get(role_name) {
                Ok(role) => return Ok(Some(role.clone())),
                Err(e) => {
                    warn!("Failed to load role '{}': {}", role_name, e);
                    // Fall through to agent_type check
                }
            }
        }

        // Check if agent_type matches a built-in role
        if let Some(builtin) = Role::builtin(&node.agent_type) {
            return Ok(Some(builtin));
        }

        // Try loading agent_type as a role file
        let mut registry = RoleRegistry::new();
        if let Ok(role) = registry.get(&node.agent_type) {
            return Ok(Some(role.clone()));
        }

        // No role found, that's okay - use node settings directly
        Ok(None)
    }

    /// Execute a flow
    pub async fn run(
        &mut self,
        flow: &Flow,
        input: &str,
        cwd: &str,
        event_tx: mpsc::Sender<FlowEvent>,
    ) -> Result<String> {
        // Validate flow first
        flow.validate()?;

        // Initialize publication store if flow uses publication mode
        if flow.coordination.mode == CoordinationMode::Publication && self.publication_store.is_none() {
            self.publication_store = Some(Arc::new(
                PublicationStore::new(flow.coordination.consensus_threshold)
                    .with_self_review(flow.coordination.allow_self_review),
            ));
            self.memory_store = Some(Arc::new(MemoryStore::new()));
            info!(
                "Initialized publication mode with consensus threshold: {}, allow_self_review: {}",
                flow.coordination.consensus_threshold,
                flow.coordination.allow_self_review,
            );
        }

        let ctx = Arc::new(RwLock::new(FlowContext::new(input.to_string(), cwd.to_string())));

        let _ = event_tx.send(FlowEvent::FlowStarted {
            flow_name: flow.name.clone(),
            entry_node: flow.entry.clone(),
        }).await;

        // Start execution from entry node
        let final_output = self
            .execute_node(flow, &flow.entry, ctx.clone(), &event_tx)
            .await?;

        // In publication mode, append consensus summary to output
        let final_output = if let Some(ref store) = self.publication_store {
            let stats = store.stats().await;
            let consensus_pubs = store.consensus_publications().await;

            if !consensus_pubs.is_empty() {
                let summary = store.format_for_context(&consensus_pubs).await;
                format!(
                    "{}\n\n--- Consensus Publications ({}/{} total) ---\n{}",
                    final_output,
                    stats.publications_with_consensus,
                    stats.total_publications,
                    summary
                )
            } else {
                final_output
            }
        } else {
            final_output
        };

        let _ = event_tx.send(FlowEvent::FlowCompleted {
            final_output: final_output.clone(),
        }).await;

        Ok(final_output)
    }

    /// Execute a single node and follow its next transitions
    fn execute_node<'a>(
        &'a self,
        flow: &'a Flow,
        node_id: &'a str,
        ctx: Arc<RwLock<FlowContext>>,
        event_tx: &'a mpsc::Sender<FlowEvent>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(self.execute_node_inner(flow, node_id, ctx, event_tx))
    }

    /// Inner implementation of execute_node
    async fn execute_node_inner(
        &self,
        flow: &Flow,
        node_id: &str,
        ctx: Arc<RwLock<FlowContext>>,
        event_tx: &mpsc::Sender<FlowEvent>,
    ) -> Result<String> {
        let node = flow.get_node(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;

        // If count > 1, spawn multiple instances of this node
        if node.count > 1 {
            return self.execute_node_instances(flow, node_id, ctx, event_tx).await;
        }

        info!("Executing node: {} ({})", node_id, node.name);

        // Resolve role for this node
        let role = self.resolve_role(node)?;

        // Spawn tmux session for this agent (if tmux is available)
        let tmux_session = if TmuxManager::is_available() {
            let session_config = AgentSessionConfig {
                name: Some(format!("{}-{}", flow.name, node_id)),
                cwd: Some(ctx.read().await.cwd.clone().into()),
                task: Some(node.description.clone()),
                background: true,
                ..Default::default()
            };
            match self.tmux_manager.spawn_agent_session(session_config) {
                Ok(handle) => Some(handle.session_name),
                Err(e) => {
                    warn!("Failed to spawn tmux session for node {}: {}", node_id, e);
                    None
                }
            }
        } else {
            None
        };

        let role_name = role.as_ref().map(|r| r.name.clone()).unwrap_or_else(|| node.agent_type.clone());

        let _ = event_tx.send(FlowEvent::NodeStarted {
            node_id: node_id.to_string(),
            node_name: node.name.clone(),
            agent_type: role_name,
            tmux_session: tmux_session.clone(),
        }).await;

        // Build the prompt with context and role system prompt
        let base_prompt = ctx.read().await.build_prompt(node_id);
        let prompt = if let Some(ref role) = role {
            if let Some(ref system) = role.system_prompt {
                format!("{}\n\n---\n\n{}", system, base_prompt)
            } else {
                base_prompt
            }
        } else {
            base_prompt
        };

        // In publication mode, inject publications from review_from nodes into the prompt
        let prompt = if let Some(ref store) = self.publication_store {
            if !node.review_from.is_empty() {
                let mut review_pubs = Vec::new();
                for from_node in &node.review_from {
                    review_pubs.extend(store.by_node(from_node).await);
                }
                if !review_pubs.is_empty() {
                    let pub_context = store.format_for_context(&review_pubs).await;
                    format!(
                        "{}\n\n--- Publications to review (from: {}) ---\n{}",
                        prompt,
                        node.review_from.join(", "),
                        pub_context,
                    )
                } else {
                    prompt
                }
            } else {
                prompt
            }
        } else {
            prompt
        };

        // Inject agent memories if available
        let prompt = if let Some(ref memory_store) = self.memory_store {
            if let Some(memory_context) = memory_store.format_for_prompt(node_id).await {
                format!("{}\n\n{}", prompt, memory_context)
            } else {
                prompt
            }
        } else {
            prompt
        };

        // Determine provider: node override > role > engine override > config default
        let provider_name = node.provider.clone()
            .or_else(|| role.as_ref().and_then(|r| r.provider.clone()))
            .or_else(|| self.provider_overrides.get(node_id).cloned())
            .unwrap_or_else(|| self.config.llm.default_provider.clone());

        // Determine model: node override > role > engine override
        let model_override = node.model.clone()
            .or_else(|| role.as_ref().and_then(|r| r.model.clone()))
            .or_else(|| self.model_overrides.get(node_id).cloned());

        let provider = if let Some(model) = model_override {
            provider::create_provider_with_model(&provider_name, &model, &self.config)?
        } else {
            provider::create_provider(&provider_name, &self.config)?
        };

        // Create agent with node-specific configuration
        let mut agent = Agent::new(
            provider,
            &self.config,
            ctx.read().await.cwd.clone(),
        ).with_flow_name(&flow.name);

        // Add publication tools if in publication mode
        if let Some(ref store) = self.publication_store {
            // Generate unique agent ID for this node instance
            let agent_id = format!("{}-{}", node_id, uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
            let mut handler = PublicationToolHandler::new(
                store.clone(),
                agent_id,
                node_id.to_string(),
            );
            if let Some(ref memory_store) = self.memory_store {
                handler = handler.with_memory(memory_store.clone());
            }
            agent = agent.with_custom_handler(Arc::new(handler));
        }

        // Add MCP tools if available
        if let Some(ref mcp) = self.mcp_handler {
            agent = agent.with_custom_handler(mcp.clone());
        }

        // Capture publication snapshot before agent runs (for diffing)
        let pub_snapshot = self.capture_pub_snapshot().await;

        // Create event channel for agent
        let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(100);

        // Run agent
        let node_id_clone = node_id.to_string();
        let event_tx_clone = event_tx.clone();

        let agent_handle = tokio::spawn(async move {
            agent.run_oneshot(&prompt, None, agent_event_tx).await
        });

        // Forward agent events to flow events
        let mut output = String::new();
        while let Some(event) = agent_event_rx.recv().await {
            match event {
                AgentEvent::TextChunk(text) => {
                    output.push_str(&text);
                    let _ = event_tx_clone.send(FlowEvent::NodeText {
                        node_id: node_id_clone.clone(),
                        text,
                    }).await;
                }
                AgentEvent::ToolStart { name, input } => {
                    let _ = event_tx_clone.send(FlowEvent::NodeToolUse {
                        node_id: node_id_clone.clone(),
                        tool: name,
                        input,
                    }).await;
                }
                AgentEvent::ToolResult { name, output, success } => {
                    let _ = event_tx_clone.send(FlowEvent::NodeToolResult {
                        node_id: node_id_clone.clone(),
                        tool: name,
                        output,
                        success,
                    }).await;
                }
                AgentEvent::Done { final_text } => {
                    output = final_text;
                }
                AgentEvent::Error(e) => {
                    let _ = event_tx_clone.send(FlowEvent::FlowError {
                        node_id: Some(node_id_clone.clone()),
                        error: e.clone(),
                    }).await;
                    return Err(anyhow::anyhow!("Agent error: {}", e));
                }
                AgentEvent::Compacted { .. } | AgentEvent::Thinking => {}
            }
        }

        // Wait for agent to complete
        agent_handle.await??;

        // Store output in context
        {
            let mut ctx_write = ctx.write().await;
            ctx_write.node_outputs.insert(node_id.to_string(), output.clone());
        }

        // Emit publication events (diff pre/post snapshots)
        if let Some(ref pre) = pub_snapshot {
            self.emit_pub_events(node_id, pre, event_tx).await;
        }

        // Determine next node
        let next_node_id = self.determine_next(&node.next, &output, &ctx).await?;

        let _ = event_tx.send(FlowEvent::NodeCompleted {
            node_id: node_id.to_string(),
            output: output.clone(),
            next_node: next_node_id.clone(),
        }).await;

        // Clean up tmux session if we created one
        if let Some(session) = tmux_session {
            if let Err(e) = self.tmux_manager.kill_session(&session) {
                warn!("Failed to clean up tmux session {}: {}", session, e);
            }
        }

        // Execute next node(s)
        match &node.next {
            NextNode::End => Ok(output),
            NextNode::Single(next_id) => {
                self.execute_node(flow, next_id, ctx, event_tx).await
            }
            NextNode::Conditional { branches: _ } => {
                if let Some(next_id) = next_node_id {
                    self.execute_node(flow, &next_id, ctx, event_tx).await
                } else {
                    // No matching branch - return current output
                    Ok(output)
                }
            }
            NextNode::Parallel { nodes, join } => {
                self.execute_parallel(flow, nodes, join, ctx, event_tx).await
            }
        }
    }

    /// Execute multiple instances of the same node (when count > 1)
    async fn execute_node_instances(
        &self,
        flow: &Flow,
        node_id: &str,
        ctx: Arc<RwLock<FlowContext>>,
        event_tx: &mpsc::Sender<FlowEvent>,
    ) -> Result<String> {
        let node = flow.get_node(node_id)
            .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id))?;

        let count = node.count;
        let run_parallel = node.parallel;

        info!(
            "Executing node '{}' ({}) with {} instance(s) ({})",
            node_id, node.name, count,
            if run_parallel { "parallel" } else { "sequential" }
        );

        // Resolve shared settings for all instances
        let role = self.resolve_role(node)?;
        let provider_name = node.provider.clone()
            .or_else(|| role.as_ref().and_then(|r| r.provider.clone()))
            .or_else(|| self.provider_overrides.get(node_id).cloned())
            .unwrap_or_else(|| self.config.llm.default_provider.clone());
        let model_override = node.model.clone()
            .or_else(|| role.as_ref().and_then(|r| r.model.clone()))
            .or_else(|| self.model_overrides.get(node_id).cloned());

        // Build the base prompt
        let base_prompt = ctx.read().await.build_prompt(node_id);
        let prompt = if let Some(ref role) = role {
            if let Some(ref system) = role.system_prompt {
                format!("{}\n\n---\n\n{}", system, base_prompt)
            } else if let Some(ref sp) = node.system_prompt {
                format!("{}\n\n---\n\n{}", sp, base_prompt)
            } else {
                base_prompt
            }
        } else if let Some(ref sp) = node.system_prompt {
            format!("{}\n\n---\n\n{}", sp, base_prompt)
        } else {
            base_prompt
        };

        // In publication mode, inject publications from review_from nodes
        let prompt = if let Some(ref store) = self.publication_store {
            if !node.review_from.is_empty() {
                let mut review_pubs = Vec::new();
                for from_node in &node.review_from {
                    review_pubs.extend(store.by_node(from_node).await);
                }
                if !review_pubs.is_empty() {
                    let pub_context = store.format_for_context(&review_pubs).await;
                    format!(
                        "{}\n\n--- Publications to review (from: {}) ---\n{}",
                        prompt,
                        node.review_from.join(", "),
                        pub_context,
                    )
                } else {
                    prompt
                }
            } else {
                prompt
            }
        } else {
            prompt
        };

        // Inject agent memories if available
        let prompt = if let Some(ref memory_store) = self.memory_store {
            if let Some(memory_context) = memory_store.format_for_prompt(node_id).await {
                format!("{}\n\n{}", prompt, memory_context)
            } else {
                prompt
            }
        } else {
            prompt
        };

        // Capture publication snapshot before instances run
        let pub_snapshot = self.capture_pub_snapshot().await;

        let cwd = ctx.read().await.cwd.clone();

        if run_parallel {
            // Spawn all instances concurrently
            let mut handles = Vec::new();

            for i in 0..count {
                let instance_id = format!("{}-{}", node_id, i + 1);
                let config_clone = self.config.clone();
                let provider_name_clone = provider_name.clone();
                let model_override_clone = model_override.clone();
                let prompt_clone = prompt.clone();
                let cwd_clone = cwd.clone();
                let event_tx_clone = event_tx.clone();
                let publication_store = self.publication_store.clone();
                let memory_store = self.memory_store.clone();
                let mcp_handler = self.mcp_handler.clone();
                let node_id_str = node_id.to_string();
                let node_name = node.name.clone();
                let flow_name_clone = flow.name.clone();

                let handle = tokio::spawn(async move {
                    let _ = event_tx_clone.send(FlowEvent::NodeStarted {
                        node_id: instance_id.clone(),
                        node_name: format!("{} #{}", node_name, i + 1),
                        agent_type: "instance".to_string(),
                        tmux_session: None,
                    }).await;

                    let provider = if let Some(ref model) = model_override_clone {
                        provider::create_provider_with_model(&provider_name_clone, model, &config_clone)?
                    } else {
                        provider::create_provider(&provider_name_clone, &config_clone)?
                    };

                    let mut agent = Agent::new(provider, &config_clone, cwd_clone)
                        .with_flow_name(&flow_name_clone);

                    // Add publication tools if in publication mode
                    if let Some(ref store) = publication_store {
                        let agent_id = format!("{}-{}", instance_id,
                            uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                        let mut handler = PublicationToolHandler::new(
                            store.clone(),
                            agent_id,
                            node_id_str.clone(),
                        );
                        if let Some(ref ms) = memory_store {
                            handler = handler.with_memory(ms.clone());
                        }
                        agent = agent.with_custom_handler(Arc::new(handler));
                    }

                    // Add MCP tools if available
                    if let Some(ref mcp) = mcp_handler {
                        agent = agent.with_custom_handler(mcp.clone());
                    }

                    let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(100);

                    let agent_handle = tokio::spawn(async move {
                        agent.run_oneshot(&prompt_clone, None, agent_event_tx).await
                    });

                    let mut output = String::new();
                    while let Some(event) = agent_event_rx.recv().await {
                        match event {
                            AgentEvent::TextChunk(text) => {
                                output.push_str(&text);
                                let _ = event_tx_clone.send(FlowEvent::NodeText {
                                    node_id: instance_id.clone(),
                                    text,
                                }).await;
                            }
                            AgentEvent::ToolStart { name, input } => {
                                let _ = event_tx_clone.send(FlowEvent::NodeToolUse {
                                    node_id: instance_id.clone(),
                                    tool: name,
                                    input,
                                }).await;
                            }
                            AgentEvent::ToolResult { name, output, success } => {
                                let _ = event_tx_clone.send(FlowEvent::NodeToolResult {
                                    node_id: instance_id.clone(),
                                    tool: name,
                                    output,
                                    success,
                                }).await;
                            }
                            AgentEvent::Done { final_text } => {
                                output = final_text;
                            }
                            AgentEvent::Error(e) => {
                                let _ = event_tx_clone.send(FlowEvent::FlowError {
                                    node_id: Some(instance_id.clone()),
                                    error: e.clone(),
                                }).await;
                                return Err(anyhow::anyhow!("Agent error in {}: {}", instance_id, e));
                            }
                            AgentEvent::Compacted { .. } | AgentEvent::Thinking => {}
                        }
                    }

                    agent_handle.await??;
                    Ok::<(String, String), anyhow::Error>((instance_id, output))
                });

                handles.push(handle);
            }

            // Collect results from all parallel instances
            let mut combined_output = String::new();
            for handle in handles {
                let (instance_id, output) = handle.await??;
                if !combined_output.is_empty() {
                    combined_output.push_str("\n\n---\n\n");
                }
                combined_output.push_str(&format!("[{}]:\n{}", instance_id, output));
            }

            // Store combined output in context
            {
                let mut ctx_write = ctx.write().await;
                ctx_write.node_outputs.insert(node_id.to_string(), combined_output.clone());
            }

            // Emit publication events
            if let Some(ref pre) = pub_snapshot {
                self.emit_pub_events(node_id, pre, event_tx).await;
            }

            // Determine and execute next node
            let next_node_id = self.determine_next(&node.next, &combined_output, &ctx).await?;
            let _ = event_tx.send(FlowEvent::NodeCompleted {
                node_id: node_id.to_string(),
                output: combined_output.clone(),
                next_node: next_node_id.clone(),
            }).await;

            match &node.next {
                NextNode::End => Ok(combined_output),
                NextNode::Single(next_id) => {
                    self.execute_node(flow, next_id, ctx, event_tx).await
                }
                NextNode::Conditional { .. } => {
                    if let Some(next_id) = next_node_id {
                        self.execute_node(flow, &next_id, ctx, event_tx).await
                    } else {
                        Ok(combined_output)
                    }
                }
                NextNode::Parallel { nodes, join } => {
                    self.execute_parallel(flow, nodes, join, ctx, event_tx).await
                }
            }
        } else {
            // Sequential execution of instances
            let mut combined_output = String::new();

            for i in 0..count {
                let instance_id = format!("{}-{}", node_id, i + 1);

                let _ = event_tx.send(FlowEvent::NodeStarted {
                    node_id: instance_id.clone(),
                    node_name: format!("{} #{}", node.name, i + 1),
                    agent_type: "instance".to_string(),
                    tmux_session: None,
                }).await;

                let provider = if let Some(ref model) = model_override {
                    provider::create_provider_with_model(&provider_name, model, &self.config)?
                } else {
                    provider::create_provider(&provider_name, &self.config)?
                };

                let mut agent = Agent::new(provider, &self.config, cwd.clone())
                    .with_flow_name(&flow.name);

                if let Some(ref store) = self.publication_store {
                    let agent_id = format!("{}-{}", instance_id,
                        uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                    let mut handler = PublicationToolHandler::new(
                        store.clone(),
                        agent_id,
                        node_id.to_string(),
                    );
                    if let Some(ref ms) = self.memory_store {
                        handler = handler.with_memory(ms.clone());
                    }
                    agent = agent.with_custom_handler(Arc::new(handler));
                }

                // Add MCP tools if available
                if let Some(ref mcp) = self.mcp_handler {
                    agent = agent.with_custom_handler(mcp.clone());
                }

                let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(100);
                let event_tx_clone = event_tx.clone();
                let instance_id_clone = instance_id.clone();
                let prompt_clone = prompt.clone();

                let agent_handle = tokio::spawn(async move {
                    agent.run_oneshot(&prompt_clone, None, agent_event_tx).await
                });

                let mut output = String::new();
                while let Some(event) = agent_event_rx.recv().await {
                    match event {
                        AgentEvent::TextChunk(text) => {
                            output.push_str(&text);
                            let _ = event_tx_clone.send(FlowEvent::NodeText {
                                node_id: instance_id_clone.clone(),
                                text,
                            }).await;
                        }
                        AgentEvent::ToolStart { name, input } => {
                            let _ = event_tx_clone.send(FlowEvent::NodeToolUse {
                                node_id: instance_id_clone.clone(),
                                tool: name,
                                input,
                            }).await;
                        }
                        AgentEvent::ToolResult { name, output, success } => {
                            let _ = event_tx_clone.send(FlowEvent::NodeToolResult {
                                node_id: instance_id_clone.clone(),
                                tool: name,
                                output,
                                success,
                            }).await;
                        }
                        AgentEvent::Done { final_text } => {
                            output = final_text;
                        }
                        AgentEvent::Error(e) => {
                            let _ = event_tx_clone.send(FlowEvent::FlowError {
                                node_id: Some(instance_id_clone.clone()),
                                error: e.clone(),
                            }).await;
                            return Err(anyhow::anyhow!("Agent error in {}: {}", instance_id, e));
                        }
                        AgentEvent::Compacted { .. } | AgentEvent::Thinking => {}
                    }
                }

                agent_handle.await??;

                if !combined_output.is_empty() {
                    combined_output.push_str("\n\n---\n\n");
                }
                combined_output.push_str(&format!("[{}]:\n{}", instance_id, output));
            }

            // Store combined output in context
            {
                let mut ctx_write = ctx.write().await;
                ctx_write.node_outputs.insert(node_id.to_string(), combined_output.clone());
            }

            // Emit publication events
            if let Some(ref pre) = pub_snapshot {
                self.emit_pub_events(node_id, pre, event_tx).await;
            }

            // Determine and execute next node
            let next_node_id = self.determine_next(&node.next, &combined_output, &ctx).await?;
            let _ = event_tx.send(FlowEvent::NodeCompleted {
                node_id: node_id.to_string(),
                output: combined_output.clone(),
                next_node: next_node_id.clone(),
            }).await;

            match &node.next {
                NextNode::End => Ok(combined_output),
                NextNode::Single(next_id) => {
                    self.execute_node(flow, next_id, ctx, event_tx).await
                }
                NextNode::Conditional { .. } => {
                    if let Some(next_id) = next_node_id {
                        self.execute_node(flow, &next_id, ctx, event_tx).await
                    } else {
                        Ok(combined_output)
                    }
                }
                NextNode::Parallel { nodes, join } => {
                    self.execute_parallel(flow, nodes, join, ctx, event_tx).await
                }
            }
        }
    }

    /// Execute multiple nodes in parallel
    async fn execute_parallel(
        &self,
        flow: &Flow,
        node_ids: &[String],
        join_node: &str,
        ctx: Arc<RwLock<FlowContext>>,
        event_tx: &mpsc::Sender<FlowEvent>,
    ) -> Result<String> {
        let _ = event_tx.send(FlowEvent::ParallelStarted {
            node_ids: node_ids.to_vec(),
        }).await;

        // Spawn all parallel nodes
        let mut handles = Vec::new();

        for node_id in node_ids {
            let flow_clone = flow.clone();
            let node_id_clone = node_id.clone();
            let ctx_clone = ctx.clone();
            let event_tx_clone = event_tx.clone();
            let config_clone = self.config.clone();
            let provider_overrides = self.provider_overrides.clone();
            let model_overrides = self.model_overrides.clone();
            let publication_store = self.publication_store.clone();
            let memory_store = self.memory_store.clone();
            let mcp_handler = self.mcp_handler.clone();

            let handle = tokio::spawn(async move {
                // Create a sub-engine for parallel execution
                let sub_engine = FlowEngine {
                    config: config_clone,
                    tmux_manager: TmuxManager::new(),
                    provider_overrides,
                    model_overrides,
                    publication_store,
                    memory_store,
                    mcp_handler,
                };

                let _node = flow_clone.get_node(&node_id_clone)
                    .ok_or_else(|| anyhow::anyhow!("Node not found: {}", node_id_clone))?;

                // Build prompt
                let prompt = ctx_clone.read().await.build_prompt(&node_id_clone);
                let cwd = ctx_clone.read().await.cwd.clone();

                // Get provider
                let provider_name = sub_engine.provider_overrides
                    .get(&node_id_clone)
                    .cloned()
                    .unwrap_or_else(|| sub_engine.config.llm.default_provider.clone());

                let provider = provider::create_provider(&provider_name, &sub_engine.config)?;
                let mut agent = Agent::new(provider, &sub_engine.config, cwd)
                    .with_flow_name(&flow_clone.name);

                // Add publication tools if in publication mode
                if let Some(ref store) = sub_engine.publication_store {
                    let agent_id = format!("{}-{}", node_id_clone, uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("0"));
                    let mut handler = PublicationToolHandler::new(
                        store.clone(),
                        agent_id,
                        node_id_clone.clone(),
                    );
                    if let Some(ref ms) = sub_engine.memory_store {
                        handler = handler.with_memory(ms.clone());
                    }
                    agent = agent.with_custom_handler(Arc::new(handler));
                }

                // Add MCP tools if available
                if let Some(ref mcp) = sub_engine.mcp_handler {
                    agent = agent.with_custom_handler(mcp.clone());
                }

                let (agent_event_tx, mut agent_event_rx) = mpsc::channel::<AgentEvent>(100);

                let agent_handle = tokio::spawn(async move {
                    agent.run_oneshot(&prompt, None, agent_event_tx).await
                });

                // Collect output
                let mut output = String::new();
                while let Some(event) = agent_event_rx.recv().await {
                    match event {
                        AgentEvent::TextChunk(text) => {
                            output.push_str(&text);
                            let _ = event_tx_clone.send(FlowEvent::NodeText {
                                node_id: node_id_clone.clone(),
                                text,
                            }).await;
                        }
                        AgentEvent::Done { final_text } => {
                            output = final_text;
                        }
                        AgentEvent::Error(e) => {
                            return Err(anyhow::anyhow!("Agent error in {}: {}", node_id_clone, e));
                        }
                        _ => {}
                    }
                }

                agent_handle.await??;

                Ok::<(String, String), anyhow::Error>((node_id_clone, output))
            });

            handles.push(handle);
        }

        // Wait for all parallel nodes to complete
        let mut results = HashMap::new();
        for handle in handles {
            let (node_id, output) = handle.await??;
            results.insert(node_id.clone(), output.clone());

            // Store in context
            let mut ctx_write = ctx.write().await;
            ctx_write.node_outputs.insert(node_id, output);
        }

        let _ = event_tx.send(FlowEvent::ParallelCompleted {
            results: results.clone(),
            join_node: join_node.to_string(),
        }).await;

        // Execute join node
        self.execute_node(flow, join_node, ctx, event_tx).await
    }

    /// Determine the next node based on output and conditions
    async fn determine_next(
        &self,
        next: &NextNode,
        output: &str,
        ctx: &Arc<RwLock<FlowContext>>,
    ) -> Result<Option<String>> {
        match next {
            NextNode::End => Ok(None),
            NextNode::Single(target) => Ok(Some(target.clone())),
            NextNode::Conditional { branches } => {
                // Check each condition
                for (condition, target) in branches {
                    if condition == "default" {
                        continue; // Check default last
                    }

                    // Condition can be:
                    // - "success" / "error" - check output for error markers
                    // - A shell command to evaluate
                    // - A simple string to check in output

                    let matched = if condition == "success" {
                        !output.to_lowercase().contains("error")
                    } else if condition == "error" {
                        output.to_lowercase().contains("error")
                    } else if condition.starts_with("!") {
                        // Shell command condition
                        let cmd = &condition[1..];
                        self.eval_shell_condition(cmd, &ctx.read().await.cwd).await
                    } else {
                        // Simple string match
                        output.contains(condition)
                    };

                    if matched {
                        return Ok(Some(target.clone()));
                    }
                }

                // Fall back to default if no condition matched
                if let Some(default_target) = branches.get("default") {
                    return Ok(Some(default_target.clone()));
                }

                Ok(None)
            }
            NextNode::Parallel { nodes: _, join } => {
                // For parallel, the join node is determined by the caller
                Ok(Some(join.clone()))
            }
        }
    }

    /// Capture a snapshot of publication state for diffing
    async fn capture_pub_snapshot(&self) -> Option<PubSnapshot> {
        let store = self.publication_store.as_ref()?;
        let all = store.all().await;
        let threshold = store.consensus_threshold();
        Some(PubSnapshot {
            ids: all.iter().map(|p| p.id.clone()).collect(),
            review_counts: all.iter().map(|p| (p.id.clone(), p.reviews.len())).collect(),
            consensus_ids: all
                .iter()
                .filter(|p| p.meets_consensus(threshold))
                .map(|p| p.id.clone())
                .collect(),
            citation_sets: all
                .iter()
                .map(|p| (p.id.clone(), p.citations.iter().cloned().collect()))
                .collect(),
        })
    }

    /// Emit publication flow events by diffing pre/post snapshots
    async fn emit_pub_events(
        &self,
        node_id: &str,
        pre: &PubSnapshot,
        event_tx: &mpsc::Sender<FlowEvent>,
    ) {
        let store = match self.publication_store.as_ref() {
            Some(s) => s,
            None => return,
        };

        let all = store.all().await;
        let threshold = store.consensus_threshold();
        let post_ids: HashSet<String> = all.iter().map(|p| p.id.clone()).collect();
        let post_consensus: HashSet<String> = all
            .iter()
            .filter(|p| p.meets_consensus(threshold))
            .map(|p| p.id.clone())
            .collect();

        // Emit PublicationCreated for new publications
        for pub_item in &all {
            if !pre.ids.contains(&pub_item.id) {
                let _ = event_tx
                    .send(FlowEvent::PublicationCreated {
                        node_id: node_id.to_string(),
                        publication_id: pub_item.id.clone(),
                        title: pub_item.title.clone(),
                        author: pub_item.author.clone(),
                    })
                    .await;
            }
        }

        // Emit PublicationReviewed for new reviews
        for pub_item in &all {
            let old_count = pre.review_counts.get(&pub_item.id).copied().unwrap_or(0);
            if pub_item.reviews.len() > old_count {
                for review in pub_item.reviews.iter().skip(old_count) {
                    let _ = event_tx
                        .send(FlowEvent::PublicationReviewed {
                            publication_id: pub_item.id.clone(),
                            reviewer: review.reviewer.clone(),
                            grade: review.grade.to_string(),
                            consensus_score: pub_item.consensus_score(),
                        })
                        .await;
                }
            }
        }

        // Emit PublicationConsensus for newly-reached consensus
        for id in post_consensus.difference(&pre.consensus_ids) {
            if let Some(pub_item) = all.iter().find(|p| &p.id == id) {
                info!(
                    "Publication '{}' ({}) reached consensus with {} accepts",
                    pub_item.title, pub_item.id, pub_item.accept_count()
                );
                let _ = event_tx
                    .send(FlowEvent::PublicationConsensus {
                        publication_id: pub_item.id.clone(),
                        title: pub_item.title.clone(),
                        accept_count: pub_item.accept_count(),
                    })
                    .await;
            }
        }

        // Emit PublicationCited for new citations
        for pub_item in &all {
            let old_citations: HashSet<String> = pre
                .citation_sets
                .get(&pub_item.id)
                .cloned()
                .unwrap_or_default();
            let new_citations: HashSet<String> = pub_item.citations.iter().cloned().collect();
            for cited_id in new_citations.difference(&old_citations) {
                let _ = event_tx
                    .send(FlowEvent::PublicationCited {
                        publication_id: pub_item.id.clone(),
                        cited_id: cited_id.clone(),
                        node_id: node_id.to_string(),
                    })
                    .await;
            }
        }

        // Suppress unused warning for post_ids (used for completeness)
        let _ = post_ids;
    }

    /// Evaluate a shell command as a condition (returns true if exit code is 0)
    async fn eval_shell_condition(&self, command: &str, cwd: &str) -> bool {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(cwd)
            .output()
            .await;

        match output {
            Ok(output) => output.status.success(),
            Err(e) => {
                warn!("Failed to evaluate condition '{}': {}", command, e);
                false
            }
        }
    }
}

/// Snapshot of publication store state for diffing before/after node execution
struct PubSnapshot {
    ids: HashSet<String>,
    review_counts: HashMap<String, usize>,
    consensus_ids: HashSet<String>,
    /// Forward citation lists per publication (pub_id -> set of cited pub_ids)
    citation_sets: HashMap<String, HashSet<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{AgentNode, Coordination};

    fn test_config() -> Config {
        Config::default()
    }

    #[allow(dead_code)]
    fn simple_flow() -> Flow {
        let mut nodes = HashMap::new();
        nodes.insert(
            "start".to_string(),
            AgentNode {
                name: "Start".to_string(),
                description: "Entry point".to_string(),
                agent_type: "general".to_string(),
                role: None,
                system_prompt: None,
                allowed_tools: vec![],
                denied_tools: vec![],
                provider: None,
                model: None,
                next: NextNode::End,
                max_iterations: 10,
                timeout_secs: 0,
                inputs: vec![],
                outputs: vec![],
                count: 1,
                parallel: false,
                review_from: vec![],
                publication_tags: vec![],
            },
        );

        Flow {
            name: "test".to_string(),
            description: "Test flow".to_string(),
            version: "0.1.0".to_string(),
            entry: "start".to_string(),
            nodes,
            coordination: Coordination::default(),
        }
    }

    #[test]
    fn test_flow_context() {
        let mut ctx = FlowContext::new("test input".to_string(), "/tmp".to_string());
        ctx.node_outputs.insert("node1".to_string(), "output1".to_string());
        ctx.variables.insert("key".to_string(), "value".to_string());

        let prompt = ctx.build_prompt("node2");
        assert!(prompt.contains("test input"));
        assert!(prompt.contains("node1"));
        assert!(prompt.contains("output1"));
        assert!(prompt.contains("key: value"));
    }

    #[tokio::test]
    async fn test_determine_next_single() {
        let config = test_config();
        let engine = FlowEngine::new(config);
        let ctx = Arc::new(RwLock::new(FlowContext::default()));

        let next = NextNode::Single("target".to_string());
        let result = engine.determine_next(&next, "output", &ctx).await.unwrap();
        assert_eq!(result, Some("target".to_string()));
    }

    #[tokio::test]
    async fn test_determine_next_end() {
        let config = test_config();
        let engine = FlowEngine::new(config);
        let ctx = Arc::new(RwLock::new(FlowContext::default()));

        let next = NextNode::End;
        let result = engine.determine_next(&next, "output", &ctx).await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn test_determine_next_conditional() {
        let config = test_config();
        let engine = FlowEngine::new(config);
        let ctx = Arc::new(RwLock::new(FlowContext::default()));

        let mut branches = HashMap::new();
        branches.insert("success".to_string(), "happy_path".to_string());
        branches.insert("error".to_string(), "error_handler".to_string());
        branches.insert("default".to_string(), "fallback".to_string());

        let next = NextNode::Conditional { branches };

        // Test success condition
        let result = engine.determine_next(&next, "all good!", &ctx).await.unwrap();
        assert_eq!(result, Some("happy_path".to_string()));

        // Test error condition
        let result = engine.determine_next(&next, "Error: something failed", &ctx).await.unwrap();
        assert_eq!(result, Some("error_handler".to_string()));
    }
}
