# gsh - Agentic Shell

Your shell already knows what you've been doing. **gsh makes the LLM know too.**

A background daemon silently records every command, exit code, directory change, and timing. When you ask a question, all of that context is sent to the LLM automatically. No copy-paste, no explaining what happened.

## Install

```bash
git clone https://github.com/gabrielchl/gsh.git && cd gsh && ./install.sh
```

Then set your API key (at least one) and restart your shell:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."   # Claude
export OPENAI_API_KEY="sk-..."          # OpenAI / GPT
export ZAI_API_KEY="..."                # GLM-4 / z.ai
export TOGETHER_API_KEY="..."           # Together.AI (Kimi K2, Qwen)
# add to ~/.zshrc, then:
source ~/.zshrc
```

> Requires: Rust toolchain ([rustup.rs](https://rustup.rs)), zsh, socat (`brew install socat`)

## Why gsh

```bash
cd ~/myproject
cargo build                    # fails with exit 1
cat src/main.rs                # you look at the file
git log --oneline -5           # you check recent commits

gsh why did my build fail
```

You didn't paste the error. You didn't explain where you are or what you tried. **gsh already sent all of that as context** — the LLM sees your working directory, the failed command, its exit code, the duration, and everything you did after.

This is the entire idea. The shell hooks capture context continuously, and gsh injects it into every LLM call.

### What the LLM sees

```
# Recent Shell Activity

[14:02:01] cd: ~ -> ~/myproject
[14:02:03] ~/myproject $ cargo build
  -> exit 1 (2340ms)
[14:02:08] ~/myproject $ cat src/main.rs
  -> OK (12ms)
[14:02:15] ~/myproject $ git log --oneline -5
  -> OK (45ms)
```

## Examples

```bash
# No quotes needed — just type naturally
gsh what files are in this directory
gsh explain the error in my last command

# It knows about timing
pip install tensorflow         # takes 45 seconds
pytest tests/                  # 3 failures
gsh why are my tests slow and failing

# It tracks directory changes
cd ~/frontend && npm run build          # exit 0
cd ~/backend && python manage.py test   # exit 1
gsh frontend built fine but backend tests fail, what changed

# Pipe anything in
git diff | gsh review these changes
cat Dockerfile | gsh any security issues here
kubectl get pods | gsh which pods are unhealthy

# It can use tools: bash, read, write, edit, glob, grep
gsh find all TODO comments in this project
gsh rename all .jpeg files to .jpg in the current directory
gsh add error handling to src/main.rs

# After a long session
gsh summarize what I've been doing for the past half hour

# Choose provider/model
gsh -p openai -m gpt-4o explain this Makefile
```

## Interactive Chat

```bash
gsh chat                         # new session
gsh chat --session a1b2c3d4      # resume a saved one
```

Every call (one-shot or chat) persists as a session that survives daemon restarts.

```bash
gsh sessions                     # list saved sessions
gsh sessions --delete <id>       # delete one
```

## Context Control

```bash
gsh-disable         # pause context tracking (for sensitive commands)
gsh-enable          # resume

export GSH_ENABLED=0   # disable for this shell session
```

Tune how much context the LLM sees in `~/.config/gsh/config.toml`:

```toml
[context]
max_events = 100           # shell events in the circular buffer
max_context_chars = 50000  # max characters injected into the prompt
```

## Environment Detection

gsh detects active Python environments (conda, micromamba, venv, pyenv) and tells the LLM to use them. No "python: command not found" errors.

## Flows (Multi-Agent)

Define multi-agent workflows in TOML. Agents can publish findings and review each other's work.

```bash
gsh -f code-review "Review the authentication module"
gsh -f research-consensus "Find security issues in the codebase"
```

See `examples/flows/` for working examples.

## MCP Support

gsh agents can use tools from external MCP servers via streamable-HTTP transport. Add to `~/.config/gsh/config.toml`:

```toml
[mcp.servers.web-search]
type = "streamable-http"
url = "https://api.example.com/mcp"
headers = { Authorization = "Bearer $API_KEY" }
```

MCP tools appear alongside built-in tools automatically.

## Subagent Management

```bash
gsh agents           # list running subagents
gsh logs <id>        # view output
gsh logs <id> -f     # follow live
gsh attach <id>      # attach to tmux session
gsh kill <id>        # stop a subagent
```

## Daemon

```bash
gsh-daemon start     # start (auto-starts via shell plugin)
gsh status           # check status
gsh stop             # stop
gsh-restart          # restart
```

## Verbosity

Control CLI output with `GSH_VERBOSITY`:

```bash
GSH_VERBOSITY=none gsh ...     # clean output only — no [brackets]
GSH_VERBOSITY=progress gsh ... # default — tool use, stats
GSH_VERBOSITY=debug gsh ...    # full trace — raw messages, tool I/O
```

## Configuration

Full reference: `~/.config/gsh/config.toml`

```toml
[daemon]
log_level = "info"

[llm]
default_provider = "anthropic"    # anthropic, openai, zhipu, together, ollama
max_tokens = 4096

[llm.anthropic]
model = "claude-sonnet-4-20250514"
# api_key via ANTHROPIC_API_KEY env var

[llm.openai]
model = "gpt-4o"
# base_url = "http://localhost:11434/v1/chat/completions"  # for Ollama/vLLM

[context]
max_events = 100
max_context_chars = 50000

[tools]
bash_enabled = true
read_enabled = true
write_enabled = true
edit_enabled = true
glob_enabled = true
grep_enabled = true
```

## Uninstall

```bash
./install.sh --uninstall
```

## License

MIT
