use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

/// Configuration for spawning an agent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionConfig {
    /// Unique name/identifier for this agent session
    pub name: Option<String>,
    /// Working directory for the agent
    pub cwd: Option<PathBuf>,
    /// Command to run in the session (if any)
    pub command: Option<String>,
    /// Whether to run in background (don't attach immediately)
    pub background: bool,
    /// Custom environment variables
    pub env: HashMap<String, String>,
    /// Agent task description (for logging/display)
    pub task: Option<String>,
}

impl Default for AgentSessionConfig {
    fn default() -> Self {
        Self {
            name: None,
            cwd: None,
            command: None,
            background: true,
            env: HashMap::new(),
            task: None,
        }
    }
}

/// Information about a running subagent session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInfo {
    /// Session name in tmux
    pub session_name: String,
    /// Agent ID (numeric)
    pub agent_id: u64,
    /// Task description if available
    pub task: Option<String>,
    /// Working directory
    pub cwd: Option<String>,
    /// Whether the session is currently attached
    pub attached: bool,
    /// Session creation time (Unix timestamp)
    pub created_at: Option<i64>,
    /// Window count
    pub window_count: u32,
}

/// Handle to a spawned agent session
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AgentHandle {
    /// Unique agent ID
    pub id: u64,
    /// Tmux session name
    pub session_name: String,
    /// Configuration used to spawn the agent
    pub config: AgentSessionConfig,
}

/// Manages tmux sessions for subagent lifecycle
pub struct TmuxManager {
    /// Prefix for all gsh-related sessions
    session_prefix: String,
    /// Counter for generating unique agent IDs
    agent_counter: AtomicU64,
    /// Socket path for daemon communication
    daemon_socket: Option<String>,
    /// Track spawned agents
    agents: RwLock<HashMap<u64, AgentHandle>>,
}

impl TmuxManager {
    /// Create a new TmuxManager
    pub fn new() -> Self {
        Self {
            session_prefix: "gsh".to_string(),
            agent_counter: AtomicU64::new(1),
            daemon_socket: std::env::var("GSH_SOCKET").ok(),
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Create a TmuxManager with custom configuration
    #[allow(dead_code)]
    pub fn with_config(session_prefix: &str, daemon_socket: Option<&str>) -> Self {
        Self {
            session_prefix: session_prefix.to_string(),
            agent_counter: AtomicU64::new(1),
            daemon_socket: daemon_socket.map(|s| s.to_string()),
            agents: RwLock::new(HashMap::new()),
        }
    }

    /// Check if tmux is available on the system
    pub fn is_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if we're currently inside a tmux session
    #[allow(dead_code)]
    pub fn is_inside_tmux() -> bool {
        std::env::var("TMUX").is_ok()
    }

    /// Get the tmux version
    #[allow(dead_code)]
    pub fn version() -> Option<String> {
        Command::new("tmux")
            .arg("-V")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    }

    /// Spawn a new agent session in tmux
    ///
    /// Creates a new detached tmux session for the agent. The session can
    /// optionally run a command and can be attached later.
    pub fn spawn_agent_session(&self, config: AgentSessionConfig) -> Result<AgentHandle> {
        // Generate unique agent ID
        let agent_id = self.agent_counter.fetch_add(1, Ordering::SeqCst);

        // Generate session name
        let session_name = config.name.clone().unwrap_or_else(|| {
            format!("{}-agent-{:04}", self.session_prefix, agent_id)
        });

        // Ensure session name has prefix if custom name was provided
        let session_name = if !session_name.starts_with(&self.session_prefix) {
            format!("{}-{}", self.session_prefix, session_name)
        } else {
            session_name
        };

        // Build the tmux new-session command
        let mut cmd = Command::new("tmux");
        cmd.arg("new-session")
            .arg("-d") // Detached
            .arg("-s")
            .arg(&session_name);

        // Set working directory if specified
        if let Some(ref cwd) = config.cwd {
            cmd.arg("-c").arg(cwd);
        }

        // Set window name based on task
        if let Some(ref task) = config.task {
            // Truncate task for window name
            let window_name: String = task.chars().take(30).collect();
            cmd.arg("-n").arg(&window_name);
        } else {
            cmd.arg("-n").arg(format!("agent-{}", agent_id));
        }

        // Set environment variables
        for (key, value) in &config.env {
            cmd.arg("-e").arg(format!("{}={}", key, value));
        }

        // If we have a daemon socket, set it in the environment
        if let Some(ref socket) = self.daemon_socket {
            cmd.arg("-e").arg(format!("GSH_SOCKET={}", socket));
        }

        // Execute the session creation
        let output = cmd.output().context("Failed to spawn tmux session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create tmux session '{}': {}", session_name, stderr);
        }

        // If there's a command to run, send it to the session
        if let Some(ref command) = config.command {
            self.send_keys(&session_name, command)?;
        }

        // Create handle
        let handle = AgentHandle {
            id: agent_id,
            session_name: session_name.clone(),
            config,
        };

        // Track the agent
        if let Ok(mut agents) = self.agents.write() {
            agents.insert(agent_id, handle.clone());
        }

        Ok(handle)
    }

    /// List all subagent sessions managed by this TmuxManager
    ///
    /// Returns detailed information about each running agent session.
    pub fn list_subagents(&self) -> Result<Vec<SubagentInfo>> {
        // Query tmux for all sessions with detailed format
        let output = Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}\t#{session_attached}\t#{session_created}\t#{session_windows}\t#{pane_current_path}",
            ])
            .output()
            .context("Failed to list tmux sessions")?;

        if !output.status.success() {
            // No sessions is not an error - return empty list
            return Ok(Vec::new());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let agent_prefix = format!("{}-agent-", self.session_prefix);

        let mut agents = Vec::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.is_empty() {
                continue;
            }

            let session_name = parts[0];

            // Filter to only include agent sessions
            if !session_name.starts_with(&agent_prefix) {
                continue;
            }

            // Parse agent ID from session name
            let agent_id = session_name
                .strip_prefix(&agent_prefix)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);

            // Get task from our tracked agents
            let task = self
                .agents
                .read()
                .ok()
                .and_then(|agents| agents.get(&agent_id).and_then(|h| h.config.task.clone()));

            let info = SubagentInfo {
                session_name: session_name.to_string(),
                agent_id,
                task,
                cwd: parts.get(4).map(|s| s.to_string()),
                attached: parts.get(1).map(|s| *s == "1").unwrap_or(false),
                created_at: parts.get(2).and_then(|s| s.parse().ok()),
                window_count: parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(1),
            };

            agents.push(info);
        }

        // Sort by agent ID
        agents.sort_by_key(|a| a.agent_id);

        Ok(agents)
    }

    /// Attach to an existing agent session
    ///
    /// If already inside tmux, this will switch clients. Otherwise, it will
    /// attach to the session.
    #[allow(dead_code)]
    pub fn attach(&self, session: &str) -> Result<()> {
        if Self::is_inside_tmux() {
            // Use switch-client when already in tmux
            let output = Command::new("tmux")
                .arg("switch-client")
                .arg("-t")
                .arg(session)
                .output()
                .context("Failed to switch tmux client")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to switch to session '{}': {}", session, stderr);
            }
        } else {
            // Use attach-session when not in tmux
            let output = Command::new("tmux")
                .arg("attach-session")
                .arg("-t")
                .arg(session)
                .output()
                .context("Failed to attach to tmux session")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("Failed to attach to session '{}': {}", session, stderr);
            }
        }

        Ok(())
    }

    /// Pipe output from a tmux session (capture pane contents)
    ///
    /// Captures the visible content of the pane, optionally including
    /// scrollback history.
    #[allow(dead_code)]
    pub fn pipe_output(&self, session: &str, include_history: bool) -> Result<String> {
        let mut cmd = Command::new("tmux");
        cmd.arg("capture-pane")
            .arg("-t")
            .arg(session)
            .arg("-p"); // Print to stdout

        if include_history {
            // Capture all scrollback history
            cmd.arg("-S").arg("-"); // Start from the beginning
        }

        let output = cmd.output().context("Failed to capture pane output")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to capture pane from '{}': {}", session, stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Pipe output to a file or command
    ///
    /// Uses tmux's pipe-pane feature to redirect output.
    #[allow(dead_code)]
    pub fn pipe_output_to(&self, session: &str, target: &str) -> Result<()> {
        let output = Command::new("tmux")
            .arg("pipe-pane")
            .arg("-t")
            .arg(session)
            .arg(target)
            .output()
            .context("Failed to pipe pane output")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to pipe output from '{}': {}", session, stderr);
        }

        Ok(())
    }

    /// Stop piping output (disable pipe-pane)
    #[allow(dead_code)]
    pub fn stop_pipe_output(&self, session: &str) -> Result<()> {
        let output = Command::new("tmux")
            .arg("pipe-pane")
            .arg("-t")
            .arg(session)
            .output()
            .context("Failed to stop pipe-pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to stop pipe from '{}': {}", session, stderr);
        }

        Ok(())
    }

    /// Send keys to a tmux session
    ///
    /// Sends the specified text to the session as if typed by the user.
    /// Optionally sends Enter at the end.
    pub fn send_keys(&self, session: &str, keys: &str) -> Result<()> {
        self.send_keys_raw(session, keys, true)
    }

    /// Send keys without adding Enter at the end
    #[allow(dead_code)]
    pub fn send_keys_no_enter(&self, session: &str, keys: &str) -> Result<()> {
        self.send_keys_raw(session, keys, false)
    }

    /// Send keys with control over whether Enter is appended
    fn send_keys_raw(&self, session: &str, keys: &str, with_enter: bool) -> Result<()> {
        let mut cmd = Command::new("tmux");
        cmd.arg("send-keys").arg("-t").arg(session).arg(keys);

        if with_enter {
            cmd.arg("Enter");
        }

        let output = cmd.output().context("Failed to send keys")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to send keys to '{}': {}", session, stderr);
        }

        Ok(())
    }

    /// Send a control character to the session
    #[allow(dead_code)]
    pub fn send_control(&self, session: &str, key: char) -> Result<()> {
        let ctrl_key = format!("C-{}", key);
        let output = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(session)
            .arg(&ctrl_key)
            .output()
            .context("Failed to send control key")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to send control key to '{}': {}", session, stderr);
        }

        Ok(())
    }

    /// Kill an agent session
    ///
    /// Terminates the tmux session and removes it from tracking.
    pub fn kill_session(&self, session: &str) -> Result<()> {
        let output = Command::new("tmux")
            .arg("kill-session")
            .arg("-t")
            .arg(session)
            .output()
            .context("Failed to kill tmux session")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't error if session doesn't exist
            if !stderr.contains("can't find session") {
                anyhow::bail!("Failed to kill session '{}': {}", session, stderr);
            }
        }

        // Remove from tracking if present
        if let Ok(mut agents) = self.agents.write() {
            agents.retain(|_, h| h.session_name != session);
        }

        Ok(())
    }

    /// Kill an agent by its ID
    pub fn kill_agent(&self, agent_id: u64) -> Result<()> {
        let session_name = format!("{}-agent-{:04}", self.session_prefix, agent_id);
        self.kill_session(&session_name)
    }

    /// Kill all agent sessions
    #[allow(dead_code)]
    pub fn kill_all_agents(&self) -> Result<usize> {
        let agents = self.list_subagents()?;
        let count = agents.len();

        for agent in agents {
            if let Err(e) = self.kill_session(&agent.session_name) {
                tracing::warn!("Failed to kill agent session '{}': {}", agent.session_name, e);
            }
        }

        // Clear tracking
        if let Ok(mut agents) = self.agents.write() {
            agents.clear();
        }

        Ok(count)
    }

    /// Kill all gsh-prefixed tmux sessions (for daemon shutdown cleanup)
    /// Returns the list of session names that were killed
    pub fn cleanup_all_sessions() -> Vec<String> {
        // List all tmux sessions
        let output = match Command::new("tmux")
            .arg("list-sessions")
            .arg("-F")
            .arg("#{session_name}")
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };

        let mut killed_sessions = Vec::new();
        for session in String::from_utf8_lossy(&output.stdout).lines() {
            if session.starts_with("gsh-") {
                if Command::new("tmux")
                    .arg("kill-session")
                    .arg("-t")
                    .arg(session)
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false)
                {
                    tracing::debug!("Killed tmux session: {}", session);
                    killed_sessions.push(session.to_string());
                }
            }
        }

        if !killed_sessions.is_empty() {
            tracing::info!("Cleaned up {} tmux session(s): {}", killed_sessions.len(), killed_sessions.join(", "));
        }

        killed_sessions
    }

    /// Check if a session exists
    #[allow(dead_code)]
    pub fn session_exists(&self, session: &str) -> bool {
        Command::new("tmux")
            .arg("has-session")
            .arg("-t")
            .arg(session)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Get information about a specific session
    #[allow(dead_code)]
    pub fn get_session_info(&self, session: &str) -> Result<Option<SubagentInfo>> {
        let agents = self.list_subagents()?;
        Ok(agents.into_iter().find(|a| a.session_name == session))
    }

    /// Create a new window in an existing session
    #[allow(dead_code)]
    pub fn new_window(&self, session: &str, name: Option<&str>, command: Option<&str>) -> Result<()> {
        let mut cmd = Command::new("tmux");
        cmd.arg("new-window").arg("-t").arg(session);

        if let Some(n) = name {
            cmd.arg("-n").arg(n);
        }

        if let Some(c) = command {
            cmd.arg(c);
        }

        let output = cmd.output().context("Failed to create new window")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create window in '{}': {}", session, stderr);
        }

        Ok(())
    }

    /// Wait for a session to complete (pane to exit)
    ///
    /// This is useful for waiting for an agent command to complete.
    /// Returns the final output of the pane.
    #[allow(dead_code)]
    pub fn wait_for_completion(&self, session: &str, timeout_secs: u64) -> Result<String> {
        use std::time::{Duration, Instant};

        let start = Instant::now();
        let timeout = Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                anyhow::bail!("Timeout waiting for session '{}' to complete", session);
            }

            // Check if session still exists
            if !self.session_exists(session) {
                // Session ended, capture wasn't possible
                return Ok("Session has ended".to_string());
            }

            // Check pane status - look for dead pane
            let output = Command::new("tmux")
                .args(["list-panes", "-t", session, "-F", "#{pane_dead}"])
                .output()?;

            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "1" {
                    // Pane is dead (command finished), capture output
                    return self.pipe_output(session, true);
                }
            }

            // Sleep briefly before checking again
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Get the session prefix
    #[allow(dead_code)]
    pub fn prefix(&self) -> &str {
        &self.session_prefix
    }
}

impl Default for TmuxManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tmux_available() {
        // This test just checks that the availability check doesn't panic
        let _ = TmuxManager::is_available();
    }

    #[test]
    fn test_version_parsing() {
        if TmuxManager::is_available() {
            let version = TmuxManager::version();
            assert!(version.is_some());
            assert!(version.unwrap().contains("tmux"));
        }
    }

    #[test]
    fn test_default_config() {
        let config = AgentSessionConfig::default();
        assert!(config.name.is_none());
        assert!(config.cwd.is_none());
        assert!(config.command.is_none());
        assert!(config.background);
        assert!(config.env.is_empty());
        assert!(config.task.is_none());
    }

    #[test]
    fn test_session_name_prefixing() {
        let manager = TmuxManager::new();

        // Test that session names get the correct prefix
        let _config = AgentSessionConfig {
            name: Some("test-agent".to_string()),
            ..Default::default()
        };

        // We can't easily test spawn without tmux, but we can check the manager setup
        assert_eq!(manager.prefix(), "gsh");
    }
}
