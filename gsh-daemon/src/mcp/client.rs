//! MCP client for streamable-HTTP transport
//!
//! Implements the Model Context Protocol (MCP) client using JSON-RPC 2.0
//! over HTTP POST. Handles the initialize handshake, tool discovery, and
//! tool invocation lifecycle.

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use tracing::{debug, info, warn};

/// Information about a tool discovered from an MCP server
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Result from calling an MCP tool
#[derive(Debug)]
pub struct McpToolResult {
    pub content: String,
    pub is_error: bool,
}

/// MCP client for a single server endpoint
pub struct McpClient {
    /// Human-readable server name
    name: String,
    /// MCP endpoint URL
    url: String,
    /// HTTP client
    http: reqwest::Client,
    /// Custom headers (Authorization, etc.)
    custom_headers: HeaderMap,
    /// Session ID from the server (set after initialize)
    session_id: RwLock<Option<String>>,
    /// Discovered tools
    tools: RwLock<Vec<McpToolInfo>>,
    /// JSON-RPC request ID counter
    next_id: AtomicU64,
}

// JSON-RPC types

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: Option<serde_json::Value>,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize, Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[allow(dead_code)]
    data: Option<serde_json::Value>,
}

impl McpClient {
    /// Create a new MCP client (does not connect yet)
    pub fn new(name: String, url: String, headers: HashMap<String, String>) -> Result<Self> {
        let mut header_map = HeaderMap::new();
        for (key, value) in &headers {
            let expanded = expand_env(value);
            let header_name: HeaderName = key.parse()
                .with_context(|| format!("Invalid header name: {}", key))?;
            let header_value: HeaderValue = expanded.parse()
                .with_context(|| format!("Invalid header value for {}", key))?;
            header_map.insert(header_name, header_value);
        }

        Ok(Self {
            name,
            url,
            http: reqwest::Client::new(),
            custom_headers: header_map,
            session_id: RwLock::new(None),
            tools: RwLock::new(Vec::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// Server name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get discovered tools
    pub fn tools(&self) -> Vec<McpToolInfo> {
        self.tools.read().unwrap().clone()
    }

    /// Connect to the MCP server: initialize handshake + discover tools
    pub async fn connect(&self) -> Result<()> {
        // Step 1: Initialize
        let init_params = serde_json::json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {
                "roots": { "listChanged": true }
            },
            "clientInfo": {
                "name": "gsh",
                "version": env!("CARGO_PKG_VERSION")
            }
        });

        let (init_result, response_headers) = self
            .send_request_with_headers("initialize", Some(init_params))
            .await
            .with_context(|| format!("MCP initialize failed for '{}'", self.name))?;

        // Extract session ID from response headers
        if let Some(session_id) = response_headers.get("mcp-session-id") {
            if let Ok(sid) = session_id.to_str() {
                *self.session_id.write().unwrap() = Some(sid.to_string());
                debug!("MCP session ID for '{}': {}", self.name, sid);
            }
        }

        // Log server info
        if let Some(server_info) = init_result.get("serverInfo") {
            info!(
                "MCP '{}': connected to {} v{}",
                self.name,
                server_info.get("name").and_then(|v| v.as_str()).unwrap_or("unknown"),
                server_info.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
            );
        }

        // Step 2: Send initialized notification
        self.send_notification("notifications/initialized", None).await;

        // Step 3: Discover tools
        let tools_result = self
            .send_request("tools/list", Some(serde_json::json!({})))
            .await
            .with_context(|| format!("MCP tools/list failed for '{}'", self.name))?;

        let tools_array = tools_result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();

        let mut discovered = Vec::new();
        for tool in &tools_array {
            let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let description = tool.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let input_schema = tool.get("inputSchema").cloned().unwrap_or(serde_json::json!({
                "type": "object",
                "properties": {}
            }));

            if !name.is_empty() {
                discovered.push(McpToolInfo {
                    name: name.clone(),
                    description,
                    input_schema,
                });
                debug!("MCP '{}': discovered tool '{}'", self.name, name);
            }
        }

        info!("MCP '{}': {} tool(s) available", self.name, discovered.len());
        *self.tools.write().unwrap() = discovered;

        Ok(())
    }

    /// Call a tool on this MCP server
    pub async fn call_tool(&self, name: &str, arguments: &serde_json::Value) -> Result<McpToolResult> {
        let params = serde_json::json!({
            "name": name,
            "arguments": arguments
        });

        let result = self
            .send_request("tools/call", Some(params))
            .await
            .with_context(|| format!("MCP tools/call '{}' failed", name))?;

        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);

        // Extract text content from the content array
        let content = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                            item.get("text").and_then(|t| t.as_str()).map(String::from)
                        } else {
                            // For non-text content, serialize as JSON
                            Some(serde_json::to_string_pretty(item).unwrap_or_default())
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| {
                // Fallback: serialize the whole result
                serde_json::to_string_pretty(&result).unwrap_or_default()
            });

        Ok(McpToolResult { content, is_error })
    }

    /// Send a JSON-RPC request and return the result
    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let (result, _headers) = self.send_request_with_headers(method, params).await?;
        Ok(result)
    }

    /// Send a JSON-RPC request and return both result and response headers
    async fn send_request_with_headers(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(serde_json::Value, HeaderMap)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);

        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: Some(id),
            method: method.to_string(),
            params,
        };

        let body = serde_json::to_string(&request)?;
        debug!("MCP '{}' -> {} (id={})", self.name, method, id);

        let mut req = self
            .http
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .headers(self.custom_headers.clone())
            .body(body);

        // Add session ID if we have one
        if let Some(ref sid) = *self.session_id.read().unwrap() {
            req = req.header("Mcp-Session-Id", sid.as_str());
        }

        let response = req.send().await
            .with_context(|| format!("HTTP request to MCP '{}' failed", self.name))?;

        let status = response.status();
        let headers = response.headers().clone();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "MCP '{}' returned HTTP {}: {}",
                self.name,
                status,
                body
            );
        }

        // Handle SSE responses by extracting the JSON-RPC message from event stream
        let body_text = response.text().await?;

        let rpc_response: JsonRpcResponse = if content_type.contains("text/event-stream") {
            // Parse SSE: look for "data: " lines containing JSON
            self.parse_sse_response(&body_text)?
        } else {
            match serde_json::from_str::<JsonRpcResponse>(&body_text) {
                Ok(r) => r,
                Err(_) => {
                    // The server returned non-JSON-RPC response (e.g. auth error).
                    // Try to extract a useful error message from the body.
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&body_text) {
                        let msg = obj.get("msg")
                            .or_else(|| obj.get("message"))
                            .or_else(|| obj.get("error"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        anyhow::bail!("MCP '{}' server error: {}", self.name, msg);
                    }
                    anyhow::bail!(
                        "MCP '{}' returned non-JSON-RPC response: {}",
                        self.name,
                        &body_text[..body_text.len().min(200)]
                    );
                }
            }
        };

        if let Some(error) = rpc_response.error {
            anyhow::bail!(
                "MCP '{}' JSON-RPC error ({}): {}",
                self.name,
                error.code,
                error.message
            );
        }

        let result = rpc_response.result.unwrap_or(serde_json::Value::Null);
        Ok((result, headers))
    }

    /// Parse a Server-Sent Events response to extract the JSON-RPC message
    fn parse_sse_response(&self, body: &str) -> Result<JsonRpcResponse> {
        for line in body.lines() {
            let line = line.trim();
            // SSE data lines: "data: {...}" or "data:{...}"
            let data = if let Some(d) = line.strip_prefix("data: ") {
                Some(d)
            } else if let Some(d) = line.strip_prefix("data:") {
                Some(d)
            } else {
                None
            };
            if let Some(data) = data {
                let data = data.trim();
                if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(data) {
                    return Ok(response);
                }
            }
        }
        anyhow::bail!(
            "No JSON-RPC response found in SSE stream from MCP '{}'. Body: {}",
            self.name,
            &body[..body.len().min(500)]
        )
    }

    /// Send a JSON-RPC notification (no response expected)
    async fn send_notification(&self, method: &str, params: Option<serde_json::Value>) {
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id: None, // notifications have no id
            method: method.to_string(),
            params,
        };

        let body = match serde_json::to_string(&request) {
            Ok(b) => b,
            Err(e) => {
                warn!("MCP '{}': failed to serialize notification: {}", self.name, e);
                return;
            }
        };

        let mut req = self
            .http
            .post(&self.url)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .headers(self.custom_headers.clone())
            .body(body);

        if let Some(ref sid) = *self.session_id.read().unwrap() {
            req = req.header("Mcp-Session-Id", sid.as_str());
        }

        if let Err(e) = req.send().await {
            warn!("MCP '{}': notification '{}' failed: {}", self.name, method, e);
        }
    }
}

/// Expand environment variables in a string.
/// Replaces `$VAR_NAME` patterns with the corresponding env var value.
pub fn expand_env(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            // Collect variable name: [A-Za-z_][A-Za-z0-9_]*
            let mut var_name = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    var_name.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if var_name.is_empty() {
                result.push('$');
            } else {
                match std::env::var(&var_name) {
                    Ok(val) => result.push_str(&val),
                    Err(_) => {
                        warn!("Environment variable ${} not set", var_name);
                        result.push('$');
                        result.push_str(&var_name);
                    }
                }
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_no_vars() {
        assert_eq!(expand_env("hello world"), "hello world");
    }

    #[test]
    fn test_expand_env_with_var() {
        std::env::set_var("GSH_TEST_VAR", "test_value");
        assert_eq!(expand_env("Bearer $GSH_TEST_VAR"), "Bearer test_value");
        std::env::remove_var("GSH_TEST_VAR");
    }

    #[test]
    fn test_expand_env_missing_var() {
        // Missing var should leave $VAR_NAME as-is
        let result = expand_env("Bearer $NONEXISTENT_VAR_12345");
        assert_eq!(result, "Bearer $NONEXISTENT_VAR_12345");
    }

    #[test]
    fn test_expand_env_multiple_vars() {
        std::env::set_var("GSH_A", "hello");
        std::env::set_var("GSH_B", "world");
        assert_eq!(expand_env("$GSH_A $GSH_B"), "hello world");
        std::env::remove_var("GSH_A");
        std::env::remove_var("GSH_B");
    }

    #[test]
    fn test_expand_env_dollar_alone() {
        assert_eq!(expand_env("cost is $5"), "cost is $5");
    }
}
