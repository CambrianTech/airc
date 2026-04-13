#!/usr/bin/env bash
#
# Agent Relay uninstaller
#
# Removes symlinks from ~/.claude/skills/ and ~/.local/bin/relay.
# Leaves the clone at ~/.agent-relay-src — delete it manually to fully remove.

set -euo pipefail

CLONE_DIR="${AGENT_RELAY_DIR:-$HOME/.agent-relay-src}"
BIN_DIR="$HOME/.local/bin"
SKILLS_TARGET="$HOME/.claude/skills"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }

# Remove skill symlinks
if [ -d "$CLONE_DIR/skills" ]; then
  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    if [ -L "$target" ]; then
      rm "$target"
      ok "Removed skill: $skill_name"
    fi
  done
fi

# Remove relay binary symlink
if [ -L "$BIN_DIR/relay" ]; then
  rm "$BIN_DIR/relay"
  ok "Removed relay from PATH"
fi

echo ""
ok "Uninstalled. Clone left at $CLONE_DIR (delete manually if desired)."
