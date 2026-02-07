# gsh - Agentic Shell

An AI-powered shell assistant that understands your terminal context. gsh captures your shell activity (commands, outputs, directory changes) and uses it to provide contextual help via LLM. Supports multi-agent flows with tmux-based isolation.

## Features

- **Context-aware**: Knows your recent commands, working directory, and shell history
- **Tool use**: Can read/write files, run commands, search with glob/grep
- **Streaming**: Real-time response streaming
- **Multiple providers**: Anthropic Claude, OpenAI, Zhipu GLM-4, Together.AI (Kimi K2), Ollama
- **Flow orchestration**: Define multi-agent workflows in TOML with sequential, conditional, and parallel execution
- **Tmux integration**: Subagents run in attachable tmux sessions for debugging
- **Observability**: JSONL logging and TUI dashboard for monitoring
- **No quotes needed**: `llm what files are here` just works

## Installation

### Prerequisites

- Rust toolchain (1.70+): https://rustup.rs
- Zsh shell
- `socat` (for shell hooks): `brew install socat` or `apt install socat`

### Build from source

```bash
git clone <repo-url> gsh
cd gsh

# Build release binaries
cargo build --release

# Optional: Install to PATH
cargo install --path gsh-daemon
cargo install --path gsh-cli
```

### Setup

1. **Copy example config**:
   ```bash
   mkdir -p ~/.config/gsh
   cp config.example.toml ~/.config/gsh/config.toml
   ```

2. **Add to your `.zshrc`**:
   ```bash
   # Source the plugin (adjust path as needed)
   source /path/to/gsh/gsh.plugin.zsh

   # Or if installed to PATH, just set the bins:
   # export GSH_DAEMON_BIN="gsh-daemon"
   # export GSH_CLI_BIN="gsh"
   # source /path/to/gsh/gsh.plugin.zsh
   ```

3. **Set your API key** (choose one):
   ```bash
   # Anthropic (Claude)
   export ANTHROPIC_API_KEY="sk-ant-..."

   # OpenAI
   export OPENAI_API_KEY="sk-..."
   ```

## Usage

### Start the daemon

```bash
# In a terminal (or add to your shell startup)
gsh-daemon start --foreground

# Or use the helper function after sourcing the plugin
gsh-start
```

### Send prompts

```bash
# No quotes needed - all arguments are joined
llm what files are in this directory
llm explain the error in my last command
llm write a python script that parses json

# Read from stdin
echo "explain this" | llm
cat error.log | llm what went wrong

# Read from file
llm - < prompt.txt

# Interactive chat
llm chat
```

### Other commands

```bash
llm status          # Check daemon status
llm stop            # Stop the daemon
gsh-restart         # Restart the daemon
gsh-disable         # Disable context tracking
gsh-enable          # Re-enable context tracking
```

### Provider and model selection

```bash
# Use specific provider
llm --provider openai "explain this code"
llm --provider anthropic "write a test"

# Use specific model
llm --model gpt-4o "complex task"
llm --model claude-sonnet-4-20250514 "review code"

# Combine both
llm --provider zhipu --model glm-4 "translate to Chinese"
```

### Flows (multi-agent orchestration)

```bash
# Run a predefined flow
llm --flow code-review "Review my recent changes"
llm --flow debug "Fix the failing tests"

# List available flows
ls ~/.config/gsh/flows/
```

### Subagent management

```bash
llm agents           # List running subagents
llm attach <id>      # Attach to subagent's tmux session
llm kill <id>        # Stop a subagent
```

### Observability dashboard

```bash
gsh-daemon dashboard  # Open TUI dashboard
```

The TUI dashboard has three tabs (switch with `1`/`2`/`3`):

- **Tab 1 - Events**: Live feed of all agent events (prompts, tool calls, results, errors). Auto-scrolls to latest. Use arrow keys to scroll manually, `a` to toggle auto-scroll.
- **Tab 2 - Usage**: Token usage breakdown by provider and model. Shows input/output/cache tokens, estimated cost in USD, and per-model breakdown.
- **Tab 3 - Files**: Available JSONL log files. Select with arrow keys and Enter to load a different session.

Press `q` to quit.

## Defining Flows

Flows are TOML files that define multi-agent workflows. Place them in `~/.config/gsh/flows/`.

### Example: Simple sequential flow

```toml
# ~/.config/gsh/flows/code-review.toml
[flow]
name = "code-review"
description = "Review code changes"
entry = "analyzer"

[nodes.analyzer]
name = "Code Analyzer"
prompt = "Analyze the code and identify issues"
tools = ["read", "glob", "grep"]
next = "reviewer"

[nodes.reviewer]
name = "Reviewer"
prompt = "Review the analysis and provide recommendations"
tools = ["read"]
next = "end"
```

### Example: Parallel execution

```toml
[flow]
name = "multi-review"
entry = "start"

[nodes.start]
name = "Dispatcher"
prompt = "Identify files to review"
tools = ["glob"]
next = { parallel = ["security", "style"] }

[nodes.security]
name = "Security Review"
prompt = "Check for security vulnerabilities"
tools = ["read", "grep"]

[nodes.style]
name = "Style Review"
prompt = "Check code style and best practices"
tools = ["read", "grep"]

[nodes.aggregator]
name = "Summary"
prompt = "Combine all review findings"
join_from = ["security", "style"]
next = "end"
```

### Example: Publication/review coordination flow

Nodes can coordinate via a shared publication store where agents publish findings and review each other's work. Consensus is reached when a publication receives enough approvals.

```toml
# ~/.config/gsh/flows/peer-review.toml
[flow]
name = "peer-review"
description = "Multiple agents review code independently and reach consensus"
entry = "analyst-a"

[flow.coordination]
mode = "publication"
consensus_threshold = 2     # Reviews needed for consensus
allow_self_review = false   # Agents cannot review their own publications

[nodes.analyst-a]
name = "Analyst A"
prompt = "Analyze the code for bugs and publish your findings"
tools = ["read", "glob", "grep", "publish", "search_publications", "review_publication"]
next = "analyst-b"

[nodes.analyst-b]
name = "Analyst B"
prompt = "Analyze the code independently, then review Analyst A's publications"
tools = ["read", "glob", "grep", "publish", "search_publications", "review_publication"]
review_from = ["analyst-a"]   # Inject Analyst A's publications for review
next = "aggregator"

[nodes.aggregator]
name = "Aggregator"
prompt = "Summarize all findings that reached consensus"
tools = ["search_publications"]
next = "end"
```

Publication tools available to agents in coordination mode:
- `publish` - Publish a finding with title, content, and tags
- `search_publications` - Search publications by tag or keyword
- `review_publication` - Review a publication with approve/reject and comments

## Using with OpenAI-Compatible APIs

gsh works with any OpenAI-compatible API (Ollama, vLLM, LM Studio, LocalAI, etc.).

### Example: Local LLM with Ollama

1. **Start Ollama** with your model:
   ```bash
   ollama run llama3.1
   ```

2. **Configure gsh** (`~/.config/gsh/config.toml`):
   ```toml
   [llm]
   default_provider = "openai"

   [llm.openai]
   api_key = "ollama"  # Ollama doesn't need a real key
   model = "llama3.1"
   base_url = "http://localhost:11434/v1/chat/completions"
   ```

3. **Test it**:
   ```bash
   gsh-restart
   llm hello world
   ```

### Example: vLLM or other OpenAI-compatible server

```toml
[llm]
default_provider = "openai"

[llm.openai]
api_key = "your-api-key"  # Or "none" if not required
model = "meta-llama/Llama-3.1-8B-Instruct"
base_url = "http://localhost:8000/v1/chat/completions"
```

### Example: Azure OpenAI

```toml
[llm]
default_provider = "openai"

[llm.openai]
api_key = "your-azure-api-key"
model = "gpt-4o"
base_url = "https://your-resource.openai.azure.com/openai/deployments/your-deployment/chat/completions?api-version=2024-02-15-preview"
```

### Example: Zhipu GLM-4 (Chinese language optimized)

```toml
[llm]
default_provider = "zhipu"

[llm.zhipu]
api_key = "your-zhipu-api-key"  # Or use ZAI_API_KEY env var
model = "glm-4"
```

### Example: Together.AI (Kimi K2, Qwen)

```toml
[llm]
default_provider = "together"

[llm.together]
api_key = "your-together-api-key"  # Or use TOGETHER_API_KEY env var
model = "Qwen/Qwen3-Coder-235B-A22B-Instruct-FP8"
```

## Testing Your Setup

### 1. Check daemon is running

```bash
llm status
# Should show: Daemon running, Version, Uptime, Socket path
```

### 2. Test basic prompt

```bash
llm say hello
# Should get a response from the LLM
```

### 3. Test tool use

```bash
llm list the files in the current directory using the bash tool
# Should see [Using tool: bash] and file listing
```

### 4. Test context awareness

```bash
# Run some commands first
ls -la
pwd
git status

# Then ask about them
llm what commands did I just run
# Should reference your recent commands
```

### 5. Test streaming

```bash
llm write a long poem about programming
# Should see text appear word-by-word
```

## Configuration Reference

Full config file (`~/.config/gsh/config.toml`):

```toml
[daemon]
# socket_path = "/tmp/gsh.sock"  # Default: /tmp/gsh-$USER.sock
log_level = "info"               # trace, debug, info, warn, error
# log_file = "~/.local/share/gsh/daemon.log"

[llm]
default_provider = "anthropic"   # anthropic, openai
max_tokens = 4096
# system_prompt = "Custom system prompt..."

[llm.anthropic]
# api_key = "sk-ant-..."         # Or use ANTHROPIC_API_KEY env var
model = "claude-sonnet-4-20250514"

[llm.openai]
# api_key = "sk-..."             # Or use OPENAI_API_KEY env var
model = "gpt-4o"
# base_url = "https://api.openai.com/v1/chat/completions"

[llm.zhipu]
# api_key = "..."                # Or use ZAI_API_KEY env var
model = "glm-4"

[llm.together]
# api_key = "..."                # Or use TOGETHER_API_KEY env var
model = "Qwen/Qwen3-Coder-235B-A22B-Instruct-FP8"

[llm.ollama]
model = "llama3.2"
base_url = "http://localhost:11434"

[context]
max_events = 100                 # Shell events to keep
max_context_chars = 50000        # Max context length
include_outputs = true

[tools]
bash_enabled = true
read_enabled = true
write_enabled = true
edit_enabled = true
glob_enabled = true
grep_enabled = true
excluded_paths = [".git/objects", "node_modules", ".venv"]
max_file_size = 1048576          # 1MB

[observability]
enabled = true
log_dir = "~/.local/share/gsh/logs"  # JSONL logs location
```

## Troubleshooting

### "Failed to connect to daemon"
The daemon isn't running. Start it with:
```bash
gsh-daemon start --foreground
```

### "API key not configured"
Set your API key:
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
# or
export OPENAI_API_KEY="sk-..."
```

### Daemon crashes or no response
Check logs (if configured) or run in foreground to see errors:
```bash
gsh-daemon start --foreground
```

### Shell hooks not working
Make sure `socat` is installed:
```bash
brew install socat  # macOS
apt install socat   # Linux
```

## License

MIT
