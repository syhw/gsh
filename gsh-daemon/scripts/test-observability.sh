#!/bin/bash
# Test script for gsh observability features
#
# Usage:
#   ./scripts/test-observability.sh
#
# Prerequisites:
#   - Set ZAI_API_KEY or TOGETHER_API_KEY
#   - Build the daemon: cargo build

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DAEMON_DIR="$(dirname "$SCRIPT_DIR")"
SOCKET_PATH="/tmp/gsh-$(whoami).sock"
LOG_DIR="${HOME}/.local/share/gsh/logs"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}=== gsh Observability Test ===${NC}"
echo

# Check for API key
if [[ -z "$ZAI_API_KEY" ]] && [[ -z "$TOGETHER_API_KEY" ]]; then
    echo -e "${RED}Error: Set ZAI_API_KEY or TOGETHER_API_KEY${NC}"
    exit 1
fi

# Build
echo -e "${YELLOW}Building daemon...${NC}"
cd "$DAEMON_DIR"
cargo build --release 2>/dev/null

# Start daemon if not running
if [[ ! -S "$SOCKET_PATH" ]]; then
    echo -e "${YELLOW}Starting daemon...${NC}"
    cargo run --release -- start --foreground &
    DAEMON_PID=$!
    sleep 2
    trap "kill $DAEMON_PID 2>/dev/null" EXIT
else
    echo -e "${GREEN}Daemon already running${NC}"
fi

# Send test queries
echo
echo -e "${YELLOW}Sending test queries...${NC}"

# Query 1: Simple text
echo '{"type":"prompt","query":"What is 2+2? Reply briefly.","cwd":"/tmp","stream":false}' | \
    nc -U "$SOCKET_PATH"
echo

# Query 2: Bash command (tests stdout capture)
echo '{"type":"prompt","query":"Use bash to run: echo hello_stdout","cwd":"/tmp","stream":false}' | \
    nc -U "$SOCKET_PATH"
echo

# Query 3: Bash with stderr
echo '{"type":"prompt","query":"Use bash to run: echo hello_stderr >&2","cwd":"/tmp","stream":false}' | \
    nc -U "$SOCKET_PATH"
echo

# Query 4: Bash with both stdout and stderr
echo '{"type":"prompt","query":"Use bash to run: echo out && echo err >&2 && exit 1","cwd":"/tmp","stream":false}' | \
    nc -U "$SOCKET_PATH"
echo

# Show logs
echo
echo -e "${YELLOW}=== Log Events ===${NC}"
LOG_FILE=$(ls -t "$LOG_DIR"/*.jsonl 2>/dev/null | head -1)
if [[ -f "$LOG_FILE" ]]; then
    echo "Log file: $LOG_FILE"
    echo
    # Show last 10 events with pretty formatting
    tail -10 "$LOG_FILE" | while read -r line; do
        event=$(echo "$line" | jq -r '.event')
        agent=$(echo "$line" | jq -r '.agent')
        ts=$(echo "$line" | jq -r '.ts' | cut -d'T' -f2 | cut -d'.' -f1)

        case "$event" in
            prompt) echo -e "  ${GREEN}[$ts]${NC} $agent: PROMPT" ;;
            bash_exec)
                cmd=$(echo "$line" | jq -r '.command // empty' | head -c 40)
                exit_code=$(echo "$line" | jq -r '.exit_code // empty')
                stdout_len=$(echo "$line" | jq -r '.stdout // "" | length')
                stderr_len=$(echo "$line" | jq -r '.stderr // "" | length')
                echo -e "  ${YELLOW}[$ts]${NC} $agent: BASH exit=$exit_code stdout=${stdout_len}b stderr=${stderr_len}b cmd='$cmd'"
                ;;
            tool_call)
                tool=$(echo "$line" | jq -r '.tool // empty')
                echo -e "  ${YELLOW}[$ts]${NC} $agent: TOOL_CALL $tool"
                ;;
            complete) echo -e "  ${GREEN}[$ts]${NC} $agent: COMPLETE" ;;
            error)
                msg=$(echo "$line" | jq -r '.message // empty' | head -c 50)
                echo -e "  ${RED}[$ts]${NC} $agent: ERROR $msg"
                ;;
            *) echo -e "  [$ts] $agent: $event" ;;
        esac
    done

    echo
    echo -e "${YELLOW}=== Raw BashExec Events ===${NC}"
    grep '"event":"bash_exec"' "$LOG_FILE" | tail -3 | jq '.'
else
    echo -e "${RED}No log files found in $LOG_DIR${NC}"
fi

echo
echo -e "${YELLOW}=== Dashboard ===${NC}"
echo "Run: cargo run -- dashboard"
echo "  - Tab 1: Live event stream"
echo "  - Tab 2: Token usage & cost"
echo "  - Press 'q' to quit"
