# gsh - Agentic Shell

An AI-powered shell assistant built on a simple idea: **log everything, build context automatically, let the LLM figure it out**.

gsh runs a background daemon that silently captures your shell activity — every command you run, every exit code, every directory change, with timestamps and durations. When you ask a question, all of that context is injected into the LLM prompt automatically. You don't have to explain what you were doing or paste error output. The LLM already knows.

## How It Works

```
 You type commands normally          gsh daemon silently records everything
 ──────────────────────────          ────────────────────────────────────────
 $ cd ~/project                  →   [14:02:01] cd: ~ -> ~/project
 $ cargo build                   →   [14:02:03] ~/project $ cargo build -> exit 1 (2340ms)
 $ cat src/main.rs               →   [14:02:08] ~/project $ cat src/main.rs -> OK (12ms)

 Then you ask:
 $ gsh why did my build fail

 The LLM sees your full shell history as context — it knows you're in ~/project,
 that cargo build failed with exit 1, and what you looked at after. No copy-paste needed.
```

The context accumulator is a circular buffer of shell events. It captures:

- **Commands**: what was run, where, exit code, duration in ms
- **Directory changes**: from/to paths with timestamps
- **Timing**: when things happened, how long they took

This context is formatted and injected into the system prompt on every LLM call. The LLM sees a chronological log of your recent shell activity and can reason about it.

## Features

- **Automatic context**: Shell hooks capture commands, exit codes, directory changes, durations
- **Configurable context window**: Control how many events and how many characters of context the LLM sees
- **Session persistence**: Every interaction is saved to disk and can be resumed after daemon restarts or reboots
- **Tool use**: The agent can read/write files, run commands, search with glob/grep
- **Environment detection**: Detects your active Python env (conda, micromamba, venv, pyenv) and tells the LLM to use it
- **Streaming**: Real-time response streaming
- **Multiple providers**: Anthropic Claude, OpenAI, Zhipu GLM-4, Together.AI (Kimi K2), Ollama
- **Flow orchestration**: Define multi-agent workflows in TOML
- **Tmux integration**: Subagents run in attachable tmux sessions
- **Observability**: JSONL logging of all events, TUI dashboard for monitoring
- **No quotes needed**: `gsh what files are here` just works

## Installation

### Prerequisites

- Rust toolchain (1.70+): https://rustup.rs
- Zsh shell
- `socat` (for shell hooks): `brew install socat` or `apt install socat`

### Build from source

```bash
git clone <repo-url> gsh
cd gsh
cargo build --release

# Optional: install to PATH
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
   source /path/to/gsh/gsh.plugin.zsh
   ```

3. **Set your API key** (at least one):
   ```bash
   export ANTHROPIC_API_KEY="sk-ant-..."
   # or
   export OPENAI_API_KEY="sk-..."
   ```

4. **Start the daemon**:
   ```bash
   gsh-daemon start --foreground   # or: gsh-start
   ```

## Context System (the core idea)

### What gets captured

The zsh plugin registers three hooks that fire automatically:

| Hook | Trigger | What it sends |
|------|---------|---------------|
| `preexec` | Before every command | Command text, cwd, timestamp |
| `precmd` | After every command | Exit code, cwd, duration in ms |
| `chpwd` | On directory change | Old path, new path, timestamp |

These are sent to the daemon over a Unix socket as fire-and-forget JSON messages. The daemon stores them in a circular buffer (the **context accumulator**).

### What the LLM sees

When you run `gsh <prompt>`, the daemon formats the accumulated events into a context block that gets prepended to the system prompt:

```
# Recent Shell Activity

[14:02:01] cd: ~ -> ~/project

[14:02:03] ~/project $ cargo build
  -> exit 1 (2340ms)

[14:02:08] ~/project $ cat src/main.rs
  -> OK (12ms)

[14:02:15] ~/project $ git diff
  -> OK (45ms)
```

The LLM can see exactly what you did, what failed, how long things took, and where you are. When you ask "why did my build fail?", it has all the context it needs.

### Configuring context

In `~/.config/gsh/config.toml`:

```toml
[context]
# How many shell events to keep in the circular buffer.
# More events = more history the LLM can see, but more tokens used.
max_events = 100

# Maximum characters of formatted context to inject into the system prompt.
# If the full history exceeds this, older events are dropped.
# Increase for complex debugging sessions, decrease to save tokens.
max_context_chars = 50000

# Include command outputs in context (when captured).
include_outputs = true
```

### Controlling context tracking

```bash
gsh-disable         # Stop sending shell events to daemon
gsh-enable          # Resume sending shell events

# Or set in your environment:
export GSH_ENABLED=0   # Disable for this shell session
```

This is useful when running sensitive commands you don't want logged, or when you want a "clean" context for a specific task.

## Usage

```bash
# Ask anything — your shell context is included automatically
gsh what files are in this directory
gsh explain the error in my last command
gsh why is this test failing

# Pipe content in
cat error.log | gsh what went wrong
git diff | gsh review these changes

# Read from file
gsh - < prompt.txt
```

### Sessions

Every `gsh` call creates a persisted session (JSONL file in `~/.local/share/gsh/sessions/`). Sessions survive daemon restarts and reboots.

```bash
# List saved sessions
gsh sessions

# Resume a session (full conversation history is sent to the LLM)
gsh chat --session a1b2c3d4
```

`gsh chat` starts an interactive REPL — a multi-turn conversation where context accumulates across messages within the same connection. Use it when you need a back-and-forth dialogue:

```bash
gsh chat                         # New interactive session
gsh chat --session a1b2c3d4      # Resume an existing one
```

> **Note**: `gsh chat` exists for the interactive REPL experience. One-shot `gsh <prompt>` calls also persist sessions — the only difference is that `chat` keeps the connection open for multiple turns.

### Provider and model selection

```bash
gsh -p openai explain this code
gsh -m gpt-4o review my last commit
gsh -p together -m moonshotai/Kimi-K2.5 what files are here
```

### Session management

```bash
gsh sessions                     # List saved sessions
gsh sessions -n 50               # Show more
gsh sessions --delete a1b2c3d4   # Delete a session
```

### Subagent management

```bash
gsh agents           # List running subagents
gsh attach <id>      # Attach to subagent's tmux session
gsh logs <id>        # View output
gsh logs <id> -f     # Follow live output
gsh kill <id>        # Stop a subagent
```

### Daemon management

```bash
gsh-daemon start --foreground   # Start (or: gsh-start)
gsh status                      # Check status
gsh stop                        # Stop daemon
gsh-restart                     # Restart
```

## Python Environment Detection

gsh detects your active Python environment and tells the LLM to use it. The CLI reads `CONDA_PREFIX`, `CONDA_DEFAULT_ENV`, and `VIRTUAL_ENV` from your shell and sends them to the daemon.

Detection priority:
1. Active conda/micromamba/venv from your terminal (highest priority)
2. Project `.venv` or `venv` in the current directory
3. gsh-managed venv at `~/.local/share/gsh/python-env`
4. Installed conda/miniconda/miniforge/micromamba/pyenv in home directory

The daemon logs which environment is being used:
```
INFO Python env: conda/micromamba 'myenv' at /Users/you/micromamba/envs/myenv
INFO   activate: micromamba activate myenv (or conda activate myenv)
```

If no environment is found, the LLM will create one at `~/.local/share/gsh/python-env` when Python is needed.

## Observability Dashboard

```bash
gsh-daemon dashboard
```

Three tabs (switch with `1`/`2`/`3`):

- **Events**: Live feed of all agent events (prompts, tool calls, results, errors). Arrow keys to scroll, `a` for auto-scroll.
- **Usage**: Token usage by provider/model with cost estimates.
- **Files**: JSONL log files. Select and Enter to load a different session.

Press `q` to quit.

## Flows (Multi-Agent Orchestration)

Flows define multi-agent workflows in TOML. Place them in `~/.config/gsh/flows/`.

```bash
gsh -f code-review "Review my recent changes"
gsh -f research-consensus "Find security issues in the codebase"
```

### Example: Publication/review consensus

```toml
# ~/.config/gsh/flows/peer-review.toml
[flow]
name = "peer-review"
description = "Agents publish findings and review each other's work"
entry = "researchers"

[coordination]
mode = "publication"
consensus_threshold = 2     # Reviews needed for consensus
allow_self_review = false

[nodes.researchers]
name = "Research Team"
publication_tags = ["finding"]
max_iterations = 20
next = "reviewers"
system_prompt = "Analyze the code and publish findings using the publish tool."

[nodes.reviewers]
name = "Review Team"
max_iterations = 15
next = "synthesizer"
system_prompt = "Use list_publications to see findings, then review each with ACCEPT/REJECT."

[nodes.synthesizer]
name = "Synthesizer"
max_iterations = 10
next = "end"
system_prompt = "Summarize all findings that reached consensus."
```

Publication tools available in coordination mode:
- `publish` — Share a finding with title, content, and tags
- `search_publications` — Search by tag or keyword
- `review_publication` — Grade with STRONG_ACCEPT / ACCEPT / NEUTRAL / REJECT / STRONG_REJECT

See `examples/flows/` for complete working examples.

## Using with OpenAI-Compatible APIs

gsh works with any OpenAI-compatible API (Ollama, vLLM, LM Studio, LocalAI, etc.).

### Ollama (local models)

```toml
[llm]
default_provider = "openai"

[llm.openai]
api_key = "ollama"
model = "llama3.1"
base_url = "http://localhost:11434/v1/chat/completions"
```

### vLLM / LocalAI

```toml
[llm]
default_provider = "openai"

[llm.openai]
api_key = "none"
model = "meta-llama/Llama-3.1-8B-Instruct"
base_url = "http://localhost:8000/v1/chat/completions"
```

### Together.AI

```toml
[llm]
default_provider = "together"

[llm.together]
model = "Qwen/Qwen3-Coder-235B-A22B-Instruct-FP8"
# api_key via TOGETHER_API_KEY env var
```

## Configuration Reference

Full config: `~/.config/gsh/config.toml`

```toml
[daemon]
log_level = "info"                   # trace, debug, info, warn, error
# socket_path = "/tmp/gsh.sock"     # Default: /tmp/gsh-$USER.sock
# log_file = "~/.local/share/gsh/daemon.log"

[llm]
default_provider = "anthropic"       # anthropic, openai, zhipu, together, ollama
max_tokens = 4096
# system_prompt = "Custom system prompt..."

[llm.anthropic]
model = "claude-sonnet-4-20250514"
# api_key = "..."                    # Or ANTHROPIC_API_KEY env var

[llm.openai]
model = "gpt-4o"
# api_key = "..."                    # Or OPENAI_API_KEY env var
# base_url = "..."                   # For compatible APIs

[llm.zhipu]
model = "glm-4"
# api_key = "..."                    # Or ZAI_API_KEY env var

[llm.together]
model = "moonshotai/Kimi-K2.5"
# api_key = "..."                    # Or TOGETHER_API_KEY env var

[llm.ollama]
model = "llama3.2"
# base_url = "http://localhost:11434"

[context]
max_events = 100                     # Shell events to keep in buffer
max_context_chars = 50000            # Max context injected into prompt
include_outputs = true               # Include command outputs

[tools]
bash_enabled = true
read_enabled = true
write_enabled = true
edit_enabled = true
glob_enabled = true
grep_enabled = true
excluded_paths = [".git/objects", "node_modules", ".venv"]
max_file_size = 1048576              # 1MB

[sessions]
max_sessions = 100                   # Max sessions on disk
max_age_days = 30                    # Auto-cleanup age
```

## Troubleshooting

### "Failed to connect to daemon"
```bash
gsh-daemon start --foreground
```

### "API key not configured"
```bash
export ANTHROPIC_API_KEY="sk-ant-..."
```

### Shell hooks not working
```bash
brew install socat   # macOS
apt install socat    # Linux
```

### Context seems empty
Make sure the zsh plugin is sourced (`source gsh.plugin.zsh`) and tracking is enabled (`gsh-enable`). Run a few commands, then `gsh what commands did I just run` to verify.

## License

MIT
