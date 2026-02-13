# gsh: Agentic Shell Architecture Plan

> A zsh-compatible shell extension that functions as an agentic coding CLI harness, inspired by Claude Code, LLMVM, OpenCode, Gemini CLI, Codex CLI, Crush, and Mistral Vibe.

## Executive Summary

**gsh** (Gab's Shell / Agentic Shell) is designed as a **zsh plugin + daemon architecture** rather than a full shell replacement. This approach:

1. Maintains full zsh compatibility and user familiarity
2. Leverages zsh's powerful hook system for logging/interception
3. Uses tmux as the agent execution environment
4. Enables the LLM to execute Python code (LLMVM-style) for complex operations
5. Logs all shell I/O to provide rich context to the LLM

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              User's Terminal                                 │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                         tmux session: gsh-main                          │ │
│  │  ┌──────────────────────────────────────────────────────────────────┐  │ │
│  │  │  zsh + gsh.plugin.zsh                                            │  │ │
│  │  │  • Normal shell interaction                                       │  │ │
│  │  │  • All I/O logged to context                                      │  │ │
│  │  │  • "gsh <prompt>" triggers agent                                  │  │ │
│  │  └──────────────────────────────────────────────────────────────────┘  │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                                    │                                         │
│                                    ▼                                         │
│  ┌────────────────────────────────────────────────────────────────────────┐ │
│  │                          gsh-daemon (Rust)                              │ │
│  │  • Session management                                                   │ │
│  │  • Context accumulation (stdin/stdout/stderr from shell)               │ │
│  │  • LLM API orchestration                                               │ │
│  │  • Tool execution & sandboxing                                         │ │
│  │  • Subagent spawning via tmux                                          │ │
│  └────────────────────────────────────────────────────────────────────────┘ │
│                          │                    │                              │
│              ┌───────────┴────────┐    ┌──────┴──────┐                      │
│              ▼                    ▼    ▼             ▼                      │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐             │
│  │ tmux: agent-001 │  │ tmux: agent-002 │  │ tmux: agent-N   │             │
│  │ (subagent)      │  │ (subagent)      │  │ (background)    │             │
│  └─────────────────┘  └─────────────────┘  └─────────────────┘             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Core Components

### 1. Zsh Plugin (`gsh.plugin.zsh`)

The zsh plugin provides shell integration without replacing the shell.

```zsh
# gsh.plugin.zsh - Core integration

GSH_VERSION="0.1.0"
GSH_SOCKET="${GSH_SOCKET:-/tmp/gsh-$USER.sock}"
GSH_LOG_DIR="${GSH_LOG_DIR:-$HOME/.gsh/logs}"

# ═══════════════════════════════════════════════════════════════
# HOOK: Log all commands before execution
# ═══════════════════════════════════════════════════════════════
_gsh_preexec() {
    local cmd="$1"
    local timestamp=$(date +%s%3N)

    # Send to daemon via Unix socket
    _gsh_send_event "preexec" "{
        \"timestamp\": $timestamp,
        \"command\": $(printf '%s' "$cmd" | jq -Rs .),
        \"pwd\": \"$PWD\",
        \"tty\": \"$(tty)\"
    }"

    # Store for duration tracking
    _GSH_CMD_START=$timestamp
    _GSH_CMD_TEXT="$cmd"
}

# ═══════════════════════════════════════════════════════════════
# HOOK: Log command completion with exit code
# ═══════════════════════════════════════════════════════════════
_gsh_precmd() {
    local exit_code=$?
    local end_time=$(date +%s%3N)

    if [[ -n "$_GSH_CMD_START" ]]; then
        _gsh_send_event "postcmd" "{
            \"exit_code\": $exit_code,
            \"duration_ms\": $(( end_time - _GSH_CMD_START )),
            \"command\": $(printf '%s' "$_GSH_CMD_TEXT" | jq -Rs .)
        }"
        unset _GSH_CMD_START _GSH_CMD_TEXT
    fi
}

# ═══════════════════════════════════════════════════════════════
# HOOK: Log directory changes
# ═══════════════════════════════════════════════════════════════
_gsh_chpwd() {
    _gsh_send_event "chpwd" "{\"pwd\": \"$PWD\", \"oldpwd\": \"$OLDPWD\"}"
}

# ═══════════════════════════════════════════════════════════════
# FUNCTION: Send event to daemon
# ═══════════════════════════════════════════════════════════════
_gsh_send_event() {
    local event_type="$1"
    local payload="$2"

    # Non-blocking send to Unix socket
    if [[ -S "$GSH_SOCKET" ]]; then
        printf '{"event":"%s","data":%s}\n' "$event_type" "$payload" | \
            nc -U -w0 "$GSH_SOCKET" 2>/dev/null &!
    fi
}

# ═══════════════════════════════════════════════════════════════
# COMMAND: gsh - invoke the agentic assistant
# ═══════════════════════════════════════════════════════════════
# gsh is installed as a binary in ~/.local/bin/gsh
# No shell function needed — the binary handles all subcommands

# ═══════════════════════════════════════════════════════════════
# ZLE WIDGET: Ctrl+G to send current line to LLM
# ═══════════════════════════════════════════════════════════════
_gsh_send_to_llm() {
    local query="$BUFFER"
    BUFFER=""
    zle redisplay

    # Send query with context
    gsh-cli prompt --context-lines=50 "$query"
}
zle -N _gsh_send_to_llm
bindkey '^G' _gsh_send_to_llm

# ═══════════════════════════════════════════════════════════════
# ZLE WIDGET: Ctrl+X Ctrl+A to attach to agent
# ═══════════════════════════════════════════════════════════════
_gsh_attach_agent() {
    local agents=$(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^gsh-agent-')

    if [[ -z "$agents" ]]; then
        echo "\nNo active agents"
        zle redisplay
        return
    fi

    local selected=$(echo "$agents" | fzf --height=10 --prompt="Attach to agent: ")
    if [[ -n "$selected" ]]; then
        tmux attach-session -t "$selected"
    fi
    zle redisplay
}
zle -N _gsh_attach_agent
bindkey '^X^A' _gsh_attach_agent

# ═══════════════════════════════════════════════════════════════
# ZLE WIDGET: Ctrl+X Ctrl+L to list agents
# ═══════════════════════════════════════════════════════════════
_gsh_list_agents() {
    echo ""
    gsh-cli agents list
    zle redisplay
}
zle -N _gsh_list_agents
bindkey '^X^L' _gsh_list_agents

# ═══════════════════════════════════════════════════════════════
# COMPLETIONS
# ═══════════════════════════════════════════════════════════════
_gsh_completion() {
    local -a commands
    commands=(
        'chat:Start interactive chat session'
        'prompt:Send a one-off prompt'
        'agents:Manage agents'
        'sessions:Manage sessions'
        'config:View/edit configuration'
        'logs:View logs'
    )
    _describe 'command' commands
}
compdef _gsh_completion gsh-cli

# ═══════════════════════════════════════════════════════════════
# INITIALIZATION
# ═══════════════════════════════════════════════════════════════
autoload -Uz add-zsh-hook
add-zsh-hook preexec _gsh_preexec
add-zsh-hook precmd _gsh_precmd
add-zsh-hook chpwd _gsh_chpwd

# Ensure we're in tmux
if [[ -z "$TMUX" ]]; then
    echo "gsh: Warning - not running in tmux. Agent features limited."
    echo "gsh: Run 'tmux new -s gsh-main' to enable full functionality."
fi

# Start daemon if not running
if [[ ! -S "$GSH_SOCKET" ]]; then
    gsh-daemon start &!
    sleep 0.5  # Give daemon time to start
fi

echo "gsh v$GSH_VERSION loaded. Use 'gsh <prompt>' or Ctrl+G to invoke."
```

---

### 2. Daemon Architecture (`gsh-daemon`)

The daemon (written in Rust) manages context, LLM interactions, and agent spawning.

#### Directory Structure

```
gsh-daemon/
├── Cargo.toml
└── src/
    ├── main.rs              # Entry point, socket server
    ├── context/
    │   ├── mod.rs           # Context manager
    │   ├── accumulator.rs   # Accumulates shell I/O
    │   └── compactor.rs     # Context window management
    ├── agent/
    │   ├── mod.rs           # Agent loop
    │   ├── tools.rs         # Tool definitions
    │   └── python_exec.rs   # LLMVM-style Python execution
    ├── session/
    │   ├── mod.rs           # Session persistence
    │   └── storage.rs       # JSONL storage
    ├── provider/
    │   ├── mod.rs           # Provider trait
    │   ├── anthropic.rs     # Claude API
    │   ├── openai.rs        # OpenAI API
    │   └── local.rs         # Ollama/local models
    ├── tmux/
    │   ├── mod.rs           # Tmux integration
    │   └── spawner.rs       # Spawn agent sessions
    └── sandbox/
        ├── mod.rs           # Sandbox orchestration
        ├── seatbelt.rs      # macOS sandbox
        └── landlock.rs      # Linux sandbox
```

#### Context Accumulator

Captures all shell activity:

```rust
// src/context/accumulator.rs

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

pub struct ContextAccumulator {
    events: VecDeque<ShellEvent>,
    max_events: usize,
    token_count: usize,
    max_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShellEvent {
    Command {
        timestamp: u64,
        command: String,
        pwd: PathBuf,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
        stdout: Option<String>,
        stderr: Option<String>,
    },
    DirectoryChange {
        timestamp: u64,
        from: PathBuf,
        to: PathBuf,
    },
    FileChange {
        timestamp: u64,
        path: PathBuf,
        change_type: FileChangeType,
    },
    UserNote {
        timestamp: u64,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
}

impl ContextAccumulator {
    pub fn new(max_events: usize, max_tokens: usize) -> Self {
        Self {
            events: VecDeque::new(),
            max_events,
            token_count: 0,
            max_tokens,
        }
    }

    pub fn add_event(&mut self, event: ShellEvent) {
        self.events.push_back(event);

        // Prune old events if over limit
        while self.events.len() > self.max_events {
            self.events.pop_front();
        }
    }

    pub fn to_context_string(&self) -> String {
        let mut context = String::new();

        for event in &self.events {
            match event {
                ShellEvent::Command {
                    command, pwd, exit_code, duration_ms, stdout, stderr, ..
                } => {
                    context.push_str(&format!(
                        "[{}] $ {}\n",
                        pwd.display(),
                        command
                    ));

                    if let Some(out) = stdout {
                        if !out.is_empty() {
                            // Truncate long output
                            let truncated = if out.len() > 1000 {
                                format!("{}... (truncated)", &out[..1000])
                            } else {
                                out.clone()
                            };
                            context.push_str(&format!("{}\n", truncated));
                        }
                    }

                    if let Some(err) = stderr {
                        if !err.is_empty() {
                            context.push_str(&format!("stderr: {}\n", err));
                        }
                    }

                    if let Some(code) = exit_code {
                        if *code != 0 {
                            context.push_str(&format!("→ exit code: {}\n", code));
                        }
                    }

                    context.push('\n');
                }
                ShellEvent::DirectoryChange { from, to, .. } => {
                    context.push_str(&format!(
                        "[cd] {} → {}\n\n",
                        from.display(),
                        to.display()
                    ));
                }
                ShellEvent::FileChange { path, change_type, .. } => {
                    context.push_str(&format!(
                        "[file {:?}] {}\n\n",
                        change_type,
                        path.display()
                    ));
                }
                ShellEvent::UserNote { content, .. } => {
                    context.push_str(&format!("[note] {}\n\n", content));
                }
            }
        }

        context
    }

    pub fn clear(&mut self) {
        self.events.clear();
        self.token_count = 0;
    }
}
```

#### Agent Loop

LLMVM-inspired with Python execution:

```rust
// src/agent/mod.rs

use crate::context::ContextAccumulator;
use crate::provider::{CompletionRequest, Provider};
use crate::session::{Message, Session};
use crate::agent::python_exec::PythonRuntime;
use crate::agent::tools::ToolRegistry;
use anyhow::Result;

pub struct Agent {
    provider: Box<dyn Provider>,
    tools: ToolRegistry,
    python_runtime: PythonRuntime,
    session: Session,
    context: ContextAccumulator,
    tmux_session: String,
    max_steps: usize,
}

pub enum ResponseType {
    Text(String),
    ToolCalls(Vec<ToolCall>),
    PythonBlock(String),
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl Agent {
    pub async fn run(&mut self, prompt: &str) -> Result<String> {
        // Add user prompt to session
        self.session.add_message(Message::User(prompt.into()));

        let mut steps = 0;
        let mut final_response = String::new();

        loop {
            if steps >= self.max_steps {
                return Err(anyhow::anyhow!("Max steps ({}) exceeded", self.max_steps));
            }
            steps += 1;

            // Build messages with shell context
            let messages = self.build_messages();

            // Call LLM
            let response = self.provider.complete(CompletionRequest {
                messages,
                tools: Some(self.tools.definitions()),
                system: Some(self.build_system_prompt()),
                max_tokens: 4096,
            }).await?;

            // Parse response for tool calls OR Python blocks
            match self.parse_response(&response)? {
                ResponseType::Text(text) => {
                    self.session.add_message(Message::Assistant(text.clone()));
                    final_response = text;
                    break; // Done
                }

                ResponseType::ToolCalls(calls) => {
                    // Record assistant message with tool calls
                    self.session.add_message(Message::AssistantWithTools {
                        content: response.content.clone(),
                        tool_calls: calls.clone(),
                    });

                    // Execute each tool
                    for call in calls {
                        let result = self.execute_tool(&call).await?;
                        self.session.add_message(Message::ToolResult {
                            tool_call_id: call.id,
                            content: result,
                        });
                    }
                }

                ResponseType::PythonBlock(code) => {
                    // LLMVM-style: execute Python in persistent runtime
                    let result = self.python_runtime.execute(&code)?;
                    self.session.add_message(Message::PythonExecution {
                        code: code.clone(),
                        result: result.clone(),
                    });
                }
            }
        }

        Ok(final_response)
    }

    fn build_system_prompt(&self) -> String {
        let pwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "unknown".into());

        format!(r#"You are gsh, an agentic shell assistant. You help the user with programming tasks.

## Context
The user is working in a zsh shell. You have access to their recent shell history
and can see what commands they've run and their outputs.

## Capabilities

### 1. Tools
You can use the following tools:
- `bash`: Execute shell commands
- `read`: Read file contents
- `write`: Write/create files
- `edit`: Edit existing files (find/replace)
- `glob`: Find files by pattern
- `grep`: Search file contents
- `git_worktree`: Manage git worktrees for parallel work

### 2. Python Execution
For complex operations, you can write Python code in <python> blocks:

<python>
import json
import requests

# Your code here
result = some_complex_operation()
print(json.dumps(result))  # Output is captured and returned to you
</python>

The Python environment persists between calls - variables and imports are retained.
Available helpers:
- `run_cmd(cmd)` - Run shell command, returns {{"stdout", "stderr", "returncode"}}
- `read_file(path)` - Read file contents
- `write_file(path, content)` - Write to file
- `llm_call(prompt)` - Call LLM with a sub-prompt for nested reasoning

## Current Shell Context

**Working directory:** {pwd}

**Recent shell activity:**
```
{context}
```

## Guidelines

1. **Prefer simple bash commands** for straightforward tasks
2. **Use Python** for data processing, API calls, or multi-step logic
3. **Always explain** what you're doing before executing
4. **Ask for confirmation** before destructive operations (rm, overwrite, etc.)
5. **Be concise** - the user is in a terminal environment
6. **Show progress** - for long operations, provide updates
"#,
            pwd = pwd,
            context = self.context.to_context_string()
        )
    }

    fn build_messages(&self) -> Vec<Message> {
        self.session.messages.clone()
    }

    fn parse_response(&self, response: &str) -> Result<ResponseType> {
        // Check for Python blocks
        if let Some(code) = self.extract_python_block(response) {
            return Ok(ResponseType::PythonBlock(code));
        }

        // Check for tool calls (provider-specific parsing)
        if let Some(calls) = self.extract_tool_calls(response) {
            return Ok(ResponseType::ToolCalls(calls));
        }

        // Plain text response
        Ok(ResponseType::Text(response.to_string()))
    }

    fn extract_python_block(&self, response: &str) -> Option<String> {
        let start = response.find("<python>")?;
        let end = response.find("</python>")?;

        if start < end {
            Some(response[start + 8..end].trim().to_string())
        } else {
            None
        }
    }

    fn extract_tool_calls(&self, _response: &str) -> Option<Vec<ToolCall>> {
        // Provider-specific tool call extraction
        // This would parse the LLM's response format
        None
    }

    async fn execute_tool(&self, call: &ToolCall) -> Result<String> {
        self.tools.execute(&call.name, &call.arguments).await
    }
}
```

#### Python Runtime (LLMVM-inspired)

```rust
// src/agent/python_exec.rs

use pyo3::prelude::*;
use pyo3::types::PyDict;
use anyhow::Result;

pub struct PythonRuntime {
    initialized: bool,
}

impl PythonRuntime {
    pub fn new() -> Result<Self> {
        // Initialize Python with helper functions
        Python::with_gil(|py| {
            py.run(r#"
import os
import sys
import json
import subprocess
import pathlib
from typing import *

# ═══════════════════════════════════════════════════════════════
# Helper functions available to LLM-generated code
# ═══════════════════════════════════════════════════════════════

def run_cmd(cmd: str, timeout: int = 120) -> dict:
    """Run a shell command and return result.

    Args:
        cmd: Shell command to execute
        timeout: Timeout in seconds

    Returns:
        dict with 'stdout', 'stderr', 'returncode'
    """
    try:
        result = subprocess.run(
            cmd,
            shell=True,
            capture_output=True,
            text=True,
            timeout=timeout
        )
        return {
            "stdout": result.stdout,
            "stderr": result.stderr,
            "returncode": result.returncode,
            "success": result.returncode == 0
        }
    except subprocess.TimeoutExpired:
        return {
            "stdout": "",
            "stderr": f"Command timed out after {timeout}s",
            "returncode": -1,
            "success": False
        }

def read_file(path: str) -> str:
    """Read file contents.

    Args:
        path: Path to file (relative or absolute)

    Returns:
        File contents as string
    """
    return pathlib.Path(path).expanduser().read_text()

def write_file(path: str, content: str) -> None:
    """Write content to file.

    Args:
        path: Path to file
        content: Content to write
    """
    p = pathlib.Path(path).expanduser()
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(content)

def append_file(path: str, content: str) -> None:
    """Append content to file.

    Args:
        path: Path to file
        content: Content to append
    """
    p = pathlib.Path(path).expanduser()
    with open(p, 'a') as f:
        f.write(content)

def list_files(pattern: str = "*") -> list:
    """List files matching pattern.

    Args:
        pattern: Glob pattern (default: "*")

    Returns:
        List of matching file paths
    """
    import glob
    return glob.glob(pattern, recursive=True)

def llm_call(prompt: str) -> str:
    """Call LLM with a sub-prompt (via IPC to daemon).

    This enables nested LLM calls for complex reasoning.

    Args:
        prompt: The prompt to send to the LLM

    Returns:
        LLM response as string
    """
    import socket
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    socket_path = os.environ.get('GSH_SOCKET', '/tmp/gsh.sock')
    sock.connect(socket_path)

    request = json.dumps({"type": "llm_call", "prompt": prompt})
    sock.send(request.encode() + b'\n')

    response = b''
    while True:
        chunk = sock.recv(4096)
        if not chunk:
            break
        response += chunk
        if b'\n' in chunk:
            break

    sock.close()
    result = json.loads(response.decode())
    return result.get("result", "")

def http_get(url: str, headers: dict = None) -> dict:
    """Make HTTP GET request.

    Args:
        url: URL to fetch
        headers: Optional headers dict

    Returns:
        dict with 'status', 'headers', 'body'
    """
    import urllib.request
    req = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return {
                "status": resp.status,
                "headers": dict(resp.headers),
                "body": resp.read().decode('utf-8', errors='replace')
            }
    except Exception as e:
        return {"error": str(e)}

def http_post(url: str, data: dict, headers: dict = None) -> dict:
    """Make HTTP POST request with JSON body.

    Args:
        url: URL to post to
        data: Data to send (will be JSON encoded)
        headers: Optional headers dict

    Returns:
        dict with 'status', 'headers', 'body'
    """
    import urllib.request
    headers = headers or {}
    headers['Content-Type'] = 'application/json'

    req = urllib.request.Request(
        url,
        data=json.dumps(data).encode(),
        headers=headers,
        method='POST'
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return {
                "status": resp.status,
                "headers": dict(resp.headers),
                "body": resp.read().decode('utf-8', errors='replace')
            }
    except Exception as e:
        return {"error": str(e)}

# Print helper for structured output
def output(data):
    """Output data in a format the agent can parse."""
    if isinstance(data, (dict, list)):
        print(json.dumps(data, indent=2, default=str))
    else:
        print(data)

print("gsh Python runtime initialized")
            "#, None, None)?;

            Ok::<_, PyErr>(())
        })?;

        Ok(Self { initialized: true })
    }

    pub fn execute(&self, code: &str) -> Result<String> {
        if !self.initialized {
            return Err(anyhow::anyhow!("Python runtime not initialized"));
        }

        Python::with_gil(|py| {
            // Capture stdout
            py.run(r#"
import io, sys
_gsh_stdout = io.StringIO()
_gsh_stderr = io.StringIO()
_gsh_old_stdout = sys.stdout
_gsh_old_stderr = sys.stderr
sys.stdout = _gsh_stdout
sys.stderr = _gsh_stderr
            "#, None, None)?;

            // Execute user code
            let exec_result = py.run(code, None, None);

            // Restore stdout/stderr and get output
            py.run(r#"
sys.stdout = _gsh_old_stdout
sys.stderr = _gsh_old_stderr
_gsh_output = _gsh_stdout.getvalue()
_gsh_errors = _gsh_stderr.getvalue()
            "#, None, None)?;

            let locals = PyDict::new(py);
            py.run("_result = (_gsh_output, _gsh_errors)", None, Some(locals))?;

            let output: String = py.eval("_gsh_output", None, None)?.extract()?;
            let errors: String = py.eval("_gsh_errors", None, None)?.extract()?;

            match exec_result {
                Ok(_) => {
                    let mut result = output;
                    if !errors.is_empty() {
                        result.push_str("\n[stderr]: ");
                        result.push_str(&errors);
                    }
                    Ok(result)
                }
                Err(e) => {
                    Ok(format!(
                        "Error: {}\nOutput before error: {}\nStderr: {}",
                        e, output, errors
                    ))
                }
            }
        })
    }
}
```

---

### 3. Tmux Integration

#### Spawner for Agent Sessions

```rust
// src/tmux/spawner.rs

use anyhow::Result;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct TmuxSpawner {
    session_prefix: String,
    agent_counter: AtomicU64,
    daemon_socket: String,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub prompt: String,
    pub tools: Vec<String>,
    pub max_steps: usize,
    pub background: bool,
}

#[derive(Debug)]
pub struct AgentHandle {
    pub id: u64,
    pub session_name: String,
    pub config: AgentConfig,
}

impl TmuxSpawner {
    pub fn new(session_prefix: &str, daemon_socket: &str) -> Self {
        Self {
            session_prefix: session_prefix.to_string(),
            agent_counter: AtomicU64::new(1),
            daemon_socket: daemon_socket.to_string(),
        }
    }

    /// Spawn a new agent in a tmux session
    pub fn spawn_agent(&self, config: AgentConfig) -> Result<AgentHandle> {
        let agent_id = self.agent_counter.fetch_add(1, Ordering::SeqCst);
        let session_name = format!("{}-agent-{:04}", self.session_prefix, agent_id);

        // Create tmux session (detached)
        Command::new("tmux")
            .args(["new-session", "-d", "-s", &session_name])
            .status()?;

        // Set window title
        Command::new("tmux")
            .args([
                "rename-window", "-t", &session_name,
                &format!("Agent {}", agent_id)
            ])
            .status()?;

        // Build the agent command
        let config_json = serde_json::to_string(&config)?;
        let agent_cmd = format!(
            "GSH_SOCKET={} gsh-agent --id {} --config '{}'",
            self.daemon_socket,
            agent_id,
            config_json.replace("'", "'\\''")  // Escape single quotes
        );

        // Send the command to the tmux session
        Command::new("tmux")
            .args(["send-keys", "-t", &session_name, &agent_cmd, "Enter"])
            .status()?;

        Ok(AgentHandle {
            id: agent_id,
            session_name,
            config,
        })
    }

    /// Spawn agent in background (user doesn't see it immediately)
    pub fn spawn_background(&self, config: AgentConfig) -> Result<AgentHandle> {
        let mut cfg = config;
        cfg.background = true;
        self.spawn_agent(cfg)
    }

    /// Spawn agent and attach user's terminal to it
    pub fn spawn_foreground(&self, config: AgentConfig) -> Result<AgentHandle> {
        let handle = self.spawn_agent(config)?;

        // Attach to the session
        Command::new("tmux")
            .args(["attach-session", "-t", &handle.session_name])
            .status()?;

        Ok(handle)
    }

    /// List all active agent sessions
    pub fn list_agents(&self) -> Result<Vec<String>> {
        let output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()?;

        let sessions = String::from_utf8_lossy(&output.stdout);
        let agents: Vec<String> = sessions
            .lines()
            .filter(|s| s.starts_with(&format!("{}-agent-", self.session_prefix)))
            .map(|s| s.to_string())
            .collect();

        Ok(agents)
    }

    /// Kill an agent session
    pub fn kill_agent(&self, session_name: &str) -> Result<()> {
        Command::new("tmux")
            .args(["kill-session", "-t", session_name])
            .status()?;
        Ok(())
    }

    /// Attach to an existing agent session
    pub fn attach(&self, session_name: &str) -> Result<()> {
        Command::new("tmux")
            .args(["attach-session", "-t", session_name])
            .status()?;
        Ok(())
    }
}
```

---

### 4. Tool System

```rust
// src/agent/tools.rs

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio::process::Command;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value;
    async fn execute(&self, args: Value) -> Result<String>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };

        // Register built-in tools
        registry.register(Box::new(BashTool::new()));
        registry.register(Box::new(ReadTool));
        registry.register(Box::new(WriteTool));
        registry.register(Box::new(EditTool));
        registry.register(Box::new(GlobTool));
        registry.register(Box::new(GrepTool));
        registry.register(Box::new(GitWorktreeTool));

        registry
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn definitions(&self) -> Vec<Value> {
        self.tools.values().map(|t| {
            json!({
                "name": t.name(),
                "description": t.description(),
                "parameters": t.parameters()
            })
        }).collect()
    }

    pub async fn execute(&self, name: &str, args: &Value) -> Result<String> {
        let tool = self.tools.get(name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", name))?;
        tool.execute(args.clone()).await
    }
}

// ═══════════════════════════════════════════════════════════════
// BASH TOOL
// ═══════════════════════════════════════════════════════════════

pub struct BashTool {
    blocked_commands: Vec<String>,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            blocked_commands: vec![
                "rm -rf /".into(),
                ":(){ :|:& };:".into(),  // Fork bomb
                "> /dev/sda".into(),
            ],
        }
    }

    fn is_blocked(&self, cmd: &str) -> bool {
        self.blocked_commands.iter().any(|blocked| cmd.contains(blocked))
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }

    fn description(&self) -> &str {
        "Execute a bash command. Use for git operations, file management, builds, tests, etc."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The bash command to execute"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 120000)"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the command"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let command = args["command"].as_str()
            .ok_or_else(|| anyhow::anyhow!("command is required"))?;

        if self.is_blocked(command) {
            return Ok("Error: This command is blocked for safety reasons.".into());
        }

        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(120000);
        let working_dir = args["working_dir"].as_str();

        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(command);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            cmd.output()
        ).await??;

        let mut result = String::new();

        if !output.stdout.is_empty() {
            result.push_str(&String::from_utf8_lossy(&output.stdout));
        }

        if !output.stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n[stderr]:\n");
            }
            result.push_str(&String::from_utf8_lossy(&output.stderr));
        }

        if !output.status.success() {
            result.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
        }

        Ok(result)
    }
}

// ═══════════════════════════════════════════════════════════════
// READ TOOL
// ═══════════════════════════════════════════════════════════════

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str { "read" }

    fn description(&self) -> &str {
        "Read the contents of a file. Supports line range selection."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "start_line": {
                    "type": "integer",
                    "description": "Starting line number (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Ending line number (inclusive)"
                }
            },
            "required": ["path"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;

        let content = tokio::fs::read_to_string(path).await?;

        let start = args["start_line"].as_u64().map(|n| n as usize);
        let end = args["end_line"].as_u64().map(|n| n as usize);

        match (start, end) {
            (Some(s), Some(e)) => {
                let lines: Vec<&str> = content.lines().collect();
                let selected: Vec<String> = lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i + 1 >= s && *i + 1 <= e)
                    .map(|(i, line)| format!("{:4}│{}", i + 1, line))
                    .collect();
                Ok(selected.join("\n"))
            }
            _ => {
                // Return with line numbers
                let numbered: Vec<String> = content
                    .lines()
                    .enumerate()
                    .map(|(i, line)| format!("{:4}│{}", i + 1, line))
                    .collect();
                Ok(numbered.join("\n"))
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// WRITE TOOL
// ═══════════════════════════════════════════════════════════════

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str { "write" }

    fn description(&self) -> &str {
        "Write content to a file. Creates parent directories if needed. Overwrites existing files."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let content = args["content"].as_str()
            .ok_or_else(|| anyhow::anyhow!("content is required"))?;

        // Create parent directories
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        tokio::fs::write(path, content).await?;

        Ok(format!("Wrote {} bytes to {}", content.len(), path))
    }
}

// ═══════════════════════════════════════════════════════════════
// EDIT TOOL
// ═══════════════════════════════════════════════════════════════

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str { "edit" }

    fn description(&self) -> &str {
        "Edit an existing file using find/replace. The old_text must match exactly."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_text": {
                    "type": "string",
                    "description": "Exact text to find and replace"
                },
                "new_text": {
                    "type": "string",
                    "description": "Text to replace with"
                }
            },
            "required": ["path", "old_text", "new_text"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let path = args["path"].as_str()
            .ok_or_else(|| anyhow::anyhow!("path is required"))?;
        let old_text = args["old_text"].as_str()
            .ok_or_else(|| anyhow::anyhow!("old_text is required"))?;
        let new_text = args["new_text"].as_str()
            .ok_or_else(|| anyhow::anyhow!("new_text is required"))?;

        let content = tokio::fs::read_to_string(path).await?;

        if !content.contains(old_text) {
            return Ok(format!(
                "Error: Could not find the specified text in {}. \
                Make sure old_text matches exactly (including whitespace).",
                path
            ));
        }

        let new_content = content.replacen(old_text, new_text, 1);
        tokio::fs::write(path, &new_content).await?;

        Ok(format!("Edited {}", path))
    }
}

// ═══════════════════════════════════════════════════════════════
// GLOB TOOL
// ═══════════════════════════════════════════════════════════════

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Find files matching a glob pattern (e.g., '**/*.rs', 'src/**/*.ts')"
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files"
                },
                "path": {
                    "type": "string",
                    "description": "Base directory to search in (default: current directory)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let pattern = args["pattern"].as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;
        let base_path = args["path"].as_str().unwrap_or(".");

        let full_pattern = format!("{}/{}", base_path, pattern);

        let mut files = Vec::new();
        for entry in glob::glob(&full_pattern)? {
            if let Ok(path) = entry {
                files.push(path.display().to_string());
            }
        }

        if files.is_empty() {
            Ok("No files found matching pattern".into())
        } else {
            Ok(files.join("\n"))
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// GREP TOOL
// ═══════════════════════════════════════════════════════════════

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }

    fn description(&self) -> &str {
        "Search for a pattern in files. Uses ripgrep for fast searching."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in"
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Only search files matching this glob (e.g., '*.rs')"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of context lines to show (default: 2)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let pattern = args["pattern"].as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;
        let path = args["path"].as_str().unwrap_or(".");
        let context = args["context_lines"].as_u64().unwrap_or(2);

        let mut cmd = Command::new("rg");
        cmd.args([
            "--line-number",
            "--color=never",
            &format!("-C{}", context),
            pattern,
            path
        ]);

        if let Some(file_pattern) = args["file_pattern"].as_str() {
            cmd.args(["-g", file_pattern]);
        }

        let output = cmd.output().await?;

        if output.stdout.is_empty() {
            Ok("No matches found".into())
        } else {
            Ok(String::from_utf8_lossy(&output.stdout).into())
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// GIT WORKTREE TOOL
// ═══════════════════════════════════════════════════════════════

pub struct GitWorktreeTool;

#[async_trait]
impl Tool for GitWorktreeTool {
    fn name(&self) -> &str { "git_worktree" }

    fn description(&self) -> &str {
        "Manage git worktrees for parallel development. Create isolated working directories for different branches."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "remove"],
                    "description": "Action to perform"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name (for create)"
                },
                "path": {
                    "type": "string",
                    "description": "Worktree path (for create/remove)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> Result<String> {
        let action = args["action"].as_str()
            .ok_or_else(|| anyhow::anyhow!("action is required"))?;

        match action {
            "create" => {
                let branch = args["branch"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("branch is required for create"))?;
                let path = args["path"].as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("../{}-worktree", branch));

                let output = Command::new("git")
                    .args(["worktree", "add", "-b", branch, &path])
                    .output()
                    .await?;

                if output.status.success() {
                    Ok(format!("Created worktree at {} on branch {}", path, branch))
                } else {
                    Ok(format!("Error: {}", String::from_utf8_lossy(&output.stderr)))
                }
            }
            "list" => {
                let output = Command::new("git")
                    .args(["worktree", "list"])
                    .output()
                    .await?;
                Ok(String::from_utf8_lossy(&output.stdout).into())
            }
            "remove" => {
                let path = args["path"].as_str()
                    .ok_or_else(|| anyhow::anyhow!("path is required for remove"))?;

                let output = Command::new("git")
                    .args(["worktree", "remove", path])
                    .output()
                    .await?;

                if output.status.success() {
                    Ok(format!("Removed worktree at {}", path))
                } else {
                    Ok(format!("Error: {}", String::from_utf8_lossy(&output.stderr)))
                }
            }
            _ => Err(anyhow::anyhow!("Unknown action: {}", action))
        }
    }
}
```

---

### 5. Session Persistence

```rust
// src/session/mod.rs

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use anyhow::Result;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub working_dir: PathBuf,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Message {
    User(String),
    Assistant(String),
    AssistantWithTools {
        content: String,
        tool_calls: Vec<crate::agent::ToolCall>,
    },
    ToolResult {
        tool_call_id: String,
        content: String,
    },
    PythonExecution {
        code: String,
        result: String,
    },
    System(String),
}

impl Session {
    pub fn new(working_dir: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            working_dir,
            messages: Vec::new(),
        }
    }

    pub fn add_message(&mut self, msg: Message) {
        self.messages.push(msg);
        self.updated_at = Utc::now();
    }

    pub fn save(&self, session_dir: &PathBuf) -> Result<()> {
        let path = session_dir.join(format!("{}.json", self.id));
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn load(session_dir: &PathBuf, id: &str) -> Result<Self> {
        let path = session_dir.join(format!("{}.json", id));
        let content = std::fs::read_to_string(path)?;
        let session: Session = serde_json::from_str(&content)?;
        Ok(session)
    }

    pub fn list(session_dir: &PathBuf) -> Result<Vec<SessionInfo>> {
        let mut sessions = Vec::new();

        for entry in std::fs::read_dir(session_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(session) = serde_json::from_str::<Session>(&content) {
                        sessions.push(SessionInfo {
                            id: session.id,
                            created_at: session.created_at,
                            message_count: session.messages.len(),
                            working_dir: session.working_dir,
                        });
                    }
                }
            }
        }

        sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(sessions)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub message_count: usize,
    pub working_dir: PathBuf,
}
```

---

## Configuration

### Default Configuration File

```toml
# ~/.config/gsh/config.toml

[general]
log_dir = "~/.gsh/logs"
session_dir = "~/.gsh/sessions"
socket_path = "/tmp/gsh-$USER.sock"

[providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"
max_tokens = 4096

[providers.openai]
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"
max_tokens = 4096

[providers.local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3.2"

[agent]
default_provider = "anthropic"
max_steps = 50
# Tools that execute without asking for permission
auto_approve = ["read", "glob", "grep"]

[sandbox]
enabled = true
allow_network = false
writable_paths = [".", "./build", "./dist", "./target"]

[tmux]
session_prefix = "gsh"
default_shell = "/bin/zsh"

[python]
enabled = true
venv_path = "~/.gsh/python-venv"
# Packages to pre-install in the Python environment
pre_install = ["requests", "beautifulsoup4", "pandas", "pyyaml"]

[keybindings]
invoke_llm = "^G"           # Ctrl+G - send buffer to LLM
attach_agent = "^X^A"       # Ctrl+X Ctrl+A - attach to agent
list_agents = "^X^L"        # Ctrl+X Ctrl+L - list agents
cancel_agent = "^X^C"       # Ctrl+X Ctrl+C - cancel current agent

[context]
max_events = 100            # Max shell events to keep in context
max_tokens = 8000           # Max tokens for context window
include_stdout = true       # Include command output in context
stdout_max_lines = 50       # Max lines of stdout per command
```

---

## User Interaction Examples

### Normal Shell Usage (All Logged)

```bash
$ cd ~/projects/myapp
$ git status
On branch main
Changes not staged for commit:
  modified:   src/auth.rs

$ cargo test
running 12 tests
test auth::test_login ... FAILED

# All of this is automatically logged and available as context for the LLM
```

### Invoke Agent with Prompt

```bash
$ gsh help me fix the failing test

gsh> Analyzing your recent commands and test output...
gsh> I see `cargo test` failed with an error in auth::test_login

gsh> [Tool: read] Reading src/auth.rs...
gsh> [Tool: read] Reading tests/auth_test.rs...

gsh> The issue is in the assertion. The function returns a Result, but the test
gsh> expects a boolean. Let me fix it:

gsh> [Tool: edit] Updating tests/auth_test.rs:42
     - assert!(login_result);
     + assert!(login_result.is_ok());

gsh> [Tool: bash] cargo test auth::test_login
     running 1 test
     test auth::test_login ... ok

gsh> Fixed! The test was checking the Result directly instead of using .is_ok()
```

### Background Agent

```bash
$ gsh --background explore how the authentication system works

[gsh] Spawned agent gsh-agent-0012 (background)
[gsh] Use 'tmux attach -t gsh-agent-0012' to observe
[gsh] Or press Ctrl+X Ctrl+A to select and attach

$ # Continue working while agent explores...
$ vim src/components/Login.tsx

[gsh] Agent gsh-agent-0012 completed. Results available.

$ gsh show 0012

gsh> ## Authentication System Analysis
gsh>
gsh> ### Overview
gsh> The authentication system uses JWT tokens with the following flow:
gsh>
gsh> 1. User submits credentials to `/api/auth/login`
gsh> 2. Server validates and returns JWT + refresh token
gsh> 3. Client stores tokens in httpOnly cookies
gsh> ...
```

### Python Execution (LLMVM-style)

```bash
$ gsh analyze the API response times from our logs

gsh> I'll analyze the logs using Python for better data processing.

gsh> <python>
gsh> import json
gsh> from collections import defaultdict
gsh> from statistics import mean, median
gsh>
gsh> # Parse log file
gsh> response_times = defaultdict(list)
gsh> with open('logs/api.log') as f:
gsh>     for line in f:
gsh>         data = json.loads(line)
gsh>         endpoint = data['endpoint']
gsh>         response_times[endpoint].append(data['duration_ms'])
gsh>
gsh> # Calculate statistics
gsh> for endpoint, times in sorted(response_times.items()):
gsh>     print(f"{endpoint}:")
gsh>     print(f"  avg: {mean(times):.1f}ms")
gsh>     print(f"  median: {median(times):.1f}ms")
gsh>     print(f"  max: {max(times):.1f}ms")
gsh> </python>

gsh> [Python output]:
gsh> /api/users:
gsh>   avg: 45.2ms
gsh>   median: 38.0ms
gsh>   max: 234.0ms
gsh> /api/orders:
gsh>   avg: 123.5ms
gsh>   median: 98.0ms
gsh>   max: 892.0ms
gsh>
gsh> The /api/orders endpoint has high variance. The max of 892ms suggests
gsh> occasional slow queries. Would you like me to investigate further?
```

---

## Key Design Decisions

### 1. Why Zsh Plugin vs. Custom Shell?

| Approach | Pros | Cons |
|----------|------|------|
| **Zsh Plugin** ✓ | Full compatibility, existing ecosystem, user familiarity, oh-my-zsh integration | Limited to zsh hooks |
| Custom Shell | Complete control | Users must learn new shell, no plugin ecosystem |

**Decision**: Zsh plugin. The hooks (`preexec`, `precmd`, `chpwd`, `zshaddhistory`) provide sufficient interception points. ZLE widgets enable custom keybindings.

### 2. Why Tmux-Mandatory?

1. **Agent Isolation**: Each agent runs in its own tmux session, can be attached/detached
2. **Persistent Context**: Agents survive terminal disconnection
3. **Parallel Work**: Multiple agents work simultaneously in separate sessions
4. **User Visibility**: Human can `tmux attach -t gsh-agent-001` anytime to observe
5. **Background Execution**: Agents can run while user does other work

### 3. Why LLMVM-Style Python Execution?

From LLMVM research, this enables:
- **Complex Logic**: LLM writes Python for data processing, not just shell commands
- **Persistent State**: Variables persist across executions within a session
- **Self-Orchestration**: LLM can call itself via `llm_call()` for sub-tasks
- **Error Correction**: Backtrack and rewrite code on failures
- **Rich Libraries**: Access to requests, pandas, json, etc.

### 4. Subagent Architecture

Inspired by Claude Code's subagent system:

```
Main Agent (foreground)
├── Explore Agent (background) - Read-only codebase exploration
├── Test Agent (background) - Running test suites
└── Build Agent (background) - Compilation/bundling
```

Each subagent:
- Has its own tmux session
- Can be promoted to foreground (`tmux attach`)
- Reports results back to main agent
- Has configurable tool permissions

---

## Implementation Phases

### Phase 1: Core Foundation
- [ ] Zsh plugin with hooks (preexec, precmd, chpwd)
- [ ] Daemon skeleton with Unix socket server
- [ ] Basic context accumulation
- [ ] Single LLM provider (Anthropic Claude)
- [ ] Basic tools (bash, read, write, edit, glob, grep)

### Phase 2: Tmux Integration
- [ ] Agent spawning in tmux sessions
- [ ] Foreground/background agent support
- [ ] Agent attachment keybindings (Ctrl+X Ctrl+A)
- [ ] Agent listing and management
- [ ] Session persistence (save/load)

### Phase 3: Python Runtime
- [ ] PyO3 integration for embedded Python
- [ ] Persistent interpreter with state
- [ ] Helper functions (run_cmd, read_file, write_file, llm_call)
- [ ] Error handling and recovery
- [ ] Output capture and formatting

### Phase 4: Advanced Features
- [ ] Subagent architecture (explore, test, build agents)
- [ ] Git worktree integration for parallel work
- [ ] Sandbox (macOS Seatbelt, Linux Landlock)
- [ ] MCP protocol support for tool extensibility
- [ ] Multiple LLM providers (OpenAI, Ollama, etc.)

### Phase 5: Polish
- [ ] Zsh completions and autosuggestions
- [ ] Session replay and resume
- [ ] Cost tracking and token usage
- [ ] Configuration UI / TUI
- [ ] Documentation and examples

---

## Technology Stack Summary

| Component | Technology | Rationale |
|-----------|------------|-----------|
| Shell Integration | Zsh plugin | Compatibility, hooks, ZLE widgets |
| Daemon | Rust + Tokio | Performance, safety, async I/O |
| Python Runtime | PyO3 | Embedded Python, persistent state |
| Agent Sessions | Tmux | Isolation, observability, persistence |
| IPC | Unix sockets | Fast, local-only, secure |
| Storage | JSON/JSONL files | Human-readable, easy to debug |
| Sandbox | Seatbelt/Landlock | OS-native isolation |
| Config | TOML | Human-friendly, well-supported |

---

## Dependencies

### Rust Crates (Cargo.toml)

```toml
[package]
name = "gsh-daemon"
version = "0.1.0"
edition = "2021"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"

# HTTP client for LLM APIs
reqwest = { version = "0.12", features = ["json", "stream"] }

# Python embedding
pyo3 = { version = "0.22", features = ["auto-initialize"] }

# CLI
clap = { version = "4", features = ["derive"] }

# Utilities
anyhow = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
glob = "0.3"
dirs = "5"

# Logging
tracing = "0.1"
tracing-subscriber = "0.3"
```

### System Dependencies

- **tmux** >= 3.0
- **zsh** >= 5.0
- **Python** >= 3.10 (for PyO3)
- **jq** (for zsh plugin JSON handling)
- **fzf** (optional, for agent selection)
- **ripgrep** (for grep tool)

---

## Research Sources

This architecture is informed by research into:

- [Claude Code](https://code.claude.com/docs/en/overview) - Agentic loop, tools, subagents, hooks, sandboxing
- [LLMVM](https://github.com/9600dev/llmvm) - Python execution, continuation passing, helper functions
- [OpenCode](https://github.com/anomalyco/opencode) - Provider abstraction, session management, event bus
- [OpenAI Codex CLI](https://developers.openai.com/codex/cli/) - Sandboxing, approval policies
- [Gemini CLI](https://developers.google.com/gemini-code-assist/docs/gemini-cli) - ReAct loop, MCP support
- [Charmbracelet Crush](https://github.com/charmbracelet/crush) - Multi-model, LSP integration, skills
- [Mistral Vibe](https://github.com/mistralai/mistral-vibe) - Stateful terminal, todo feature
- [Zsh Documentation](https://zsh.sourceforge.io/Doc/) - Hooks, ZLE, completions

---

## Next Steps

1. Set up Rust project structure with Cargo workspace
2. Implement basic zsh plugin with hooks
3. Create daemon skeleton with Unix socket server
4. Implement context accumulator
5. Add Anthropic provider
6. Implement basic tools
7. Test end-to-end flow
