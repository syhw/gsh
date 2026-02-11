//! MCP tool handler implementing CustomToolHandler
//!
//! Wraps one or more McpClients and exposes their tools to the agent
//! through the CustomToolHandler trait.

use super::client::{McpClient, McpToolInfo};
use crate::agent::{CustomToolHandler, CustomToolResult};
use crate::config::McpServerConfig;
use crate::provider::ToolDefinition;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{info, warn};

/// Handler that bridges MCP servers into gsh's agent tool system
pub struct McpToolHandler {
    /// Connected MCP clients
    clients: Vec<Arc<McpClient>>,
    /// Maps tool name → index into clients vec
    tool_map: HashMap<String, usize>,
    /// Cached tool definitions
    tool_defs: Vec<ToolDefinition>,
}

impl McpToolHandler {
    /// Connect to all configured MCP servers and discover their tools.
    /// Servers that fail to connect are logged as warnings (non-fatal).
    pub async fn connect(servers: &HashMap<String, McpServerConfig>) -> anyhow::Result<Self> {
        let mut clients = Vec::new();
        let mut tool_map = HashMap::new();
        let mut tool_defs = Vec::new();

        for (name, config) in servers {
            if config.transport_type != "streamable-http" {
                warn!("MCP '{}': unsupported transport '{}', skipping", name, config.transport_type);
                continue;
            }

            let client = match McpClient::new(name.clone(), config.url.clone(), config.headers.clone()) {
                Ok(c) => Arc::new(c),
                Err(e) => {
                    warn!("MCP '{}': failed to create client: {}", name, e);
                    continue;
                }
            };

            if let Err(e) = client.connect().await {
                warn!("MCP '{}': failed to connect: {:#}", name, e);
                continue;
            }

            let client_idx = clients.len();
            let tools = client.tools();

            for tool in &tools {
                if tool_map.contains_key(&tool.name) {
                    warn!(
                        "MCP '{}': tool '{}' conflicts with existing tool, skipping",
                        name, tool.name
                    );
                    continue;
                }
                tool_map.insert(tool.name.clone(), client_idx);
                tool_defs.push(mcp_tool_to_definition(tool));
            }

            clients.push(client);
        }

        info!(
            "MCP: {} server(s) connected, {} tool(s) available",
            clients.len(),
            tool_defs.len()
        );

        Ok(Self {
            clients,
            tool_map,
            tool_defs,
        })
    }

    /// Returns true if any MCP tools are available
    pub fn has_tools(&self) -> bool {
        !self.tool_defs.is_empty()
    }
}

#[async_trait]
impl CustomToolHandler for McpToolHandler {
    fn handles(&self, tool_name: &str) -> bool {
        self.tool_map.contains_key(tool_name)
    }

    async fn execute(&self, tool_name: &str, input: &serde_json::Value) -> CustomToolResult {
        let client_idx = match self.tool_map.get(tool_name) {
            Some(idx) => *idx,
            None => {
                return CustomToolResult {
                    output: format!("Unknown MCP tool: {}", tool_name),
                    success: false,
                };
            }
        };

        let client = &self.clients[client_idx];

        match client.call_tool(tool_name, input).await {
            Ok(result) => CustomToolResult {
                output: result.content,
                success: !result.is_error,
            },
            Err(e) => CustomToolResult {
                output: format!("MCP tool '{}' error: {}", tool_name, e),
                success: false,
            },
        }
    }

    fn tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_defs.clone()
    }
}

/// Convert an MCP tool info to a gsh ToolDefinition
fn mcp_tool_to_definition(tool: &McpToolInfo) -> ToolDefinition {
    ToolDefinition {
        name: tool.name.clone(),
        description: tool.description.clone(),
        input_schema: tool.input_schema.clone(),
    }
}
