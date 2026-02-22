#!/usr/bin/env bash
set -euo pipefail

# gsh installer — build, install, configure shell

GSH_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${GSH_INSTALL_BIN:-$HOME/.local/bin}"
CONFIG_DIR="$HOME/.config/gsh"
DATA_DIR="$HOME/.local/share/gsh"

# --- Helpers ---

info() { printf '\033[0;34m%s\033[0m\n' "$*"; }
ok()   { printf '\033[0;32m%s\033[0m\n' "$*"; }
err()  { printf '\033[0;31m%s\033[0m\n' "$*" >&2; }

# --- Uninstall ---

uninstall() {
    info "Uninstalling gsh..."
    [ -S "/tmp/gsh-${USER}.sock" ] && "$BIN_DIR/gsh-daemon" stop 2>/dev/null || true
    rm -f "$BIN_DIR/gsh" "$BIN_DIR/gsh-daemon"
    # Remove block from .zshrc
    if grep -qF '# >>> gsh >>>' "$HOME/.zshrc" 2>/dev/null; then
        sed -i '' '/# >>> gsh >>>/,/# <<< gsh <<</d' "$HOME/.zshrc"
        ok "Cleaned .zshrc"
    fi
    ok "Uninstalled. Config preserved at $CONFIG_DIR"
    echo "  To remove everything: rm -rf $CONFIG_DIR $DATA_DIR"
}

# --- Main ---

case "${1:-}" in
    --uninstall) uninstall; exit 0 ;;
    --help|-h)
        echo "Usage: install.sh [--uninstall | --help]"
        exit 0 ;;
esac

# Prerequisites
if ! command -v cargo &>/dev/null; then
    err "cargo not found. Install Rust: https://rustup.rs/"
    exit 1
fi

# Build
info "Building gsh..."
(cd "$GSH_DIR" && cargo build --release)

# Install binaries
mkdir -p "$BIN_DIR"
cp "$GSH_DIR/target/release/gsh" "$BIN_DIR/gsh"
cp "$GSH_DIR/target/release/gsh-daemon" "$BIN_DIR/gsh-daemon"
chmod +x "$BIN_DIR/gsh" "$BIN_DIR/gsh-daemon"
ok "Installed binaries to $BIN_DIR"

# Seed config if missing
mkdir -p "$CONFIG_DIR" "$DATA_DIR/sessions"
if [ ! -f "$CONFIG_DIR/config.toml" ]; then
    cp "$GSH_DIR/config.example.toml" "$CONFIG_DIR/config.toml"
    ok "Created $CONFIG_DIR/config.toml"
fi

# Shell integration — source plugin directly from repo, auto-start daemon
ZSHRC="$HOME/.zshrc"
[ -f "$ZSHRC" ] || touch "$ZSHRC"

# Remove old block if present, then append fresh one
if grep -qF '# >>> gsh >>>' "$ZSHRC" 2>/dev/null; then
    sed -i '' '/# >>> gsh >>>/,/# <<< gsh <<</d' "$ZSHRC"
fi

cat >> "$ZSHRC" << BLOCK
# >>> gsh >>>
[[ -f "$GSH_DIR/gsh.plugin.zsh" ]] && source "$GSH_DIR/gsh.plugin.zsh"
[[ "\$GSH_ENABLED" != "0" ]] && [[ ! -S "/tmp/gsh-\${USER}.sock" ]] && gsh-daemon start &>/dev/null
export PATH="$BIN_DIR:\$PATH"
# <<< gsh <<<
BLOCK

ok "Updated $ZSHRC"

echo ""
ok "gsh installed!"
echo "  Restart your shell or: source $ZSHRC"
echo "  Try: gsh 'what files are here?'"
