# gsh - Agentic Shell

An AI-powered shell assistant that understands your terminal context. gsh captures your shell activity (commands, outputs, directory changes) and uses it to provide contextual help via LLM.

## Features

- **Context-aware**: Knows your recent commands, working directory, and shell history
- **Tool use**: Can read/write files, run commands, search with glob/grep
- **Streaming**: Real-time response streaming
- **Multiple providers**: Anthropic Claude, OpenAI, or any OpenAI-compatible API
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
