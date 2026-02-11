//! gsh-daemon library
//!
//! This library provides the core functionality for the gsh agentic shell daemon,
//! including LLM providers, agent execution, and flow orchestration.

pub mod agent;
pub mod config;
pub mod context;
pub mod flow;
pub mod mcp;
pub mod observability;
pub mod protocol;
pub mod provider;
pub mod session;
pub mod state;
pub mod tmux;

#[cfg(test)]
mod tests;
