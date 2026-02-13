# gsh Examples

## Quick Start

```bash
# Start the daemon (keep this running in a terminal)
gsh-daemon start

# Source the plugin in your shell
source /path/to/gsh.plugin.zsh

# Ask a question — no quotes needed
gsh what does this project do

# Pipe content into gsh
cat error.log | gsh explain this error

# Read from a file
gsh - < requirements.txt
```

## Context-Aware Prompts

The key feature of gsh is that it **already knows what you've been doing**. The zsh plugin silently logs every command, exit code, directory change, and timing to the daemon. When you ask a question, all of that context is included automatically.

### Debugging a failed build

```bash
# You work normally — gsh records everything in the background
cd ~/myproject
cargo build                    # fails with exit 1
cat src/main.rs                # you look at the file
git log --oneline -5           # you check recent commits

# Now just ask — the LLM already sees all of the above
gsh why did my build fail

# The LLM knows:
#   - You're in ~/myproject
#   - cargo build failed (exit 1, took 2340ms)
#   - You looked at src/main.rs after
#   - Your recent git history
# No need to paste error output or explain context.
```

### Investigating a slow command

```bash
pip install tensorflow         # takes 45 seconds
pytest tests/                  # takes 12 seconds, 3 failures

gsh why are my tests slow and why are 3 failing

# The LLM sees the durations and exit codes.
# It knows pip install took 45s and pytest exited with failures.
```

### Working across directories

```bash
cd ~/frontend
npm run build                  # exit 0
cd ~/backend
python manage.py test          # exit 1

gsh the frontend built fine but the backend tests are failing, what changed

# The LLM sees both directory changes and both command results.
```

### After a long session

```bash
# After 30 minutes of work, with dozens of commands recorded:
gsh summarize what I've been doing for the past half hour

# The LLM sees your full shell history — up to 100 events
# (configurable via max_events in config.toml)
```

## Controlling Context

```bash
# Disable tracking (for sensitive commands)
gsh-disable
export SECRET_KEY=abc123
curl -H "Authorization: Bearer $TOKEN" https://api.example.com/...
gsh-enable

# Check what context the LLM would see
gsh what commands did I run recently

# Restart daemon for a clean context slate
gsh-restart
```

### Tuning context size

In `~/.config/gsh/config.toml`:

```toml
[context]
# Keep more history for complex debugging sessions
max_events = 200
max_context_chars = 80000

# Or keep it minimal to save tokens
max_events = 20
max_context_chars = 10000
```

## One-Shot Prompts

Every prompt creates a persisted session automatically.

```bash
# Simple questions
gsh how do I list all docker containers

# Code generation
gsh write a python script that parses CSV and outputs JSON

# File operations (agent has tools: bash, read, write, edit, glob, grep)
gsh find all TODO comments in this project
gsh rename all .jpeg files to .jpg in the current directory
gsh add error handling to src/main.rs

# System tasks
gsh what processes are using port 8080
gsh show disk usage for the top 10 largest directories

# Piping content
git diff | gsh review these changes
cat Dockerfile | gsh are there any security issues here
kubectl get pods | gsh which pods are unhealthy
```

## Interactive Chat

`gsh chat` opens a multi-turn REPL. Use it when you need a back-and-forth conversation. The shell context from your terminal is included in the first message.

> **Note**: One-shot `gsh <prompt>` calls also persist sessions. `gsh chat` is just the interactive REPL mode — the only difference is it keeps the connection open for multiple turns.

```bash
gsh chat

# Chat session started (a1b2c3d4)
# Type 'exit' or Ctrl+D to end the session.
#
# > what files are in src/?
# [Using tool: bash]
# [Tool bash OK]
# Here are the files in src/...
#
# > now add a new module called utils
# [Using tool: write]
# [Tool write OK]
# Created src/utils.rs with...
#
# > exit
# Chat session ended.
```

## Session Management

Sessions are stored as JSONL files in `~/.local/share/gsh/sessions/` and survive daemon restarts and reboots.

```bash
# List saved sessions
gsh sessions

# Output:
#   Saved sessions (3):
#
#   a1b2c3d4 |  12 msgs | 2h ago | what files are in src/?
#   e5f6a7b8 |   4 msgs | 1d ago | help me debug the auth module
#   c9d0e1f2 |   8 msgs | 3d ago | write unit tests for parser
#
#   Resume with: gsh chat --session <id>
#   Delete with: gsh sessions --delete <id>

# Resume a session — the full conversation history is sent to the LLM
gsh chat --session a1b2c3d4

# Output:
#   Chat session resumed (a1b2c3d4, 12 messages)
#   Type 'exit' or Ctrl+D to end the session.
#
#   > where were we?
#   ...

# Delete old sessions
gsh sessions --delete e5f6a7b8

# Show more sessions
gsh sessions -n 50
```

## Provider & Model Selection

```bash
# Use a specific provider
gsh -p openai explain this Makefile
gsh -p ollama summarize README.md

# Use a specific model
gsh -m gpt-4o review my last git commit
gsh -m claude-sonnet-4-20250514 refactor this function

# Combine both
gsh -p together -m moonshotai/Kimi-K2.5 what files are here
```

## Python Environment Detection

gsh detects your active Python environment (conda, micromamba, venv, pyenv) and tells the LLM to use it. No more "python: command not found" errors.

```bash
# Activate your environment in the terminal
micromamba activate myenv

# gsh detects it automatically — the LLM will use myenv's python
gsh write a hello world program and run it

# The daemon log shows what was detected:
#   INFO Python env: conda/micromamba 'myenv' at /Users/you/micromamba/envs/myenv
#   INFO   activate: micromamba activate myenv
```

If no environment is active, gsh checks for project venvs (`.venv/`, `venv/`) in the current directory and common installations in your home directory.

## Flows (Multi-Agent Orchestration)

Flows run multiple agents in sequence or parallel, defined via TOML files in `~/.config/gsh/flows/`.

```bash
# Run a flow
gsh -f simple-pub-test "Analyze the main.rs file and publish findings"

# Multi-agent research with peer review
gsh -f research-consensus "Find potential security issues in the codebase"
```

### Example Flow: Code Review

Create `~/.config/gsh/flows/code-review.toml`:

```toml
[flow]
name = "code-review"
description = "Automated code review with researcher and reviewer"
version = "1.0.0"
entry = "researcher"

[coordination]
mode = "publication"
consensus_threshold = 1
allow_self_review = false

[nodes.researcher]
name = "Code Analyst"
description = "Analyze code for issues"
publication_tags = ["issue", "suggestion"]
max_iterations = 15
next = "reviewer"

system_prompt = """
You are a code reviewer. Analyze the files and publish findings using the `publish` tool.
Focus on bugs, security issues, and code quality. Include file paths and line numbers.
"""

[nodes.reviewer]
name = "Review Validator"
description = "Validate findings"
max_iterations = 10
next = "end"

system_prompt = """
You are a senior engineer. Use `list_publications` to see findings, then `review` each one.
Grade as ACCEPT (valid issue), NEUTRAL (needs evidence), or REJECT (not a real issue).
"""
```

Then run it:

```bash
gsh -f code-review "Review the authentication module"
```

## Subagent Management

When flows or prompts spawn background agents in tmux sessions:

```bash
# List running agents
gsh agents

# Output:
#   Running agents (2):
#   [0001] gsh-agent-0001 - Analyzing src/auth.rs
#   [0002] gsh-agent-0002 - Reviewing findings

# View an agent's output
gsh logs 1
gsh logs 1 --all        # include full scrollback
gsh logs 1 --follow     # live tail

# Attach to an agent's tmux session (for debugging)
gsh attach 1

# Kill a stuck agent
gsh kill 1
```

## Daemon Management

```bash
# Start in foreground (see logs in terminal)
gsh-daemon start

# Check daemon status
gsh status

# Output:
#   Daemon running
#   Version: 0.1.0
#   Uptime: 3600s
#   Socket: /tmp/gsh-gab.sock

# Stop the daemon
gsh stop
```

## Shell Integration

Source the zsh plugin for seamless integration:

```bash
# Add to ~/.zshrc
source /path/to/gsh.plugin.zsh

# Now use gsh directly
gsh what does this error mean
gsh --flow code-review "check src/"
```

The plugin registers three zsh hooks that fire automatically:

| Hook | When | What it captures |
|------|------|-----------------|
| `preexec` | Before every command | Command text, working directory, timestamp |
| `precmd` | After every command | Exit code, working directory, duration in ms |
| `chpwd` | On directory change | Old path, new path, timestamp |

Events are sent as fire-and-forget JSON messages over a Unix socket. The daemon stores them in a configurable circular buffer and formats them into the LLM's system prompt on each call.

## Tips

- **No quotes needed**: `gsh explain this Makefile` works — no need for `gsh "explain this Makefile"`
- **Context is automatic**: just use your shell normally, the LLM sees everything
- **Sessions persist across reboots**: stored as JSONL files in `~/.local/share/gsh/sessions/`
- **Disable for sensitive work**: `gsh-disable` stops context logging, `gsh-enable` resumes
- **Tune context size**: adjust `max_events` and `max_context_chars` in config for your use case
- **Python environments**: gsh detects your active conda/micromamba/venv and tells the LLM to use it
- **8-char IDs**: session IDs can be shortened to the first 8 characters
- **Pipe anything**: `git diff | gsh review these changes` works great
- **Tool visibility**: watch `[Using tool: ...]` messages to see what the agent is doing
