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
#      puts the installed binary on PATH).
#   2. Shows GitHub device authorization and waits if approval is needed.
#   3. Verifies health after join provisions the daemon and identity.
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

# 1. Reconcile installation on every run, including previously failed stages.
{
  step "Running the repeatable AIRC installer"
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
  if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ] &&
     [ -f "$script_dir/Cargo.toml" ] && [ -f "$script_dir/install.sh" ]; then
    bash "$script_dir/install.sh"
  else
    curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/canary/install.sh | bash
  fi
  # Pick up the freshly-installed binary in this same session.
  export PATH="${BIN_DIR:-$HOME/.local/bin}:$PATH"
  if ! command -v airc >/dev/null 2>&1; then
    die "Installer returned success but airc is unavailable. Rerun setup to repair it."
  fi
  ok "airc installed: $(command -v airc)"
}

# Same consent stage as both installers; reuse approval and verify identity.
source_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-.}")" && pwd)"
if [ ! -f "$source_dir/setup/github-auth.sh" ]; then
  source_dir="$(cat "$HOME/.airc/install-source" 2>/dev/null || true)"
  if command -v cygpath >/dev/null 2>&1; then source_dir="$(cygpath -u "$source_dir")"; fi
fi
if [ -f "$source_dir/setup/github-auth.sh" ]; then
  bash "$source_dir/setup/github-auth.sh"
else
  # Older installed binaries may have no new setup helpers. Acquire this
  # bootstrap's stage automatically without changing their source branch.
  auth_stage="$(mktemp)"
  trap 'rm -f "$auth_stage"' EXIT
  curl -fsSL "https://raw.githubusercontent.com/CambrianTech/airc/${AIRC_CHANNEL:-canary}/setup/github-auth.sh" -o "$auth_stage"
  bash "$auth_stage"
  rm -f "$auth_stage"
  trap - EXIT
fi
# 4. join the room
if [ -n "$MNEMONIC" ]; then
  step "Joining room via mnemonic / gist-id: $MNEMONIC"
  AIRC_NO_ATTACH=1 airc join "$MNEMONIC"
else
  step "Joining auto-scoped room (no mnemonic given -- using git remote org or #general)"
  AIRC_NO_ATTACH=1 airc join
fi

step "Verifying the joined mesh: airc doctor --health"
airc doctor --health || die "Mesh health verification failed; setup is not complete."

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
    airc room                       # inspect current room
    airc help                       # full command list
EOF
echo ""
