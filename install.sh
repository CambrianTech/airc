#!/usr/bin/env bash
#
# Agent Relay installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/agent-relay/main/install.sh | bash
#
# Clones the repo, puts `relay` on PATH, symlinks skills into ~/.claude/skills/

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/agent-relay.git"
CLONE_DIR="${AGENT_RELAY_DIR:-$HOME/.agent-relay-src}"
BIN_DIR="$HOME/.local/bin"
SKILLS_TARGET="$HOME/.claude/skills"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ]; then
  info "Updating existing install"
  git -C "$CLONE_DIR" pull --ff-only --quiet
else
  info "Installing agent-relay"
  git clone --quiet "$REPO_URL" "$CLONE_DIR"
fi

# ── relay on PATH ───────────────────────────────────────────────────────

mkdir -p "$BIN_DIR"
ln -sf "$CLONE_DIR/relay" "$BIN_DIR/relay"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  # Add to shell profile automatically
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && ! grep -q 'agent-relay' "$rc"; then
      echo 'export PATH="$HOME/.local/bin:$PATH"  # agent-relay' >> "$rc"
      ok "Added ~/.local/bin to PATH in $(basename "$rc")"
      break
    fi
  done
  export PATH="$BIN_DIR:$PATH"
fi

# ── Skills into Claude Code ─────────────────────────────────────────────

if [ -d "$CLONE_DIR/skills" ]; then
  mkdir -p "$SKILLS_TARGET"
  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    [ -L "$target" ] && rm "$target"
    ln -sf "$skill_dir" "$target"
    ok "Skill: /relay:$skill_name"
  done
fi

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  relay start <your-name>    # on this machine"
echo "  relay join <name@host>     # on the other machine"
echo ""
