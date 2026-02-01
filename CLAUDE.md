# gsh - Agentic Shell

A zsh plugin + Rust daemon for an agentic coding CLI. The daemon captures shell context and provides LLM-powered assistance with tool use.

## Architecture

```
┌─────────────────┐     Unix Socket      ┌──────────────────┐
│   Zsh Plugin    │ ──────────────────▶  │   gsh-daemon     │
│ (gsh.plugin.zsh)│                      │                  │
└─────────────────┘                      │  ┌────────────┐  │
        │                                │  │  Context   │  │
        │ preexec/precmd/chpwd hooks     │  │ Accumulator│  │
        ▼                                │  └────────────┘  │
┌─────────────────┐                      │  ┌────────────┐  │
│    gsh-cli      │ ──────────────────▶  │  │  Provider  │──┼──▶ Anthropic/OpenAI
│  (gsh binary)   │     Prompt/Chat      │  │  (LLM API) │  │
└─────────────────┘                      │  └────────────┘  │
                                         │  ┌────────────┐  │
                                         │  │   Agent    │  │
                                         │  │  + Tools   │  │
                                         │  └────────────┘  │
                                         └──────────────────┘
```

## Project Structure

```
gsh_claude/
├── Cargo.toml              # Workspace root
├── config.example.toml     # Example configuration
├── gsh.plugin.zsh          # Zsh plugin (hooks, llm command)
├── gsh-daemon/             # Daemon crate
│   └── src/
│       ├── main.rs         # Socket server, message routing
│       ├── config.rs       # TOML configuration
│       ├── protocol.rs     # JSON message types
│       ├── state.rs        # Shared daemon state
│       ├── context/        # Shell event accumulation
│       ├── provider/       # LLM API clients (Anthropic, OpenAI)
│       ├── agent/          # Agentic loop + tool execution
│       ├── session/        # Chat session management
│       └── tmux/           # Tmux integration (future)
└── gsh-cli/                # CLI client crate
    └── src/main.rs         # Thin client for daemon communication
```

## Build & Run

```bash
# Build
cargo build --release

# Start daemon (in one terminal)
./target/release/gsh-daemon start --foreground

# Use CLI (in another terminal)
source gsh.plugin.zsh
llm what files are here
```

## Configuration

Config file: `~/.config/gsh/config.toml`

Required: Set `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` environment variable, or configure in the TOML file.

## Key Components

### Protocol (`gsh-daemon/src/protocol.rs`)
- `ShellMessage`: Messages from shell/CLI to daemon (Preexec, Postcmd, Chpwd, Prompt, Chat*)
- `DaemonMessage`: Responses from daemon (Ack, TextChunk, ToolUse, ToolResult, Error)

### Context Accumulator (`gsh-daemon/src/context/accumulator.rs`)
- Tracks shell commands, exit codes, durations, directory changes
- Generates context string for LLM system prompt
- Circular buffer with configurable max events

### Provider (`gsh-daemon/src/provider/`)
- `Provider` trait with `chat()` and `chat_stream()` methods
- `AnthropicProvider`: Claude API with streaming SSE parsing
- `OpenAIProvider`: OpenAI/compatible APIs

### Agent (`gsh-daemon/src/agent/`)
- Agentic loop: prompt → LLM → tool calls → execute → loop
- Tools: bash, read, write, edit, glob, grep
- Streams events back to client during execution

### CLI (`gsh-cli/src/main.rs`)
- Thin client that connects to daemon socket
- Supports: `gsh <prompt>`, `gsh chat`, `gsh status`, `gsh stop`
- Stdin support: `echo "prompt" | gsh` or `gsh - < file.txt`

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

### Adding a new shell message type
1. Add variant to `ShellMessage` in `gsh-daemon/src/protocol.rs`
2. Handle in `process_message()` in `gsh-daemon/src/main.rs`
3. Add corresponding `DaemonMessage` response if needed
4. Update CLI if client needs to send this message

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
← {"type":"text_chunk","text":" the files:","done":false}
← {"type":"tool_use","tool":"bash","input":{"command":"ls -la"}}
← {"type":"tool_result","tool":"bash","output":"...","success":true}
← {"type":"text_chunk","text":"\n\nDone!","done":true}
```

## Environment Variables

- `ANTHROPIC_API_KEY` - Anthropic API key
- `OPENAI_API_KEY` - OpenAI API key
- `GSH_SOCKET` - Override socket path (default: `/tmp/gsh-$USER.sock`)
- `GSH_ENABLED` - Set to `0` to disable context tracking in shell
- `GSH_QUIET` - Set to `1` to suppress plugin load message
