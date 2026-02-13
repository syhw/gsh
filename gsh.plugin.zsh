#!/usr/bin/env zsh
# gsh.plugin.zsh - Zsh plugin for gsh (Agentic Shell)
#
# This plugin hooks into your shell to capture context and provides
# shell hooks for the gsh daemon.

# Configuration
GSH_SOCKET="${GSH_SOCKET:-/tmp/gsh-${USER}.sock}"
GSH_DAEMON_BIN="${GSH_DAEMON_BIN:-gsh-daemon}"
GSH_CLI_BIN="${GSH_CLI_BIN:-gsh}"
GSH_ENABLED="${GSH_ENABLED:-1}"

# Internal state
typeset -g _gsh_cmd_start_time
typeset -g _gsh_last_cwd

# Check if daemon is running
_gsh_daemon_running() {
    [[ -S "$GSH_SOCKET" ]] && timeout 1 socat - "UNIX-CONNECT:$GSH_SOCKET" <<< '{"type":"ping"}' &>/dev/null
}

# Send a message to the daemon (fire-and-forget)
_gsh_send() {
    if [[ ! -S "$GSH_SOCKET" ]]; then
        return 1
    fi

    # Use socat if available, fall back to nc
    if command -v socat &>/dev/null; then
        echo "$1" | socat -t2 - "UNIX-CONNECT:$GSH_SOCKET" &>/dev/null &!
    elif command -v nc &>/dev/null; then
        echo "$1" | nc -U "$GSH_SOCKET" &>/dev/null &!
    fi
}

# Hook: Before command execution (preexec)
_gsh_preexec() {
    [[ "$GSH_ENABLED" != "1" ]] && return

    local cmd="$1"
    local cwd="${PWD}"
    local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    # Record start time for duration calculation
    _gsh_cmd_start_time=$EPOCHREALTIME

    # Escape the command for JSON
    local escaped_cmd=$(printf '%s' "$cmd" | sed 's/\\/\\\\/g; s/"/\\"/g; s/\t/\\t/g; s/\r/\\r/g; s/$/\\n/' | tr -d '\n' | sed 's/\\n$//')
    local escaped_cwd=$(printf '%s' "$cwd" | sed 's/\\/\\\\/g; s/"/\\"/g')

    local msg="{\"type\":\"preexec\",\"command\":\"${escaped_cmd}\",\"cwd\":\"${escaped_cwd}\",\"timestamp\":\"${timestamp}\"}"
    _gsh_send "$msg"
}

# Hook: After command execution (precmd)
_gsh_precmd() {
    local exit_code=$?

    [[ "$GSH_ENABLED" != "1" ]] && return

    local cwd="${PWD}"
    local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
    local duration_ms=""

    # Calculate duration if we have a start time
    if [[ -n "$_gsh_cmd_start_time" ]]; then
        local end_time=$EPOCHREALTIME
        local duration=$(( (end_time - _gsh_cmd_start_time) * 1000 ))
        duration_ms=$(printf '%.0f' "$duration")
        _gsh_cmd_start_time=""
    fi

    local escaped_cwd=$(printf '%s' "$cwd" | sed 's/\\/\\\\/g; s/"/\\"/g')

    local msg
    if [[ -n "$duration_ms" ]]; then
        msg="{\"type\":\"postcmd\",\"exit_code\":${exit_code},\"cwd\":\"${escaped_cwd}\",\"duration_ms\":${duration_ms},\"timestamp\":\"${timestamp}\"}"
    else
        msg="{\"type\":\"postcmd\",\"exit_code\":${exit_code},\"cwd\":\"${escaped_cwd}\",\"duration_ms\":null,\"timestamp\":\"${timestamp}\"}"
    fi
    _gsh_send "$msg"
}

# Hook: Directory change (chpwd)
_gsh_chpwd() {
    [[ "$GSH_ENABLED" != "1" ]] && return

    local old_cwd="${_gsh_last_cwd:-$HOME}"
    local new_cwd="${PWD}"
    local timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")

    # Update last cwd
    _gsh_last_cwd="$new_cwd"

    local escaped_old=$(printf '%s' "$old_cwd" | sed 's/\\/\\\\/g; s/"/\\"/g')
    local escaped_new=$(printf '%s' "$new_cwd" | sed 's/\\/\\\\/g; s/"/\\"/g')

    local msg="{\"type\":\"chpwd\",\"old_cwd\":\"${escaped_old}\",\"new_cwd\":\"${escaped_new}\",\"timestamp\":\"${timestamp}\"}"
    _gsh_send "$msg"
}

# List running subagents
gsh-agents() {
    if [[ ! -S "$GSH_SOCKET" ]]; then
        echo "Error: gsh daemon not running"
        return 1
    fi

    local response
    if command -v socat &>/dev/null; then
        response=$(echo '{"type":"list_agents"}' | socat -t2 - "UNIX-CONNECT:$GSH_SOCKET" 2>/dev/null)
    elif command -v nc &>/dev/null; then
        response=$(echo '{"type":"list_agents"}' | nc -U "$GSH_SOCKET" 2>/dev/null)
    else
        echo "Error: socat or nc required"
        return 1
    fi

    if [[ -z "$response" ]]; then
        echo "No running agents"
        return 0
    fi

    echo "$response" | python3 -c "
import json, sys
try:
    data = json.loads(sys.stdin.read())
    if data.get('type') == 'agent_list':
        agents = data.get('agents', [])
        if not agents:
            print('No running agents')
        else:
            print(f'Running agents ({len(agents)}):')
            for a in agents:
                task = a.get('task', 'unknown')[:40]
                print(f\"  [{a['agent_id']:04d}] {a['session_name']} - {task}\")
    elif data.get('type') == 'error':
        print(f\"Error: {data.get('message', 'unknown')}\")
except Exception as e:
    print(f'Parse error: {e}')
" 2>/dev/null || echo "$response"
}

# Attach to a subagent's tmux session
gsh-attach() {
    if [[ -z "$1" ]]; then
        echo "Usage: gsh-attach <agent-id or session-name>"
        return 1
    fi

    local target="$1"

    # If it's a number, convert to session name
    if [[ "$target" =~ ^[0-9]+$ ]]; then
        target="gsh-agent-$(printf '%04d' "$target")"
    fi

    if tmux has-session -t "$target" 2>/dev/null; then
        tmux attach-session -t "$target"
    else
        echo "Session not found: $target"
        echo "Available gsh sessions:"
        tmux list-sessions 2>/dev/null | grep "^gsh-" || echo "  (none)"
        return 1
    fi
}

# Tail a subagent's output
gsh-logs() {
    if [[ -z "$1" ]]; then
        echo "Usage: gsh-logs <agent-id or session-name>"
        return 1
    fi

    local target="$1"

    if [[ "$target" =~ ^[0-9]+$ ]]; then
        target="gsh-agent-$(printf '%04d' "$target")"
    fi

    if tmux has-session -t "$target" 2>/dev/null; then
        tmux capture-pane -t "$target" -p -S -
    else
        echo "Session not found: $target"
        return 1
    fi
}

# Kill a subagent
gsh-kill() {
    if [[ -z "$1" ]]; then
        echo "Usage: gsh-kill <agent-id>"
        return 1
    fi

    local agent_id="$1"

    if [[ ! -S "$GSH_SOCKET" ]]; then
        echo "Error: gsh daemon not running"
        return 1
    fi

    local response
    if command -v socat &>/dev/null; then
        response=$(echo "{\"type\":\"kill_agent\",\"agent_id\":$agent_id}" | socat -t2 - "UNIX-CONNECT:$GSH_SOCKET" 2>/dev/null)
    fi

    echo "Sent kill request for agent $agent_id"
}

# Alias gsh to the CLI binary
alias gsh="$GSH_CLI_BIN"

# Start daemon helper
gsh-start() {
    if _gsh_daemon_running; then
        echo "gsh daemon is already running"
        return 0
    fi

    if ! command -v "$GSH_DAEMON_BIN" &>/dev/null; then
        echo "Error: gsh-daemon not found. Install it with: cargo install --path gsh-daemon"
        return 1
    fi

    echo "Starting gsh daemon..."
    "$GSH_DAEMON_BIN" start

    # Wait for socket to appear
    local i=0
    while [[ ! -S "$GSH_SOCKET" ]] && [[ $i -lt 20 ]]; do
        sleep 0.1
        ((i++))
    done

    if [[ -S "$GSH_SOCKET" ]]; then
        echo "gsh daemon started (socket: $GSH_SOCKET)"
    else
        echo "Warning: Daemon may have failed to start (check ~/.local/share/gsh/daemon.log)"
    fi
}

# Stop daemon helper
gsh-stop() {
    if ! _gsh_daemon_running; then
        echo "gsh daemon is not running"
        return 0
    fi

    "$GSH_CLI_BIN" stop
}

# Restart daemon helper
gsh-restart() {
    gsh-stop
    sleep 0.5
    gsh-start
}

# Enable/disable context tracking
gsh-enable() {
    GSH_ENABLED=1
    echo "gsh context tracking enabled"
}

gsh-disable() {
    GSH_ENABLED=0
    echo "gsh context tracking disabled"
}

# Register hooks
autoload -Uz add-zsh-hook
add-zsh-hook preexec _gsh_preexec
add-zsh-hook precmd _gsh_precmd
add-zsh-hook chpwd _gsh_chpwd

# Initialize last cwd
_gsh_last_cwd="${PWD}"

# Print a subtle message on load
if [[ "$GSH_QUIET" != "1" ]]; then
    print -P "%F{8}gsh plugin loaded%f"
fi
