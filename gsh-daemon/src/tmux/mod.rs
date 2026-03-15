//! Tmux integration for gsh daemon
//!
//! This module provides functionality for managing tmux sessions,
//! particularly for spawning and managing subagent sessions.

pub mod manager;

// Re-export main types for convenience
pub use manager::{AgentSessionConfig, TmuxManager};
