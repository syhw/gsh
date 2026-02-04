//! Tmux integration for gsh daemon
//!
//! This module provides functionality for managing tmux sessions,
//! particularly for spawning and managing subagent sessions.

pub mod manager;

// Re-export main types for convenience
pub use manager::{AgentSessionConfig, TmuxManager};

use anyhow::{Context, Result};
use std::process::Command;

/// Legacy TmuxManager for basic session operations
///
/// This is maintained for backward compatibility. For new code,
/// prefer using `manager::TmuxManager` which provides more comprehensive
/// subagent lifecycle management.
#[allow(dead_code)]
pub struct BasicTmuxManager {
    session_prefix: String,
}

#[allow(dead_code)]
impl BasicTmuxManager {
    pub fn new() -> Self {
        Self {
            session_prefix: "gsh".to_string(),
        }
    }

    /// Check if tmux is available
    pub fn is_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if we're inside a tmux session
    pub fn is_inside_tmux() -> bool {
        std::env::var("TMUX").is_ok()
    }

    /// Create a new tmux session with the given name
    pub fn create_session(&self, name: &str, cwd: Option<&str>) -> Result<String> {
        let session_name = format!("{}-{}", self.session_prefix, name);

        let mut cmd = Command::new("tmux");
        cmd.arg("new-session")
            .arg("-d") // detached
            .arg("-s")
            .arg(&session_name);

        if let Some(dir) = cwd {
            cmd.arg("-c").arg(dir);
        }

        let output = cmd.output().context("Failed to create tmux session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create tmux session: {}", stderr);
        }

        Ok(session_name)
    }

    /// Send keys to a tmux session
    pub fn send_keys(&self, session: &str, keys: &str) -> Result<()> {
        let output = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(session)
            .arg(keys)
            .arg("Enter")
            .output()
            .context("Failed to send keys to tmux")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to send keys: {}", stderr);
        }

        Ok(())
    }

    /// Capture the current pane content
    pub fn capture_pane(&self, session: &str) -> Result<String> {
        let output = Command::new("tmux")
            .arg("capture-pane")
            .arg("-t")
            .arg(session)
            .arg("-p") // print to stdout
            .output()
            .context("Failed to capture tmux pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to capture pane: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Kill a tmux session
    pub fn kill_session(&self, session: &str) -> Result<()> {
        let output = Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(session)
            .output()
            .context("Failed to kill tmux session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to kill session: {}", stderr);
        }

        Ok(())
    }

    /// List all gsh-prefixed sessions
    pub fn list_sessions(&self) -> Result<Vec<String>> {
        let output = Command::new("tmux")
            .arg("list-sessions")
            .arg("-F")
            .arg("#{session_name}")
            .output()
            .context("Failed to list tmux sessions")?;

        if !output.status.success() {
            // No sessions is not an error
            return Ok(Vec::new());
        }

        let sessions: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|s| s.starts_with(&self.session_prefix))
            .map(|s| s.to_string())
            .collect();

        Ok(sessions)
    }

    /// Attach to a session (only works when not already in tmux)
    pub fn attach(&self, session: &str) -> Result<()> {
        if Self::is_inside_tmux() {
            // Switch instead of attach when already in tmux
            let output = Command::new("tmux")
                .arg("switch-client")
                .arg("-t")
                .arg(session)
                .output()
                .context("Failed to switch tmux client")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to switch to session: {}", stderr);
            }
        } else {
            let output = Command::new("tmux")
                .arg("attach-session")
                .arg("-t")
                .arg(session)
                .output()
                .context("Failed to attach to tmux session")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to attach to session: {}", stderr);
            }
        }

        Ok(())
    }
}

impl Default for BasicTmuxManager {
    fn default() -> Self {
        Self::new()
    }
}
