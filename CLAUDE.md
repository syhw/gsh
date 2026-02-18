# gsh - Agentic Shell

A zsh plugin + Rust daemon for an agentic coding CLI. The daemon captures shell context and provides LLM-powered assistance with multi-agent orchestration, flow-based execution, MCP integration, and tmux-based subagent isolation.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              gsh-daemon (gshd)                              │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐│
│  │   Session   │  │    Agent    │  │    Flow     │  │      Provider       ││
│  │   Manager   │  │   Manager   │  │   Engine    │  │      Registry       ││
│  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────────────┘│
│         │                │                │                    │            │
│         ▼                ▼                ▼                    ▼            │
│  ┌──────────────────┐  ┌──────────────────────────────────────────────────┐ │
│  │   MCP Clients    │  │              Tmux Session Manager               │ │
│  │  (external tools)│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐        │ │
│  └──────────────────┘  │  │ agent-1  │ │ agent-2  │ │ agent-N  │        │ │
│                        │  │ (hidden) │ │ (hidden) │ │ (hidden) │        │ │
│                        │  └──────────┘ └──────────┘ └──────────┘        │ │
│                        └──────────────────────────────────────────────────┘ │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │     Observability (JSONL logging, cost tracking, TUI dashboard)        ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │     Context Engineering (truncate → prune → compact, JIT retrieval)    ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
     │          │          │          │          │          │          │
     ▼          ▼          ▼          ▼          ▼          ▼          ▼
 Anthropic   OpenAI    Z.ai/Zhipu  Together  Moonshot   Mistral   Cerebras
 (Claude)   (GPT/o3/   (GLM-5)    (Kimi K2  (Kimi)    (Devstral) (Llama)
             codex)                 Qwen)
```

## Project Structure

```
gsh/
├── Cargo.toml                    # Workspace root (members: gsh-daemon, gsh-cli)
├── config.example.toml           # Example configuration
├── gsh.plugin.zsh                # Zsh plugin (shell hooks + helper functions)
├── install.sh                    # One-command install script with shell integration
├── examples/flows/               # Example flow definitions
├── gsh-daemon/                   # Daemon crate
│   └── src/
│       ├── lib.rs                # Module re-exports
│       ├── main.rs               # Socket server, message routing, daemonization
│       ├── config.rs             # TOML configuration (all provider configs)
│       ├── protocol.rs           # JSON message types (ShellMessage, DaemonMessage)
│       ├── state.rs              # Shared daemon state (context, sessions, observer)
│       ├── context/              # Shell context management
│       │   ├── accumulator.rs    # Command history, cwd tracking, persistent JSONL history
│       │   └── retriever.rs      # JIT context retrieval (search_context tool backend)
│       ├── provider/             # LLM API clients
│       │   ├── mod.rs            # Provider trait, factory, error types
│       │   ├── anthropic.rs      # Claude API (native SSE streaming)
│       │   ├── openai.rs         # OpenAI Chat Completions (also Mistral, Cerebras)
│       │   ├── openai_responses.rs # OpenAI Responses API (codex, gpt-5)
│       │   ├── zhipu.rs          # Z.ai / Zhipu GLM-5 (default provider)
│       │   ├── together.rs       # Together.AI (Kimi K2.5, Qwen3-Coder)
│       │   ├── moonshot.rs       # Moonshot AI (Kimi, 200K context)
│       │   └── ollama.rs         # Local models via Ollama
│       ├── agent/                # Agentic loop + tool execution
│       │   ├── mod.rs            # Agent struct, event loop, context engineering
│       │   └── tools.rs          # Built-in tools + search_context
│       ├── session/              # Chat session persistence (JSONL storage)
│       │   └── mod.rs            # Session create/load/list/delete/cleanup
│       ├── flow/                 # Flow-based orchestration
│       │   ├── mod.rs            # Flow, AgentNode, NextNode, Coordination types
│       │   ├── engine.rs         # Flow execution engine with event emission
│       │   ├── parser.rs         # TOML flow parser
│       │   ├── roles.rs          # Role registry (built-in + loadable from disk)
│       │   ├── memory.rs         # Per-node memory store (remember/recall)
│       │   ├── publication.rs    # Publication store for consensus
│       │   └── pub_tools.rs      # Publication tools (publish, review, cite)
│       ├── mcp/                  # Model Context Protocol client
│       │   ├── client.rs         # JSON-RPC 2.0 over streamable-HTTP
│       │   └── handler.rs        # McpToolHandler (CustomToolHandler impl)
│       ├── tmux/                 # Tmux session management
│       │   └── manager.rs        # Spawn, attach, cleanup subagent sessions
│       └── observability/        # Logging, cost tracking, dashboard
│           ├── mod.rs            # Observer coordinator
│           ├── events.rs         # EventKind enum, ObservabilityEvent
│           ├── logger.rs         # JSONL file logger with daily rotation
│           ├── cost.rs           # Token cost tracking with model pricing
│           └── dashboard.rs      # TUI dashboard (ratatui + crossterm)
└── gsh-cli/                      # CLI client crate
    └── src/main.rs               # Client with spinner, stats, env detection
```

## Build & Run

```bash
# One-command install (builds, installs, configures shell)
./install.sh

# Or manual build
cargo build --release

# Start daemon (daemonizes by default)
gsh-daemon start
# Or foreground for development
gsh-daemon start --foreground

# Use CLI
gsh what files are here
gsh --provider anthropic --model claude-sonnet-4-20250514 "explain this error"
gsh --flow code-review "Review my recent changes"

# Interactive chat (with optional session resume)
gsh chat
gsh chat --session abc123

# Manage sessions
gsh sessions
gsh sessions --delete abc123

# Subagent management
gsh agents
gsh attach <agent-id>
gsh logs <agent-id> --follow
gsh kill <agent-id>

# Observability dashboard
gsh-daemon dashboard
```

## Configuration

Config file: `~/.config/gsh/config.toml`

Required: Set API key for at least one provider via environment variable or config file.

Default provider: `zai` (Z.ai GLM-5). Override with `--provider` flag or `[llm] default_provider` in config.

### MCP Servers

External tool servers are configured under `[mcp.servers]`:
```toml
[mcp.servers.web-search]
type = "streamable-http"
url = "https://api.z.ai/api/mcp/web_search_prime/mcp"
headers = { Authorization = "Bearer $ZAI_API_KEY" }
```
Header values support `$ENV_VAR` expansion. Tools from MCP servers are automatically available to agents.

## Key Components

### Protocol (`gsh-daemon/src/protocol.rs`)
- `ShellMessage`: Messages from shell/CLI to daemon (Preexec, Postcmd, Chpwd, Prompt, ChatStart/Message/End, ListAgents, KillAgent, ListSessions, DeleteSession, Ping, Shutdown)
- `DaemonMessage`: Responses from daemon (Ack, TextChunk, ToolUse, ToolResult, AgentList, Error, FlowEvent, SessionList, Compacted, Thinking, etc.)
- `EnvInfo`: Client environment (conda, venv, PATH) forwarded to daemon

### Context Engineering (`gsh-daemon/src/context/`, `gsh-daemon/src/agent/mod.rs`)
3-layer context management system (see `CONTEXT_ENGINEERING.md` for full details):
1. **Truncate**: Per tool result, caps at `max_tool_output_bytes` (30KB default), prefix+suffix strategy
2. **Prune**: Every iteration, replaces old tool outputs beyond `prune_protect_tokens` (40K tokens)
3. **Compact**: At `compact_threshold` (85% of context window), LLM-based summarization of history

JIT context retrieval:
- `ambient_commands` (default 5): Only last N shell commands in system prompt
- `search_context` tool: On-demand search of shell_history, sessions, and logs
- Persistent shell history: `~/.local/share/gsh/shell-history.jsonl`

### Provider (`gsh-daemon/src/provider/`)
- `Provider` trait with `chat()`, `chat_with_usage()`, and `chat_stream()` methods
- `AnthropicProvider`: Claude API with native SSE streaming
- `OpenAIProvider`: OpenAI Chat Completions (also used for Mistral, Cerebras via base URL override)
- `OpenAIResponsesProvider`: OpenAI Responses API for codex/gpt-5 models (auto-detected)
- `ZhipuProvider`: Z.ai / Zhipu GLM-5 (default provider)
- `TogetherProvider`: Together.AI (Kimi K2.5, Qwen3-Coder)
- `MoonshotProvider`: Moonshot AI / Kimi (200K context)
- `OllamaProvider`: Local models via Ollama

### Agent (`gsh-daemon/src/agent/`)
- Agentic loop: prompt → LLM → tool calls → execute → loop
- Built-in tools: bash, read, write, edit, glob, grep, search_context
- Extensible via `CustomToolHandler` trait (publication tools, MCP tools)
- Context engineering: automatic truncation, pruning, and compaction
- Python environment detection (conda, venv, pyenv) injected into system prompt
- Streams `AgentEvent`s back to client (TextChunk, ToolStart, ToolResult, Compacted, Thinking)

### Flow Engine (`gsh-daemon/src/flow/`)
- TOML-based flow definitions (loaded from `.gsh/flows/` or `~/.config/gsh/flows/`)
- Sequential, conditional, and parallel execution
- Publication/review coordination mode (consensus aggregation)
- Role system: built-in roles (general, code-reviewer, planner, coder) + loadable from `.gsh/roles/`
- Per-node memory store (remember/recall within a flow)
- Each node can specify role, tools, model/provider override

### MCP (`gsh-daemon/src/mcp/`)
- JSON-RPC 2.0 client over streamable-HTTP transport
- Automatic tool discovery via `tools/list`
- Tool invocation via `tools/call`, exposed as `CustomToolHandler`
- Supports `mcp-session-id` header tracking
- Non-fatal connection failures (server unavailable doesn't block startup)

### Session Manager (`gsh-daemon/src/session/`)
- JSONL-based session persistence in `~/.local/share/gsh/sessions/`
- Create, load, resume, list, delete sessions
- Automatic cleanup: max_sessions (100), max_age_days (30)

### Tmux Manager (`gsh-daemon/src/tmux/`)
- Spawns subagent sessions: `gsh-agent-NNNN`
- Attach/detach for debugging (switch-client if already in tmux)
- Automatic cleanup on daemon shutdown

### Observability (`gsh-daemon/src/observability/`)
- JSONL structured logging with daily rotation
- Token usage and cost tracking per session and per flow (with model pricing table)
- TUI dashboard (ratatui) for monitoring events and usage
- Event types: Start, Iteration, Prompt, Text, ToolCall, ToolResult, BashExec, Usage, Complete, Error, Compaction, Truncation, Spawn

### CLI (`gsh-cli/src/main.rs`)
- Thin client connecting to daemon via Unix socket
- Commands: `gsh <prompt>`, `gsh chat`, `gsh sessions`, `gsh agents`, `gsh attach`, `gsh logs`, `gsh kill`, `gsh status`, `gsh stop`
- Flow execution: `gsh --flow <name> <prompt>`
- Provider/model override: `gsh --provider openai --model gpt-4o <prompt>`
- Stdin support: `gsh - < file.txt` or piped input
- Braille spinner animation during inference
- Generation stats: TTFT, tokens/sec printed on completion
- Python env forwarding (conda, venv, PATH) to daemon

## Common Development Tasks

### Adding a new tool
1. Add tool definition in `gsh-daemon/src/agent/tools.rs` → `definitions()`
2. Add execution handler in `execute()` match arm
3. Implement the `exec_<toolname>()` async method

### Adding a new LLM provider
1. Create `gsh-daemon/src/provider/<name>.rs`
2. Implement `Provider` trait (`chat`, `chat_with_usage`, `chat_stream`)
3. Add to `create_provider()` / `create_provider_with_model()` in `gsh-daemon/src/provider/mod.rs`
4. Add config structs in `gsh-daemon/src/config.rs`
5. For OpenAI-compatible APIs, reuse `OpenAIProvider` with a custom base URL

### Adding a new flow
1. Create TOML file in `~/.config/gsh/flows/<name>.toml`
2. Define nodes with roles, tools, model preferences
3. Define edges for routing (sequential, conditional, parallel)
4. Test with `gsh --flow <name> "<prompt>"`

### Adding custom tool handlers
1. Implement `CustomToolHandler` trait in your module
2. Add handler to agent via `agent.with_custom_handler(Arc::new(handler))`
3. Handler's tools will be available alongside built-in tools

### Adding an MCP server
1. Add config under `[mcp.servers.<name>]` in `~/.config/gsh/config.toml`
2. Set `type = "streamable-http"`, `url`, and optional `headers`
3. Use `$ENV_VAR` syntax in header values for API keys
4. Tools are automatically discovered and exposed to agents

## Testing

```bash
cargo test                    # Run all tests
cargo test -p gsh-daemon      # Daemon tests only
```

## Socket Protocol

JSON-over-Unix-socket, newline-delimited. Each message is a single JSON object followed by `\n`.

Example exchange:
```json
→ {"type":"prompt","query":"list files","cwd":"/home/user","session_id":null,"stream":true}
← {"type":"thinking"}
← {"type":"text_chunk","text":"Here are","done":false}
← {"type":"tool_use","tool":"bash","input":{"command":"ls -la"}}
← {"type":"tool_result","tool":"bash","output":"...","success":true}
← {"type":"text_chunk","text":"\n\nDone!","done":true}
```

## Environment Variables

- `ANTHROPIC_API_KEY` - Anthropic API key
- `OPENAI_API_KEY` - OpenAI API key
- `ZAI_API_KEY` - Z.ai / Zhipu AI API key
- `TOGETHER_API_KEY` - Together.AI API key
- `MOONSHOT_API_KEY` - Moonshot AI (Kimi) API key
- `MISTRAL_API_KEY` - Mistral API key
- `CEREBRAS_API_KEY` - Cerebras API key
- `GSH_SOCKET` - Override socket path (default: `/tmp/gsh-$USER.sock`)
- `GSH_ENABLED` - Set to `0` to disable context tracking in shell
- `GSH_QUIET` - Set to `1` to suppress plugin load message
- `GSH_VERBOSITY` - CLI output verbosity: `none`, `progress` (default), `debug`

## Data Directories

- `~/.config/gsh/` - Configuration (config.toml, flows/, roles/)
- `~/.local/share/gsh/` - Data (sessions/, shell-history.jsonl, daemon.log)
- `~/.local/share/gsh/observability/` - JSONL event logs

## Current Work

See `TODO.claude` for active development tasks and known issues.
