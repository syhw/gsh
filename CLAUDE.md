# gsh - Agentic Shell

A zsh plugin + Rust daemon for an agentic coding CLI. The daemon captures shell context and provides LLM-powered assistance with multi-agent orchestration, flow-based execution, and tmux-based subagent isolation.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                              gsh-daemon (gshd)                              │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   Session   │  │    Agent    │  │    Flow     │  │      Provider       │ │
│  │   Manager   │  │   Manager   │  │   Engine    │  │      Registry       │ │
│  └─────────────┘  └─────────────┘  └─────────────┘  └─────────────────────┘ │
│         │                │                │                    │            │
│         ▼                ▼                ▼                    ▼            │
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │                         Tmux Session Manager                            ││
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐               ││
│  │  │ gsh-root │  │ agent-1  │  │ agent-2  │  │ agent-N  │  (attachable) ││
│  │  │ (user UI)│  │ (hidden) │  │ (hidden) │  │ (hidden) │               ││
│  │  └──────────┘  └──────────┘  └──────────┘  └──────────┘               ││
│  └─────────────────────────────────────────────────────────────────────────┘│
│  ┌─────────────────────────────────────────────────────────────────────────┐│
│  │                         Observability (JSONL)                           ││
│  └─────────────────────────────────────────────────────────────────────────┘│
└─────────────────────────────────────────────────────────────────────────────┘
           │                  │                  │                  │
           ▼                  ▼                  ▼                  ▼
    ┌────────────┐     ┌────────────┐     ┌────────────┐     ┌────────────┐
    │  Anthropic │     │   OpenAI   │     │   Zhipu    │     │  Together  │
    │  (Claude)  │     │  (GPT/o3)  │     │  (GLM-4)   │     │  (Kimi K2) │
    └────────────┘     └────────────┘     └────────────┘     └────────────┘
```

## Project Structure

```
gsh_claude/
├── Cargo.toml                    # Workspace root
├── config.example.toml           # Example configuration
├── gsh.plugin.zsh                # Zsh plugin (shell hooks)
├── examples/flows/               # Example flow definitions
├── gsh-daemon/                   # Daemon crate
│   └── src/
│       ├── main.rs               # Socket server, message routing
│       ├── config.rs             # TOML configuration
│       ├── protocol.rs           # JSON message types
│       ├── state.rs              # Shared daemon state
│       ├── context/              # Shell event accumulation
│       │   └── accumulator.rs    # Command history, cwd tracking
│       ├── provider/             # LLM API clients
│       │   ├── mod.rs            # Provider trait, factory
│       │   ├── anthropic.rs      # Claude API
│       │   ├── openai.rs         # OpenAI/compatible APIs
│       │   ├── zhipu.rs          # GLM-4 (Zhipu AI)
│       │   ├── together.rs       # Together.AI (Kimi K2, Qwen)
│       │   └── ollama.rs         # Local models via Ollama
│       ├── agent/                # Agentic loop + tool execution
│       │   ├── mod.rs            # Agent struct, event loop
│       │   └── tools.rs          # Tool definitions & execution
│       ├── session/              # Chat session management
│       ├── flow/                 # Flow-based orchestration
│       │   ├── mod.rs            # Flow, Node, Edge types
│       │   ├── engine.rs         # Flow execution engine
│       │   ├── parser.rs         # TOML flow parser
│       │   ├── publication.rs    # Publication store for consensus
│       │   └── pub_tools.rs      # Publication tools (publish, review, cite)
│       ├── tmux/                 # Tmux session management
│       │   └── manager.rs        # Spawn, attach, cleanup sessions
│       └── observability/        # Logging and metrics
│           └── mod.rs            # JSONL logging, dashboard
└── gsh-cli/                      # CLI client crate
    └── src/main.rs               # Thin client for daemon communication
```

## Build & Run

```bash
# Build
cargo build --release

# Start daemon (in one terminal)
./target/release/gsh-daemon start --foreground

# Use CLI (in another terminal)
gsh what files are here

# Run a flow
gsh --flow code-review "Review my recent changes"

# Open observability dashboard
./target/release/gsh-daemon dashboard
```

## Configuration

Config file: `~/.config/gsh/config.toml`

Required: Set API key for at least one provider via environment variable or config file.

## Key Components

### Protocol (`gsh-daemon/src/protocol.rs`)
- `ShellMessage`: Messages from shell/CLI to daemon (Preexec, Postcmd, Chpwd, Prompt, ListAgents, KillAgent, etc.)
- `DaemonMessage`: Responses from daemon (Ack, TextChunk, ToolUse, ToolResult, AgentList, Error)

### Context Accumulator (`gsh-daemon/src/context/accumulator.rs`)
- Tracks shell commands, exit codes, durations, directory changes
- Generates context string for LLM system prompt
- Circular buffer with configurable max events

### Provider (`gsh-daemon/src/provider/`)
- `Provider` trait with `chat()` and `chat_stream()` methods
- `AnthropicProvider`: Claude API with streaming SSE parsing
- `OpenAIProvider`: OpenAI/compatible APIs with reasoning support
- `ZhipuProvider`: GLM-4 models (Chinese language optimized)
- `TogetherProvider`: Together.AI (Kimi K2.5, Qwen3-Coder)
- `OllamaProvider`: Local models

### Agent (`gsh-daemon/src/agent/`)
- Agentic loop: prompt → LLM → tool calls → execute → loop
- Built-in tools: bash, read, write, edit, glob, grep
- Extensible via `CustomToolHandler` trait (e.g., publication tools)
- Streams events back to client during execution
- Observability integration for logging and metrics

### Flow Engine (`gsh-daemon/src/flow/`)
- TOML-based flow definitions
- Sequential, conditional, and parallel execution
- Publication/review coordination mode (schrd-style consensus)
- Each node can specify role, tools, model override

### Tmux Manager (`gsh-daemon/src/tmux/`)
- Spawns subagent sessions: `gsh-agent-NNNN`
- Attach/detach for debugging
- Automatic cleanup on daemon shutdown

### Observability (`gsh-daemon/src/observability/`)
- JSONL structured logging of all events
- Token usage tracking per provider/model
- TUI dashboard for monitoring flows

### CLI (`gsh-cli/src/main.rs`)
- Thin client that connects to daemon socket
- Supports: `gsh <prompt>`, `gsh chat`, `gsh status`, `gsh stop`
- Flow execution: `gsh --flow <name> <prompt>`
- Provider/model override: `gsh --provider openai --model gpt-4o <prompt>`

## Common Development Tasks

### Adding a new tool
1. Add tool definition in `gsh-daemon/src/agent/tools.rs` → `definitions()`
2. Add execution handler in `execute()` match arm
3. Implement the `exec_<toolname>()` async method

### Adding a new LLM provider
1. Create `gsh-daemon/src/provider/<name>.rs`
2. Implement `Provider` trait (both `chat` and `chat_stream`)
3. Add to `create_provider()` in `gsh-daemon/src/provider/mod.rs`
4. Add config structs in `gsh-daemon/src/config.rs`

### Adding a new flow
1. Create TOML file in `~/.config/gsh/flows/<name>.toml`
2. Define nodes with roles, tools, model preferences
3. Define edges for routing (sequential, conditional, parallel)
4. Test with `gsh --flow <name> "<prompt>"`

### Adding custom tool handlers
1. Implement `CustomToolHandler` trait in your module
2. Add handler to agent via `agent.with_custom_handler(Arc::new(handler))`
3. Handler's tools will be available alongside built-in tools

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
← {"type":"text_chunk","text":"Here are","done":false}
← {"type":"tool_use","tool":"bash","input":{"command":"ls -la"}}
← {"type":"tool_result","tool":"bash","output":"...","success":true}
← {"type":"text_chunk","text":"\n\nDone!","done":true}
```

## Environment Variables

- `ANTHROPIC_API_KEY` - Anthropic API key
- `OPENAI_API_KEY` - OpenAI API key
- `ZAI_API_KEY` - Zhipu AI API key
- `TOGETHER_API_KEY` - Together.AI API key
- `GSH_SOCKET` - Override socket path (default: `/tmp/gsh-$USER.sock`)
- `GSH_ENABLED` - Set to `0` to disable context tracking in shell
- `GSH_QUIET` - Set to `1` to suppress plugin load message

## Current Work

See `TODO.claude` for active development tasks and known issues.
