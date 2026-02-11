//! MCP (Model Context Protocol) client support
//!
//! Connects to external MCP servers over streamable-HTTP transport,
//! discovers their tools, and exposes them to gsh agents via the
//! CustomToolHandler trait.

pub mod client;
pub mod handler;

pub use handler::McpToolHandler;
