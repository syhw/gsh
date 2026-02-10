use crate::config::Config;
use crate::context::ContextRetriever;
use crate::provider::ToolDefinition;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

/// Detailed result from bash command execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashResult {
    pub command: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
}

impl BashResult {
    /// Format as a combined string (for LLM consumption)
    pub fn to_output_string(&self) -> String {
        let mut result = String::new();
        if !self.stdout.is_empty() {
            result.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n--- stderr ---\n");
            }
            result.push_str(&self.stderr);
        }
        result.push_str(&format!("\n[exit code: {}]", self.exit_code));
        result
    }
}

/// Result from tool execution - can be simple string or structured
#[derive(Debug, Clone)]
pub enum ToolResult {
    /// Simple text output
    Text(String),
    /// Structured bash result with separate stdout/stderr
    Bash(BashResult),
}

impl ToolResult {
    /// Get the output as a string (for LLM)
    pub fn as_string(&self) -> String {
        match self {
            ToolResult::Text(s) => s.clone(),
            ToolResult::Bash(b) => b.to_output_string(),
        }
    }

    /// Check if the result indicates success
    pub fn is_success(&self) -> bool {
        match self {
            ToolResult::Text(_) => true,
            ToolResult::Bash(b) => b.exit_code == 0,
        }
    }
}

/// Executes tools based on LLM requests
pub struct ToolExecutor {
    config: Config,
    cwd: PathBuf,
    context_retriever: Option<Arc<ContextRetriever>>,
}

impl ToolExecutor {
    pub fn new(config: Config, cwd: String, context_retriever: Option<Arc<ContextRetriever>>) -> Self {
        Self {
            config,
            cwd: PathBuf::from(cwd),
            context_retriever,
        }
    }

    /// Get all tool definitions
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut tools = Vec::new();

        if self.config.tools.bash_enabled {
            tools.push(ToolDefinition {
                name: "bash".to_string(),
                description: "Execute a bash command and return its output. Use this for running shell commands, scripts, git operations, etc.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command to execute"
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Optional timeout in seconds (default: 30)"
                        }
                    },
                    "required": ["command"]
                }),
            });
        }

        if self.config.tools.read_enabled {
            tools.push(ToolDefinition {
                name: "read".to_string(),
                description: "Read the contents of a file. Use absolute paths or paths relative to the current working directory.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file to read"
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "Optional: start reading from this line (1-indexed)"
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Optional: stop reading at this line (inclusive)"
                        }
                    },
                    "required": ["file_path"]
                }),
            });
        }

        if self.config.tools.write_enabled {
            tools.push(ToolDefinition {
                name: "write".to_string(),
                description: "Write content to a file, creating it if it doesn't exist or overwriting if it does.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write to the file"
                        }
                    },
                    "required": ["file_path", "content"]
                }),
            });
        }

        if self.config.tools.edit_enabled {
            tools.push(ToolDefinition {
                name: "edit".to_string(),
                description: "Edit a file by replacing a specific text pattern with new text. The old_text must match exactly (including whitespace).".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the file to edit"
                        },
                        "old_text": {
                            "type": "string",
                            "description": "The exact text to find and replace"
                        },
                        "new_text": {
                            "type": "string",
                            "description": "The text to replace it with"
                        }
                    },
                    "required": ["file_path", "old_text", "new_text"]
                }),
            });
        }

        if self.config.tools.glob_enabled {
            tools.push(ToolDefinition {
                name: "glob".to_string(),
                description: "Find files matching a glob pattern. Returns a list of matching file paths.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Glob pattern to match files (e.g., '**/*.rs', 'src/*.ts')"
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Optional: directory to search from (default: current working directory)"
                        }
                    },
                    "required": ["pattern"]
                }),
            });
        }

        if self.config.tools.grep_enabled {
            tools.push(ToolDefinition {
                name: "grep".to_string(),
                description: "Search for a pattern in files. Returns matching lines with file paths and line numbers.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {
                            "type": "string",
                            "description": "Regular expression pattern to search for"
                        },
                        "path": {
                            "type": "string",
                            "description": "File or directory to search in"
                        },
                        "include": {
                            "type": "string",
                            "description": "Optional: glob pattern to filter files (e.g., '*.rs')"
                        },
                        "context_lines": {
                            "type": "integer",
                            "description": "Optional: number of context lines before and after matches"
                        }
                    },
                    "required": ["pattern"]
                }),
            });
        }

        // search_context tool (only if retriever is available)
        if self.context_retriever.is_some() {
            tools.push(ToolDefinition {
                name: "search_context".to_string(),
                description: "Search shell command history, past agent sessions, and observability logs. Use this to recall what commands were run, what the user worked on previously, past errors, or previous agent interactions.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "source": {
                            "type": "string",
                            "enum": ["shell_history", "sessions", "logs"],
                            "description": "Which data store to search:\n- shell_history: Shell command history (commands, exit codes, directories)\n- sessions: Past agent conversation sessions\n- logs: Observability logs (bash executions with stdout/stderr, tool calls)"
                        },
                        "query": {
                            "type": "string",
                            "description": "Search pattern or keyword (substring match, case-insensitive). Omit to get most recent entries."
                        },
                        "cwd": {
                            "type": "string",
                            "description": "Filter by working directory prefix. Only for shell_history."
                        },
                        "exit_code": {
                            "type": "integer",
                            "description": "Filter by exit code. Use -1 to find all failures. Only for shell_history."
                        },
                        "event_type": {
                            "type": "string",
                            "enum": ["bash_exec", "tool_call", "prompt", "error"],
                            "description": "Filter by event type. Only for logs."
                        },
                        "last_n": {
                            "type": "integer",
                            "description": "Return only the last N matching results (default: 20, max: 50)"
                        }
                    },
                    "required": ["source"]
                }),
            });
        }

        tools
    }

    /// Execute a tool by name
    pub async fn execute(&self, name: &str, input: &serde_json::Value) -> Result<ToolResult> {
        match name {
            "bash" => self.exec_bash(input).await,
            "read" => self.exec_read(input).await.map(ToolResult::Text),
            "write" => self.exec_write(input).await.map(ToolResult::Text),
            "edit" => self.exec_edit(input).await.map(ToolResult::Text),
            "glob" => self.exec_glob(input).await.map(ToolResult::Text),
            "grep" => self.exec_grep(input).await.map(ToolResult::Text),
            "search_context" => self.exec_search_context(input).await.map(ToolResult::Text),
            _ => anyhow::bail!("Unknown tool: {}", name),
        }
    }

    fn resolve_path(&self, path: &str) -> PathBuf {
        let p = Path::new(path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        }
    }

    fn is_path_excluded(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        self.config.tools.excluded_paths.iter().any(|excluded| {
            path_str.contains(excluded)
        })
    }

    async fn exec_bash(&self, input: &serde_json::Value) -> Result<ToolResult> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        let timeout_secs = input["timeout_secs"].as_u64().unwrap_or(30);

        let start = std::time::Instant::now();

        let output = Command::new("bash")
            .arg("-c")
            .arg(command)
            .current_dir(&self.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output();

        let timeout = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            output,
        )
        .await;

        let duration_ms = start.elapsed().as_millis() as u64;

        match timeout {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let exit_code = output.status.code().unwrap_or(-1);

                Ok(ToolResult::Bash(BashResult {
                    command: command.to_string(),
                    stdout,
                    stderr,
                    exit_code,
                    duration_ms,
                }))
            }
            Ok(Err(e)) => Err(anyhow::anyhow!("Command execution failed: {}", e)),
            Err(_) => Err(anyhow::anyhow!("Command timed out after {} seconds", timeout_secs)),
        }
    }

    async fn exec_read(&self, input: &serde_json::Value) -> Result<String> {
        // Debug: log the actual input received
        tracing::debug!("read tool input: {}", serde_json::to_string(input).unwrap_or_default());

        // Accept both "path" and "file_path" for compatibility with different models
        let path_str = input["path"]
            .as_str()
            .or_else(|| input["file_path"].as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'path' or 'file_path' parameter. Received: {}", input))?;

        let path = self.resolve_path(path_str);

        if self.is_path_excluded(&path) {
            anyhow::bail!("Path is excluded by configuration");
        }

        let metadata = tokio::fs::metadata(&path)
            .await
            .with_context(|| format!("Failed to stat file: {}", path.display()))?;

        if metadata.len() as usize > self.config.tools.max_file_size {
            anyhow::bail!(
                "File too large ({} bytes, max {} bytes)",
                metadata.len(),
                self.config.tools.max_file_size
            );
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let start_line = input["start_line"].as_u64().map(|n| n as usize);
        let end_line = input["end_line"].as_u64().map(|n| n as usize);

        let lines: Vec<&str> = content.lines().collect();

        let (start, end) = match (start_line, end_line) {
            (Some(s), Some(e)) => (s.saturating_sub(1), e.min(lines.len())),
            (Some(s), None) => (s.saturating_sub(1), lines.len()),
            (None, Some(e)) => (0, e.min(lines.len())),
            (None, None) => (0, lines.len()),
        };

        let selected_lines: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{:4}: {}", start + i + 1, line))
            .collect();

        Ok(selected_lines.join("\n"))
    }

    async fn exec_write(&self, input: &serde_json::Value) -> Result<String> {
        let path_str = input["file_path"]
            .as_str()
            .or_else(|| input["path"].as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path' parameter"))?;

        let content = input["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

        let path = self.resolve_path(path_str);

        if self.is_path_excluded(&path) {
            anyhow::bail!("Path is excluded by configuration");
        }

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        Ok(format!("Successfully wrote {} bytes to {}", content.len(), path.display()))
    }

    async fn exec_edit(&self, input: &serde_json::Value) -> Result<String> {
        let path_str = input["file_path"]
            .as_str()
            .or_else(|| input["path"].as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'file_path' parameter"))?;

        let old_text = input["old_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'old_text' parameter"))?;

        let new_text = input["new_text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'new_text' parameter"))?;

        let path = self.resolve_path(path_str);

        if self.is_path_excluded(&path) {
            anyhow::bail!("Path is excluded by configuration");
        }

        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        let occurrences = content.matches(old_text).count();

        if occurrences == 0 {
            anyhow::bail!("Text not found in file. Make sure 'old_text' matches exactly.");
        }

        let new_content = content.replace(old_text, new_text);

        tokio::fs::write(&path, &new_content)
            .await
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        Ok(format!(
            "Successfully replaced {} occurrence(s) in {}",
            occurrences,
            path.display()
        ))
    }

    async fn exec_glob(&self, input: &serde_json::Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' parameter"))?;

        let search_dir = input["cwd"]
            .as_str()
            .map(|s| self.resolve_path(s))
            .unwrap_or_else(|| self.cwd.clone());

        let full_pattern = search_dir.join(pattern);
        let pattern_str = full_pattern.to_string_lossy();

        let mut matches: Vec<String> = Vec::new();

        for entry in glob::glob(&pattern_str)
            .with_context(|| format!("Invalid glob pattern: {}", pattern))?
        {
            match entry {
                Ok(path) => {
                    if !self.is_path_excluded(&path) {
                        matches.push(path.display().to_string());
                    }
                }
                Err(e) => {
                    // Log but continue on individual errors
                    tracing::warn!("Glob error for entry: {}", e);
                }
            }
        }

        if matches.is_empty() {
            Ok("No files found matching pattern".to_string())
        } else {
            Ok(format!(
                "Found {} file(s):\n{}",
                matches.len(),
                matches.join("\n")
            ))
        }
    }

    async fn exec_grep(&self, input: &serde_json::Value) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'pattern' parameter"))?;

        let search_path = input["path"]
            .as_str()
            .map(|s| self.resolve_path(s))
            .unwrap_or_else(|| self.cwd.clone());

        let include = input["include"].as_str();
        let context_lines = input["context_lines"].as_u64().unwrap_or(0);

        // Build ripgrep/grep command
        let mut cmd = if which::which("rg").is_ok() {
            let mut c = Command::new("rg");
            c.arg("--line-number")
                .arg("--no-heading")
                .arg("--color=never");

            if context_lines > 0 {
                c.arg("-C").arg(context_lines.to_string());
            }

            if let Some(inc) = include {
                c.arg("--glob").arg(inc);
            }

            c.arg(pattern).arg(&search_path);
            c
        } else {
            let mut c = Command::new("grep");
            c.arg("-rn");

            if context_lines > 0 {
                c.arg(format!("-C{}", context_lines));
            }

            if let Some(inc) = include {
                c.arg("--include").arg(inc);
            }

            c.arg(pattern).arg(&search_path);
            c
        };

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run search command")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if stdout.is_empty() && stderr.is_empty() {
            Ok("No matches found".to_string())
        } else if !stdout.is_empty() {
            // Limit output to avoid overwhelming context
            let lines: Vec<&str> = stdout.lines().take(100).collect();
            let truncated = stdout.lines().count() > 100;

            let mut result = lines.join("\n");
            if truncated {
                result.push_str("\n\n[Output truncated, showing first 100 lines]");
            }
            Ok(result)
        } else {
            Ok(format!("Search error: {}", stderr))
        }
    }

    async fn exec_search_context(&self, input: &serde_json::Value) -> Result<String> {
        let retriever = self.context_retriever.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Context retriever not available"))?;

        let source = input["source"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'source' parameter"))?;

        let query = input["query"].as_str();
        let last_n = input["last_n"].as_u64().unwrap_or(20) as usize;

        match source {
            "shell_history" => {
                let cwd = input["cwd"].as_str();
                let exit_code = input["exit_code"].as_i64().map(|c| c as i32);
                retriever.search_shell_history(query, cwd, exit_code, last_n)
            }
            "sessions" => {
                let keyword = query.unwrap_or("");
                if keyword.is_empty() {
                    anyhow::bail!("'query' is required when searching sessions");
                }
                retriever.search_sessions(keyword, last_n.min(10))
            }
            "logs" => {
                let event_type = input["event_type"].as_str();
                let session_id = input.get("session_id").and_then(|v| v.as_str());
                retriever.search_logs(query, event_type, session_id, last_n)
            }
            _ => anyhow::bail!("Invalid source '{}'. Must be one of: shell_history, sessions, logs", source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{ContextAccumulator, ContextRetriever};
    use tempfile::TempDir;

    fn make_retriever(tmp: &TempDir) -> Arc<ContextRetriever> {
        let history_path = tmp.path().join("shell-history.jsonl");
        let log_dir = tmp.path().join("logs");
        let session_dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::create_dir_all(&session_dir).unwrap();
        Arc::new(ContextRetriever::new(history_path, log_dir, session_dir))
    }

    fn make_executor(tmp: &TempDir) -> ToolExecutor {
        let retriever = make_retriever(tmp);
        ToolExecutor::new(Config::default(), "/tmp".to_string(), Some(retriever))
    }

    #[test]
    fn test_search_context_tool_definition_present() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);
        let defs = executor.definitions();
        assert!(defs.iter().any(|d| d.name == "search_context"));
    }

    #[test]
    fn test_search_context_tool_definition_absent_without_retriever() {
        let executor = ToolExecutor::new(Config::default(), "/tmp".to_string(), None);
        let defs = executor.definitions();
        assert!(!defs.iter().any(|d| d.name == "search_context"));
    }

    #[tokio::test]
    async fn test_exec_search_context_shell_history() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("shell-history.jsonl");

        // Write some history events
        let mut acc = ContextAccumulator::with_history(100, history_path.clone(), 10_000);
        let now = chrono::Utc::now();
        acc.preexec("cargo build".to_string(), "/project".to_string(), now);
        acc.postcmd(0, "/project".to_string(), None, now);
        acc.preexec("cargo test".to_string(), "/project".to_string(), now);
        acc.postcmd(1, "/project".to_string(), None, now);

        let retriever = Arc::new(ContextRetriever::new(
            history_path,
            tmp.path().join("logs"),
            tmp.path().join("sessions"),
        ));
        let executor = ToolExecutor::new(Config::default(), "/tmp".to_string(), Some(retriever));

        // Search all
        let result = executor.exec_search_context(&json!({
            "source": "shell_history"
        })).await.unwrap();
        assert!(result.contains("cargo build"));
        assert!(result.contains("cargo test"));

        // Search with query
        let result = executor.exec_search_context(&json!({
            "source": "shell_history",
            "query": "build"
        })).await.unwrap();
        assert!(result.contains("cargo build"));
        assert!(!result.contains("cargo test"));

        // Search failures
        let result = executor.exec_search_context(&json!({
            "source": "shell_history",
            "exit_code": -1
        })).await.unwrap();
        assert!(result.contains("cargo test"));
        assert!(!result.contains("cargo build"));
    }

    #[tokio::test]
    async fn test_exec_search_context_sessions_requires_query() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);

        let result = executor.exec_search_context(&json!({
            "source": "sessions"
        })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("query"));
    }

    #[tokio::test]
    async fn test_exec_search_context_logs_empty() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);

        let result = executor.exec_search_context(&json!({
            "source": "logs"
        })).await.unwrap();
        assert!(result.contains("No matching"));
    }

    #[tokio::test]
    async fn test_exec_search_context_invalid_source() {
        let tmp = TempDir::new().unwrap();
        let executor = make_executor(&tmp);

        let result = executor.exec_search_context(&json!({
            "source": "invalid"
        })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid source"));
    }

    #[tokio::test]
    async fn test_exec_search_context_no_retriever() {
        let executor = ToolExecutor::new(Config::default(), "/tmp".to_string(), None);

        let result = executor.exec_search_context(&json!({
            "source": "shell_history"
        })).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not available"));
    }
}
