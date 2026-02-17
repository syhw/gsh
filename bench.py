#!/usr/bin/env python3
"""
Benchmark LLM providers through gsh-daemon.
Measures TTFT and completion time for implementing & testing a Fibonacci function.

Usage:
    python3 bench.py

Requires gsh-daemon to be running. Tests all providers with available API keys.
"""

import os
import re
import subprocess
import sys
import time

PROMPT = (
    "Implement a Python function fibonacci(n) that returns the nth Fibonacci number. "
    "Handle edge cases: fib(0)=0, fib(1)=1, negative n raises ValueError. "
    "Then write pytest-style unit tests covering fib(0), fib(1), fib(10)=55, fib(20)=6765, "
    "and negative input. Show the complete code ready to run."
)

# (display_name, gsh_provider_flag, model, env_var_for_api_key)
PROVIDERS = [
    ("Anthropic",    "anthropic", "claude-opus-4-6",              "ANTHROPIC_API_KEY"),
    ("OpenAI",       "openai",    "gpt-5.2-codex",                "OPENAI_API_KEY"),
    ("Z.ai (GLM)",   "zai",       "glm-5",                        "ZAI_API_KEY"),
    ("Together.AI",  "together",  "Qwen/Qwen3-Coder-Next-FP8",   "TOGETHER_API_KEY"),
    ("Mistral",      "mistral",   "devstral-small-latest",        "MISTRAL_API_KEY"),
    ("Cerebras",     "cerebras",  "zai-glm-4.7",                  "CEREBRAS_API_KEY"),
    ("Google",       None,        "gemini-3.0-pro",                "GOOGLE_API_KEY"),
]

# Matches: [TTFT 4.1s | 143 tok/s | ~194 tokens | 5.5s]
# Also:    [TTFT 4.1s | ~194 tokens | 5.5s]  (variant without tok/s)
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
STATS_FULL_RE = re.compile(
    r"\[TTFT\s+(\S+)\s+\|\s+(\d+)\s+tok/s\s+\|\s+~(\d+)\s+tokens?\s+\|\s+(\S+)\]"
)
STATS_SHORT_RE = re.compile(
    r"\[TTFT\s+(\S+)\s+\|\s+~(\d+)\s+tokens?\s+\|\s+(\S+)\]"
)


def strip_ansi(text):
    return ANSI_RE.sub("", text)


def parse_stats(stderr_raw):
    """Parse TTFT stats from gsh stderr output. Returns dict or None."""
    text = strip_ansi(stderr_raw)

    m = STATS_FULL_RE.search(text)
    if m:
        return {
            "ttft": m.group(1),
            "tps": m.group(2),
            "tokens": m.group(3),
            "total": m.group(4),
        }

    m = STATS_SHORT_RE.search(text)
    if m:
        return {
            "ttft": m.group(1),
            "tps": "-",
            "tokens": m.group(2),
            "total": m.group(3),
        }

    return None


def run_benchmark_once(provider_flag, model):
    """Run gsh once and return (stats_dict, error_string)."""
    env = os.environ.copy()
    env["GSH_VERBOSITY"] = "progress"

    cmd = ["gsh", "-p", provider_flag, "-m", model, PROMPT]

    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, env=env, timeout=300
        )
    except subprocess.TimeoutExpired:
        return None, "timeout (300s)"

    combined = strip_ansi(proc.stderr + proc.stdout)

    if proc.returncode != 0:
        first_line = combined.strip().split("\n")[0][:80]
        return None, first_line or f"exit code {proc.returncode}"

    # Check stats first — if we got TTFT stats, the request completed
    # (even if there were intermediate tool errors along the way)
    stats = parse_stats(proc.stderr)
    if stats:
        return stats, None

    # Only if no stats: scan for top-level errors (not indented tool result errors)
    for line in strip_ansi(proc.stderr).split("\n"):
        if line.startswith("Error:"):
            return None, line[:80]

    # No stats found but command succeeded — return raw wall time hint
    return None, None


def run_benchmark(provider_flag, model, retries=1):
    """Run benchmark with retries for transient errors (429, timeout)."""
    for attempt in range(1 + retries):
        stats, err = run_benchmark_once(provider_flag, model)
        if err and attempt < retries and ("429" in err or "timeout" in err):
            time.sleep(5 * (attempt + 1))
            continue
        return stats, err
    return stats, err


def check_daemon():
    """Check if gsh-daemon is running."""
    try:
        result = subprocess.run(
            ["gsh", "status"], capture_output=True, text=True, timeout=5
        )
        return "not running" not in result.stdout.lower()
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return False


def main():
    if not check_daemon():
        print("Error: gsh-daemon is not running. Start it with: gsh-daemon start")
        sys.exit(1)

    # Optional filter: python3 bench.py zai  (or: anthropic openai together ...)
    only = set(a.lower() for a in sys.argv[1:])

    print("=" * 74)
    print("  LLM Provider Benchmark (via gsh-daemon)")
    print("  Task: Implement & test a Fibonacci function")
    print("=" * 74)
    print()

    # Detect available providers
    available = []
    for name, flag, model, env_key in PROVIDERS:
        key = os.environ.get(env_key)
        if flag is None:
            print(f"  [--] {name:15s} {model:30s} (no gsh provider)")
        elif not key:
            print(f"  [--] {name:15s} {model:30s} (${env_key} not set)")
        elif only and flag.lower() not in only:
            print(f"  [--] {name:15s} {model:30s} (skipped)")
        else:
            print(f"  [ok] {name:15s} {model:30s}")
            available.append((name, flag, model))

    if not available:
        print("\nNo providers available. Set API keys and retry.")
        sys.exit(1)

    print(f"\nBenchmarking {len(available)} provider(s)...\n")
    print("-" * 74)

    results = []
    for name, flag, model in available:
        label = f"{name} ({model})"
        if len(label) > 48:
            label = label[:45] + "..."
        sys.stdout.write(f"  {label:50s}")
        sys.stdout.flush()

        stats, err = run_benchmark(flag, model)

        if err:
            print(f"ERROR: {err}")
            results.append((name, model, None, err))
        elif stats:
            print(
                f"TTFT {stats['ttft']:>6s} | "
                f"{stats['tps']:>4s} tok/s | "
                f"~{stats['tokens']} tok | "
                f"{stats['total']}"
            )
            results.append((name, model, stats, None))
        else:
            print("done (no stats captured)")
            results.append((name, model, None, None))

    # Summary table
    print()
    print("=" * 74)
    print(f"  {'Provider':15s} {'Model':25s} {'TTFT':>7s} {'Total':>7s} {'tok/s':>7s}")
    print("-" * 74)

    for name, model, stats, err in results:
        m = model if len(model) <= 25 else model[:22] + "..."
        if err:
            print(f"  {name:15s} {m:25s} {'error':>7s}")
        elif stats:
            print(
                f"  {name:15s} {m:25s} "
                f"{stats['ttft']:>7s} "
                f"{stats['total']:>7s} "
                f"{stats['tps']:>7s}"
            )
        else:
            print(f"  {name:15s} {m:25s} {'-':>7s} {'-':>7s} {'-':>7s}")

    print("=" * 74)
    print()
    print("  TTFT = time to first token. tok/s = tokens/sec during generation.")
    print("  Measured end-to-end through gsh-daemon (includes agent overhead).")


if __name__ == "__main__":
    main()
