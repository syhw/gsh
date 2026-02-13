#!/usr/bin/env bash
set -euo pipefail

# gsh installer — builds, installs, and configures the agentic shell

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Install paths (overridable via env vars)
INSTALL_BIN="${GSH_INSTALL_BIN:-$HOME/.local/bin}"
INSTALL_SHARE="${GSH_INSTALL_SHARE:-$HOME/.local/share/gsh}"
INSTALL_CONFIG="${GSH_INSTALL_CONFIG:-$HOME/.config/gsh}"

# Idempotency markers
MARKER_BEGIN="# >>> gsh >>>"
MARKER_END="# <<< gsh <<<"

# Colors
if [ -t 1 ]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
    BLUE='\033[0;34m'; BOLD='\033[1m'; NC='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; BLUE=''; BOLD=''; NC=''
fi

info()  { printf "${BLUE}%s${NC}\n" "$*"; }
ok()    { printf "${GREEN}%s${NC}\n" "$*"; }
warn()  { printf "${YELLOW}%s${NC}\n" "$*"; }
err()   { printf "${RED}%s${NC}\n" "$*" >&2; }

# --- Prerequisites ---

check_prerequisites() {
    if ! command -v cargo &>/dev/null; then
        err "Error: cargo not found. Install Rust: https://rustup.rs/"
        exit 1
    fi

    if ! command -v socat &>/dev/null; then
        warn "Warning: socat not found. Shell hooks require it."
        echo "  macOS:  brew install socat"
        echo "  Linux:  apt install socat"
    fi

    if ! command -v tmux &>/dev/null; then
        warn "Warning: tmux not found. Subagent features require it."
    fi
}

# --- Build ---

build_binaries() {
    info "Building gsh (release)..."
    (cd "$SCRIPT_DIR" && cargo build --release)
    ok "Build complete."
}

# --- Install binaries ---

install_binaries() {
    info "Installing binaries to ${INSTALL_BIN}..."
    mkdir -p "$INSTALL_BIN"

    cp "$SCRIPT_DIR/target/release/gsh" "$INSTALL_BIN/gsh"
    cp "$SCRIPT_DIR/target/release/gsh-daemon" "$INSTALL_BIN/gsh-daemon"
    chmod +x "$INSTALL_BIN/gsh" "$INSTALL_BIN/gsh-daemon"

    ok "  ${INSTALL_BIN}/gsh"
    ok "  ${INSTALL_BIN}/gsh-daemon"
}

# --- Install plugin & data files ---

install_data_files() {
    info "Installing plugin and data files..."
    mkdir -p "$INSTALL_SHARE"

    cp "$SCRIPT_DIR/gsh.plugin.zsh" "$INSTALL_SHARE/gsh.plugin.zsh"
    ok "  Plugin: ${INSTALL_SHARE}/gsh.plugin.zsh"

    # Example flows (don't overwrite user-modified copies)
    mkdir -p "$INSTALL_CONFIG/flows"
    for f in "$SCRIPT_DIR"/examples/flows/*.toml; do
        [ -f "$f" ] || continue
        local base
        base="$(basename "$f")"
        if [ ! -f "$INSTALL_CONFIG/flows/$base" ]; then
            cp "$f" "$INSTALL_CONFIG/flows/$base"
            echo "  Flow: $base"
        fi
    done
}

# --- Config ---

install_config() {
    info "Setting up configuration..."
    mkdir -p "$INSTALL_CONFIG"
    mkdir -p "$HOME/.local/share/gsh/sessions"

    if [ ! -f "$INSTALL_CONFIG/config.toml" ]; then
        cp "$SCRIPT_DIR/config.example.toml" "$INSTALL_CONFIG/config.toml"
        ok "  Created ${INSTALL_CONFIG}/config.toml"
        warn "  Edit this file to add your API keys."
    else
        echo "  Config already exists, skipping."
    fi
}

# --- Shell detection ---

detect_shell() {
    basename "${SHELL:-/bin/bash}"
}

get_rc_file() {
    local shell_name="$1"
    case "$shell_name" in
        zsh)  echo "$HOME/.zshrc" ;;
        bash)
            if [ -f "$HOME/.bashrc" ]; then
                echo "$HOME/.bashrc"
            elif [ "$(uname)" = "Darwin" ]; then
                echo "$HOME/.bash_profile"
            else
                echo "$HOME/.bashrc"
            fi
            ;;
        *)    echo "" ;;
    esac
}

# --- PATH check ---

path_includes_install_bin() {
    case ":$PATH:" in
        *":${INSTALL_BIN}:"*) return 0 ;;
        *)                    return 1 ;;
    esac
}

# --- RC block generators ---

generate_zsh_block() {
    local need_path="$1"
    echo "$MARKER_BEGIN"
    echo "# gsh - Agentic Shell (installed $(date +%Y-%m-%d))"
    if [ "$need_path" = "yes" ]; then
        # Use single quotes so $HOME is expanded at shell startup, not now
        echo 'export PATH="$HOME/.local/bin:$PATH"'
    fi
    echo "source \"${INSTALL_SHARE}/gsh.plugin.zsh\""
    cat <<'BLOCK'
# Auto-start daemon if not running
if [[ "$GSH_ENABLED" != "0" ]] && ! [[ -S "/tmp/gsh-${USER}.sock" ]]; then
    gsh-daemon start &>/dev/null
fi
BLOCK
    echo "$MARKER_END"
}

generate_bash_block() {
    local need_path="$1"
    echo "$MARKER_BEGIN"
    echo "# gsh - Agentic Shell (installed $(date +%Y-%m-%d))"
    if [ "$need_path" = "yes" ]; then
        echo 'export PATH="$HOME/.local/bin:$PATH"'
    fi
    cat <<'BLOCK'
# Auto-start daemon if not running
if [ "${GSH_ENABLED:-1}" != "0" ] && ! [ -S "/tmp/gsh-${USER}.sock" ]; then
    gsh-daemon start &>/dev/null 2>&1
fi
BLOCK
    echo "$MARKER_END"
}

# --- Inject into rc file (idempotent) ---

inject_rc_block() {
    local rc_file="$1"
    local block="$2"

    [ -f "$rc_file" ] || touch "$rc_file"

    # Remove existing block if present
    if grep -qF "$MARKER_BEGIN" "$rc_file" 2>/dev/null; then
        local tmp
        tmp="$(mktemp)"
        sed "/${MARKER_BEGIN}/,/${MARKER_END}/d" "$rc_file" > "$tmp"
        mv "$tmp" "$rc_file"
    fi

    # Append new block
    printf '\n%s\n' "$block" >> "$rc_file"
    ok "  Updated $rc_file"
}

# --- Uninstall ---

uninstall() {
    info "Uninstalling gsh..."

    # Stop daemon
    if [ -S "/tmp/gsh-${USER}.sock" ]; then
        echo "  Stopping daemon..."
        "$INSTALL_BIN/gsh-daemon" stop 2>/dev/null || true
    fi

    rm -f "$INSTALL_BIN/gsh" "$INSTALL_BIN/gsh-daemon"
    echo "  Removed binaries"

    rm -f "$INSTALL_SHARE/gsh.plugin.zsh"
    echo "  Removed plugin"

    # Remove rc blocks from all possible rc files
    for rc in "$HOME/.zshrc" "$HOME/.bashrc" "$HOME/.bash_profile"; do
        if [ -f "$rc" ] && grep -qF "$MARKER_BEGIN" "$rc" 2>/dev/null; then
            local tmp
            tmp="$(mktemp)"
            sed "/${MARKER_BEGIN}/,/${MARKER_END}/d" "$rc" > "$tmp"
            mv "$tmp" "$rc"
            echo "  Cleaned $rc"
        fi
    done

    echo ""
    ok "gsh uninstalled."
    echo "  Config preserved at: $INSTALL_CONFIG"
    echo "  Data preserved at:   $HOME/.local/share/gsh"
    echo "  To remove everything: rm -rf $INSTALL_CONFIG $HOME/.local/share/gsh"
}

# --- Main ---

main() {
    printf "${BOLD}gsh installer${NC}\n\n"

    local do_uninstall=false
    local skip_build=false

    for arg in "$@"; do
        case "$arg" in
            --uninstall)  do_uninstall=true ;;
            --skip-build) skip_build=true ;;
            --help|-h)
                echo "Usage: install.sh [OPTIONS]"
                echo ""
                echo "Options:"
                echo "  --skip-build    Skip cargo build (use existing binaries)"
                echo "  --uninstall     Remove gsh installation"
                echo "  --help, -h      Show this help"
                exit 0
                ;;
            *)
                err "Unknown option: $arg"
                exit 1
                ;;
        esac
    done

    if [ "$do_uninstall" = true ]; then
        uninstall
        exit 0
    fi

    check_prerequisites

    if [ "$skip_build" = false ]; then
        build_binaries
    fi

    install_binaries
    install_data_files
    install_config

    # Shell integration
    local shell_name
    shell_name="$(detect_shell)"
    local rc_file
    rc_file="$(get_rc_file "$shell_name")"

    if [ -z "$rc_file" ]; then
        warn "Unknown shell: $shell_name — skipping shell integration."
        echo "  Add ${INSTALL_BIN} to your PATH manually."
        echo "  For zsh: source ${INSTALL_SHARE}/gsh.plugin.zsh"
    else
        local need_path="no"
        if ! path_includes_install_bin; then
            need_path="yes"
        fi

        local block
        case "$shell_name" in
            zsh)  block="$(generate_zsh_block "$need_path")" ;;
            bash) block="$(generate_bash_block "$need_path")" ;;
            *)    block="" ;;
        esac

        if [ -n "$block" ]; then
            inject_rc_block "$rc_file" "$block"
        fi
    fi

    echo ""
    printf "${GREEN}${BOLD}gsh installed successfully!${NC}\n"
    echo ""
    echo "Next steps:"
    echo "  1. Restart your shell or run:  source $rc_file"
    echo "  2. Set your API key:"
    echo "       export ANTHROPIC_API_KEY='sk-ant-...'"
    echo "     or edit ~/.config/gsh/config.toml"
    echo "  3. Try it:  gsh 'what files are here?'"
    echo ""
}

main "$@"
