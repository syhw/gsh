# Context Engineering in gsh

gsh uses two complementary systems to manage context effectively:

1. **Context Management** — A 3-layer system (truncate, prune, compact) that prevents context overflow during long agent sessions.
2. **Context Retrieval** — A JIT (just-in-time) system that gives the agent on-demand access to shell history, past sessions, and observability logs via the `search_context` tool, rather than dumping everything upfront.

Together these ensure the agent always has the right context — recent activity is injected automatically as a small ambient summary, deeper history is available on demand, and the conversation never overflows regardless of session length.

```
                 ┌──────────────────────────────────────────┐
                 │         CONTEXT RETRIEVAL (JIT)          │
                 │                                          │
                 │  Ambient summary (last 5 commands)       │
                 │  injected into first message             │
                 │          +                               │
                 │  search_context tool for on-demand       │
                 │  queries across 3 data stores:           │
                 │                                          │
                 │  ┌─────────────┐ ┌──────┐ ┌──────────┐  │
                 │  │Shell History│ │Sesns.│ │Obs. Logs │  │
                 │  │  (JSONL)    │ │(JSONL│ │ (JSONL)  │  │
                 │  └─────────────┘ └──────┘ └──────────┘  │
                 └──────────────────────────────────────────┘

                     Agent Loop Iteration
                            │
                ┌───────────▼──────────┐
                │  Layer 2: PRUNE      │  Free. Erases old tool outputs
                │  (every iteration)   │  beyond a protection window.
                └───────────┬──────────┘
                            │
                ┌───────────▼──────────┐
                │  Layer 3: COMPACT    │  1 LLM call. Summarizes entire
                │  (if over threshold) │  conversation into a handoff.
                └───────────┬──────────┘
                            │
                ┌───────────▼──────────┐
                │  Call LLM            │
                └───────────┬──────────┘
                            │
                ┌───────────▼──────────┐
                │  Execute tools       │
                └───────────┬──────────┘
                            │
                ┌───────────▼──────────┐
                │  Layer 1: TRUNCATE   │  Free. Caps each tool output
                │  (per tool result)   │  at max_tool_output_bytes.
                └───────────┬──────────┘
                            │
                     Next iteration
```

## Configuration

All settings live in `~/.config/gsh/config.toml` under `[context]`:

```toml
[context]
# Context management (3-layer system)
max_tool_output_bytes = 30000   # Layer 1: truncate tool outputs above this (0 = off)
prune_protect_tokens = 40000    # Layer 2: keep this many tokens of recent tool outputs
compact_threshold = 0.85        # Layer 3: compact when context reaches 85% of window
max_iterations = 25             # Max agent loop iterations per request

# Context retrieval (JIT system)
ambient_commands = 5            # Number of recent commands injected automatically (0 = fully JIT)
max_history_lines = 10000       # Maximum shell history events persisted on disk
```

## Token Estimation

gsh estimates tokens using the **bytes / 4** heuristic (same as OpenAI Codex). This avoids needing a tokenizer dependency and is accurate enough for threshold decisions. Each message also adds 4 tokens of overhead.

For reference:
- 1 KB of English text ~ 250 tokens
- A 30 KB tool output ~ 7,500 tokens
- A 200K-token context window ~ 800 KB of text

## Layer 1: Truncate (per tool output, free)

**When:** Immediately after each tool executes, before its output enters the message history.

**What:** If a tool output exceeds `max_tool_output_bytes` (default: 30,000 bytes), gsh keeps the first half and last half of the byte budget, cutting the middle:

```
┌──────────── original output (e.g., 80 KB) ─────────────┐
│ first 15KB │       ... (50,000 bytes omitted) ...       │ last 15KB │
└─────────────────────────────────────────────────────────┘
```

**Why prefix + suffix:** Error messages and summaries tend to appear at the end of command output, while headers and context appear at the start. Cutting the middle preserves both.

**Logged as:** `EventKind::Truncation { tool, original_bytes, truncated_bytes }` in the JSONL observability log.

### Example: Reading a large file

You ask gsh to read a 100 KB log file:

```
You: "Read /var/log/app.log and find the error"
```

1. Agent calls the `read` tool on `/var/log/app.log`
2. Tool returns 100,000 bytes of log data
3. **Layer 1 kicks in:** output exceeds 30,000 bytes
4. gsh keeps bytes 0..15,000 and bytes 85,000..100,000
5. The LLM sees:

```
[first 15,000 bytes of the log]

... (70,000 bytes omitted) ...

[last 15,000 bytes of the log]
```

6. The error message (usually near the end) is preserved. The LLM can still find it.

### Example: Small tool output (no truncation)

```
You: "What's in this directory?"
```

1. Agent calls `bash` with `ls -la`
2. Tool returns 800 bytes
3. 800 < 30,000 — **no truncation**, output passes through unchanged

### Disabling truncation

Set `max_tool_output_bytes = 0` in config to disable. All tool output will be kept verbatim. Not recommended for long sessions.

## Layer 2: Prune (per iteration, free)

**When:** At the start of every agent loop iteration, before calling the LLM.

**What:** Walks backwards through the message history counting tool output tokens. The most recent `prune_protect_tokens` (default: 40,000) worth of tool outputs are kept intact. Older tool outputs that are larger than ~100 tokens get replaced with a placeholder:

```
[output pruned, was ~2500 tokens]
```

Small tool outputs (< 100 tokens, roughly < 400 bytes) are never pruned — they're cheap and often contain important status messages like "OK" or error codes.

**Why backwards:** Recent tool outputs are most relevant. The agent just saw them and may be acting on them. Old tool outputs from 5 iterations ago are almost never re-read.

### Example: Multi-step file editing session

Imagine a chat session where the agent has been working for 8 iterations:

```
Iteration 1: read package.json      → 2,000 bytes (~500 tokens)
Iteration 2: read src/index.ts      → 12,000 bytes (~3,000 tokens)
Iteration 3: bash "npm test"        → 40,000 bytes (~10,000 tokens)  ← big
Iteration 4: read src/utils.ts      → 8,000 bytes (~2,000 tokens)
Iteration 5: edit src/utils.ts      → 200 bytes (~50 tokens)
Iteration 6: bash "npm test"        → 45,000 bytes (~11,250 tokens)  ← big
Iteration 7: read src/index.ts      → 12,000 bytes (~3,000 tokens)
Iteration 8: edit src/index.ts      → 200 bytes (~50 tokens)
```

At the start of iteration 9, pruning walks backwards:

| Tool output (newest first) | Tokens | Running total | Action |
|---|---|---|---|
| edit src/index.ts (iter 8) | ~50 | 50 | **keep** (< 100 tokens) |
| read src/index.ts (iter 7) | ~3,000 | 3,050 | **keep** (within 40K window) |
| bash npm test (iter 6) | ~11,250 | 14,300 | **keep** (within 40K window) |
| edit src/utils.ts (iter 5) | ~50 | 14,350 | **keep** (< 100 tokens) |
| read src/utils.ts (iter 4) | ~2,000 | 16,350 | **keep** (within 40K window) |
| bash npm test (iter 3) | ~10,000 | 26,350 | **keep** (within 40K window) |
| read src/index.ts (iter 2) | ~3,000 | 29,350 | **keep** (within 40K window) |
| read package.json (iter 1) | ~500 | 29,850 | **keep** (within 40K window) |

In this case, everything fits within the 40K protection window, so nothing is pruned.

Now imagine 5 more iterations with large outputs push the total past 40K. The oldest tool outputs (package.json, src/index.ts from iteration 2, the first npm test) would be replaced with `[output pruned, was ~N tokens]`. The LLM still knows *which* tools were called and roughly how much output they produced, but the actual content is gone.

### What the LLM sees after pruning

Before pruning (iteration 1's read):
```json
{"tool_use_id": "1", "content": "{\n  \"name\": \"my-app\",\n  \"version\": \"1.0.0\", ...2000 bytes...}"}
```

After pruning:
```json
{"tool_use_id": "1", "content": "[output pruned, was ~500 tokens]"}
```

The tool call itself (tool name, input) is never pruned — only the result content.

## Layer 3: Compact (threshold-triggered, costs 1 LLM call)

**When:** At the start of each iteration, after pruning, if estimated total tokens exceed `context_limit * compact_threshold` (default: 85% of the provider's context window).

**What:** Sends the entire message history plus a compaction prompt to the LLM and asks it to write a "handoff briefing." The briefing replaces the entire message history with a single user message.

**Context limit** comes from the provider's declared capabilities:
- Anthropic Claude: 200,000 tokens → compacts at ~170,000
- OpenAI GPT-4o: 128,000 tokens → compacts at ~108,800
- Ollama local model (4K): 4,000 tokens → compacts at ~3,400
- Fallback (unknown provider): 100,000 tokens → compacts at ~85,000

### The compaction prompt

```
You are performing a CONTEXT CHECKPOINT. Summarize the conversation so far
into a handoff briefing for yourself to continue the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences discovered
- Files modified or read (with paths)
- What remains to be done (clear next steps)
- Any critical data, code snippets, or references needed to continue

Be thorough but concise. Focus on what's needed to seamlessly continue.
```

### Example: Long chat session hits compaction

You're in a `gsh chat` session working on a refactoring task. After 15 iterations of reading files, running tests, editing code, the estimated token count reaches 175,000 (provider is Claude with 200K limit):

```
Iteration 15 starts:
  1. Prune runs (replaces some old tool outputs)
  2. Estimate tokens: ~175,000
  3. Threshold: 200,000 × 0.85 = 170,000
  4. 175,000 > 170,000 → COMPACT triggered
```

The agent:
1. Appends the compaction prompt to the full message history
2. Calls the LLM (non-streaming, no tools) to generate a summary
3. Replaces the entire history with one message:

```
[Context compacted - previous conversation summary]

## Progress
- Refactoring auth module from class-based to functional approach
- Completed: src/auth/login.ts, src/auth/session.ts
- Tests passing for login and session modules

## Key decisions
- Using dependency injection pattern (user preferred)
- Keeping backward-compatible exports in index.ts

## Files modified
- src/auth/login.ts (rewritten)
- src/auth/session.ts (rewritten)
- src/auth/index.ts (updated exports)
- tests/auth.test.ts (updated)

## Remaining work
- src/auth/register.ts needs same refactoring
- src/auth/middleware.ts depends on register.ts
- Integration tests not yet updated

## Critical context
- User wants to keep the AuthConfig type unchanged
- npm test command: `npm run test:auth`
```

4. The CLI shows: `[context compacted: 175000 → 2500 tokens]`
5. The agent continues from iteration 16 with ~2,500 tokens of context instead of 175,000

### What happens if compaction fails

If the LLM call for compaction fails (network error, provider outage, etc.), the agent returns an error and stops. The message history was consumed for the compaction attempt, so it cannot continue with the original context. In a chat session, you can send another message to retry.

## How the layers interact

The three layers work together in sequence. Here's a timeline of a long session:

```
Iteration 1-5:   Everything fits. No layers activate.
                 Context: ~20,000 tokens

Iteration 6:     A tool reads a huge file (80 KB).
                 Layer 1 truncates it to 30 KB (~7,500 tokens).
                 Context: ~35,000 tokens

Iteration 7-10:  More tool calls accumulate.
                 Layer 2 starts pruning oldest tool outputs.
                 Context stays around ~50,000-60,000 tokens

Iteration 11-15: Heavy tool use (many reads, bash runs).
                 Layer 2 prunes aggressively.
                 Context climbs to ~170,000 tokens despite pruning.

Iteration 16:    Layer 3 fires. Context compacted to ~3,000 tokens.
                 Agent continues with fresh context + summary.

Iteration 17+:   Cycle restarts from low token count.
```

## Observability

All three layers log events to the JSONL observability log (`~/.local/share/gsh/logs/`):

| Event | Layer | Fields |
|---|---|---|
| `truncation` | 1 | `tool`, `original_bytes`, `truncated_bytes` |
| (pruning is silent) | 2 | Only logged at `debug` level via tracing |
| `compaction` | 3 | `original_tokens`, `summary_tokens`, `messages_before`, `messages_after` |

The dashboard (`gsh dashboard`) shows compaction events with a `C` marker and truncation events with a `T` marker.

The CLI prints compaction notifications inline:
```
[context compacted: 175000 -> 2500 tokens]
```

## Comparison with other tools

| Feature | gsh | Claude Code | OpenCode | Codex CLI |
|---|---|---|---|---|
| Tool output truncation | prefix+suffix, 30KB | 25K tokens | via prune tool | prefix+suffix, ~10K tokens |
| Old output pruning | automatic, 40K window | - | automatic + LLM tools | - |
| Compaction trigger | 85% of context | 95% of context | overflow detection | 95% of context |
| Compaction method | LLM summary | LLM summary | LLM summary | LLM summary |
| Token estimation | bytes/4 | tokenizer | tokenizer | bytes/4 |
| Max iterations | 25 (configurable) | unlimited | unlimited | configurable |

## Tuning guide

**For small context models (Ollama, 4K-8K context):**
```toml
[context]
max_tool_output_bytes = 8000     # Aggressive truncation
prune_protect_tokens = 4000      # Small protection window
compact_threshold = 0.75         # Compact early
max_iterations = 15              # Fewer iterations
```

**For large context models (Claude 200K, GPT-4o 128K):**
```toml
[context]
max_tool_output_bytes = 50000    # More generous truncation
prune_protect_tokens = 80000     # Large protection window
compact_threshold = 0.90         # Compact later
max_iterations = 40              # More room to work
```

**For cost-sensitive usage:**
```toml
[context]
max_tool_output_bytes = 15000    # Tight truncation saves input tokens
prune_protect_tokens = 20000     # Aggressive pruning
compact_threshold = 0.70         # Compact early (fewer tokens per call)
max_iterations = 15
```

---

## Context Retrieval

The retrieval system is the other half of context engineering. Instead of dumping all shell history into the first message (which wastes tokens and floods the LLM), gsh injects a small **ambient summary** and gives the agent a **`search_context` tool** to pull in deeper history on demand.

### The problem it solves

Previously, gsh captured up to 100 shell events (commands, exit codes, directory changes) in memory and injected them all into the system prompt. This had three problems:

1. **Token waste** — 100 events can easily be 5-10K tokens, most of which are irrelevant to the current request
2. **Lost on restart** — events lived only in memory; restarting the daemon erased all history
3. **No access to past sessions or logs** — the agent couldn't search what happened in previous conversations

### How it works now

```
┌─────────────────────────────────────────────────────┐
│                  First message                       │
│                                                      │
│  System prompt + ambient summary (last 5 commands)   │
│  + "Use the search_context tool for more history"    │
└─────────────────────────────────────────────────────┘
                        │
                        ▼
              Agent decides: do I need
              more context for this task?
                   │           │
                   No          Yes
                   │           │
                   ▼           ▼
             Answer       search_context tool
             directly     ┌────────────────┐
                          │ source:        │
                          │  shell_history │
                          │  sessions      │
                          │  logs          │
                          └────────────────┘
```

### Ambient summary

The ambient summary is injected into every first message (new conversation, not resumed chats). It contains the last `ambient_commands` (default: 5) shell commands the user ran, formatted with exit codes and directories:

```
# Recent Shell Activity

$ cargo build (in /Users/you/project) → exit 0
$ cargo test (in /Users/you/project) → exit 1
$ vim src/main.rs (in /Users/you/project) → exit 0
$ cargo test (in /Users/you/project) → exit 0
$ git diff (in /Users/you/project) → exit 0

Use the `search_context` tool for more shell history, past sessions, or logs.
```

This gives the agent immediate situational awareness — it knows where you are and what you just did — without burning thousands of tokens on old, irrelevant commands.

Set `ambient_commands = 0` to disable the ambient summary entirely (fully JIT mode — the agent must always call the tool to see any history).

### The `search_context` tool

A single unified tool with three data sources, exposed to the agent alongside the standard tools (bash, read, write, etc.):

```json
{
  "name": "search_context",
  "input_schema": {
    "source": "shell_history | sessions | logs",
    "query": "optional search pattern (case-insensitive substring match)",
    "cwd": "directory prefix filter (shell_history only)",
    "exit_code": "exact code, or -1 for all failures (shell_history only)",
    "event_type": "bash_exec | tool_call | prompt | error (logs only)",
    "last_n": "max results to return (default 20, max 50)"
  }
}
```

#### Source: `shell_history`

Searches the persistent shell history file (`~/.local/share/gsh/shell-history.jsonl`). Every command you run in a gsh-enabled shell is appended here and survives daemon restarts.

**Example: "What commands failed recently?"**
```json
{"source": "shell_history", "exit_code": -1, "last_n": 10}
```
Returns:
```
# Shell History (3 results)

$ npm test (in /frontend) → exit 1 (2025-06-15 14:32)
$ cargo build (in /backend) → exit 101 (2025-06-15 14:28)
$ make deploy (in /infra) → exit 2 (2025-06-15 13:55)
```

**Example: "What did I run in this directory?"**
```json
{"source": "shell_history", "cwd": "/Users/you/project", "last_n": 20}
```

**Example: "Did I run any docker commands?"**
```json
{"source": "shell_history", "query": "docker"}
```

#### Source: `sessions`

Searches past agent conversation sessions (stored as JSONL in `~/.local/share/gsh/sessions/`). Matches the keyword against session titles and message content, returning excerpts.

**Example: "What did we discuss about auth?"**
```json
{"source": "sessions", "query": "authentication"}
```
Returns:
```
# Session Search: "authentication" (2 results)

## Refactor auth module (2025-06-14 16:20)
Session: a3f8b2c1 | 24 messages | cwd: /project
  > ...switching from JWT to session-based authentication...
  > ...the AuthConfig type should stay unchanged per user request...

## Fix login bug (2025-06-12 09:15)
Session: e7d1a9f4 | 8 messages | cwd: /project
  > ...authentication failing because token expired check was off by one...
```

#### Source: `logs`

Searches the observability logs (JSONL in `~/.local/share/gsh/logs/`). These contain detailed records of every bash execution (with stdout/stderr), tool call, prompt, and error from agent sessions.

**Example: "What was the output of the last test run?"**
```json
{"source": "logs", "event_type": "bash_exec", "query": "test", "last_n": 5}
```
Returns:
```
# Log Search (2 results)

[2025-06-15 14:32:01] bash: `npm test` -> exit 1 (4523ms)
  output: FAIL src/auth.test.ts ● login() should validate token...
  stderr: Jest encountered 1 failure

[2025-06-15 14:28:15] bash: `cargo test` -> OK (1205ms)
  output: running 42 tests... test result: ok. 42 passed
```

**Example: "Show me recent errors"**
```json
{"source": "logs", "event_type": "error", "last_n": 10}
```

### Persistent shell history

Shell events are persisted to `~/.local/share/gsh/shell-history.jsonl` as newline-delimited JSON. Each line is a `ShellEvent` (either a `Command` with command text, exit code, cwd, duration, or a `DirChange` with old/new path).

On daemon startup:
1. The file is loaded into the in-memory `ContextAccumulator` (capped at `max_events`)
2. If the file exceeds `max_history_lines` (default: 10,000), it's truncated to keep only the most recent entries
3. New events are appended in real-time as you use the shell

This means shell context survives daemon restarts, machine reboots, and even works across multiple terminal sessions — they all write to the same file.

### When the agent uses it

The agent decides autonomously whether to call `search_context` based on the user's request. Some patterns:

| User says | Agent behavior |
|---|---|
| "list files here" | Doesn't need history — answers directly with `bash ls` |
| "what was I doing before?" | Calls `search_context` with `source: shell_history` to see recent commands |
| "we discussed this last time" | Calls `search_context` with `source: sessions` to find the previous conversation |
| "the build was failing earlier" | Calls `search_context` with `source: logs, event_type: bash_exec, query: build` |
| "what error did we hit?" | Calls `search_context` with `source: logs, event_type: error` |

### How retrieval interacts with the 3-layer system

The `search_context` tool outputs are subject to the same 3-layer management as any other tool:

- **Layer 1 (Truncate):** If a search returns a huge result, it gets truncated to `max_tool_output_bytes`
- **Layer 2 (Prune):** Old search results get pruned like any other tool output
- **Layer 3 (Compact):** If compaction fires, the LLM summary includes key findings from search results

This means the agent can freely search for context without worrying about overflowing the context window — the management layers handle it automatically.

### Data flow diagram

```
Shell commands (all terminals)
        │
        ▼
 ContextAccumulator ──append──▶ ~/.local/share/gsh/shell-history.jsonl
        │
        ├── generate_ambient_summary(5) ──▶ First message to LLM
        │
        └── (in-memory buffer for real-time tracking)

 Agent tool call: search_context
        │
        ▼
 ContextRetriever
        │
        ├── search_shell_history() ──▶ Reads shell-history.jsonl
        ├── search_sessions()      ──▶ Reads ~/.local/share/gsh/sessions/*.jsonl
        └── search_logs()          ──▶ Reads ~/.local/share/gsh/logs/*.jsonl
```

### Tuning retrieval

**Disable ambient context (fully JIT):**
```toml
[context]
ambient_commands = 0   # Agent must always call search_context
```

**More ambient context:**
```toml
[context]
ambient_commands = 15   # Inject last 15 commands
```

**Longer history retention:**
```toml
[context]
max_history_lines = 50000   # Keep 50K commands on disk
```

**Minimal history:**
```toml
[context]
max_history_lines = 1000   # Only keep last 1K commands
```
