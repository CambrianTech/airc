#!/usr/bin/env bash
#
# bootstrap-airc.sh -- cold install + first-time setup + room join in one command
#
# Usage:
#   ./bootstrap-airc.sh [mnemonic-or-gist-id]
#   curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/canary/bootstrap-airc.sh \
#     | bash -s -- [mnemonic-or-gist-id]
#
# What it does:
#   1. Runs install.sh if airc isn't already on PATH (handles prereqs +
#      symlinks the binary into ~/.local/bin).
#   2. Runs `airc doctor --connect` to verify the env can pair (catches
#      Tailscale-down / gh-missing / network-out before they silently fail).
#   3. Walks gh auth if not already done.
#   4. Joins a room: with the mnemonic-or-gist-id argument if given,
#      otherwise auto-scope from the current git repo (or #general).
#   5. Sets a default identity if pronouns are still unset.
#   6. Prints a final whois + next-step hints.
#
# Designed for first-time users (especially first-EXTERNAL users like
# Toby) so the path from "got the SMS with a 4-word phrase" to "in the
# room" is a single command, not seven.
#
# Issue #81. Pairs with bootstrap-airc.ps1 for Windows native.

set -euo pipefail

MNEMONIC="${1:-}"

step() { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn() { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }
die()  { printf '\n\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }

# 1. install if not present
if ! command -v airc >/dev/null 2>&1; then
  step "airc not on PATH -- running installer (canary channel)"
  curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/canary/install.sh | bash
  # Pick up the freshly-installed binary in this same session.
  export PATH="$HOME/.local/bin:$PATH"
  if ! command -v airc >/dev/null 2>&1; then
    die "airc still not on PATH after install. Add ~/.local/bin to PATH and re-run."
  fi
  ok "airc installed: $(command -v airc)"
else
  ok "airc already on PATH: $(command -v airc)"
fi

# 2. pre-flight (catches Tailscale-down, gh-missing, network-out, etc.)
step "Pre-flight: airc doctor --connect"
if ! airc doctor --connect; then
  die "Pre-flight failed. Fix the items above, then re-run this script."
fi

# 3. gh auth if needed
if ! gh auth status >/dev/null 2>&1; then
  step "Authenticating gh (need 'gist' scope for room substrate)"
  gh auth login -s gist
fi

# 4. join the room
if [ -n "$MNEMONIC" ]; then
  step "Joining room via mnemonic / gist-id: $MNEMONIC"
  airc join "$MNEMONIC"
else
  step "Joining auto-scoped room (no mnemonic given -- using git remote org or #general)"
  airc join
fi

# Give the pair handshake a moment to settle before identity check.
sleep 1

# 5. set default identity if unset
if airc identity show 2>/dev/null | grep -qE 'pronouns: *\(unset\)'; then
  step "Setting default identity (override later with: airc identity set ...)"
  airc identity set \
    --pronouns it \
    --role onboarded-via-bootstrap \
    --bio "Joined via bootstrap-airc.sh"
fi

# 6. final summary
echo ""
ok "Bootstrap complete. Your airc identity:"
echo ""
airc whois 2>&1 | sed 's/^/    /'
echo ""
ok "Next steps:"
cat <<'EOF'
    airc msg "hello room"           # broadcast to your room
    airc msg @<peer> "hi"           # DM a peer
    airc peers                      # list paired peers
    airc whois <peer>               # see another peer's identity
    airc list                       # see all rooms on your gh account
    airc help                       # full command list
EOF
echo ""
