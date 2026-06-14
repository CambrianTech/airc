#!/usr/bin/env bash
#
# AIRC uninstaller — single source of truth for full removal.
#
# Direct entry:    bash ~/.airc/src/uninstall.sh
# Curl-pipe:       curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/uninstall.sh | bash -s -- --yes
# Via the verb:    airc uninstall            (preferred; just exec's this script)
#
# What it removes:
#   - the running airc daemon (via airc stop, if airc is on PATH)
#   - daemon (launchd / systemd-user / Task Scheduler) via airc daemon uninstall
#   - stale POSIX forwarders and Windows shim files under $BIN_DIR
#   - airc-owned skill directories under ~/.claude/skills/ and ~/.codex/skills/
#   - the clone itself (~/.airc/src or $AIRC_DIR)
#
# What it leaves:
#   - per-project .airc/ state in every dir you ran `airc join` from
#     (identity keys, peer records, message logs — your data, not ours)
#   - gh auth, brew/apt-installed packages (gh / cargo / openssh)
#   - other agents' configs (Codex / Cursor / opencode / etc.)
#
# Flags:
#   --yes / -y     skip the confirmation prompt (required for curl|bash)
#   --purge        also print the list of per-project .airc/ dirs to remove manually
#   --help / -h    this message
#
# AIRC_DIR env var overrides the clone location (default $HOME/.airc/src).

set -euo pipefail

CLONE_DIR="${AIRC_DIR:-$HOME/.airc/src}"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SKILLS_TARGET="${SKILLS_TARGET:-$HOME/.claude/skills}"

ASSUME_YES=0
PURGE=0
for arg in "$@"; do
  case "$arg" in
    -y|--yes)   ASSUME_YES=1 ;;
    --purge)    PURGE=1 ;;
    -h|--help)
      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *)
      echo "ERROR: unknown flag: $arg" >&2
      echo "Try: bash $0 --help" >&2
      exit 2 ;;
  esac
done

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn()  { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }

# Move out of the clone before we start, so `rm -rf $CLONE_DIR` doesn't
# leave us with a no-longer-existent cwd (which breaks every command after).
cd "$HOME" 2>/dev/null || cd /

cat <<EOF
This will remove airc from this machine:
  PATH files        $BIN_DIR/{airc,airc.exe} plus legacy $BIN_DIR/{airc-core,airc-core.exe,airc.cmd,airc.ps1}
  skill dirs        $SKILLS_TARGET/<airc-skills>/ + ~/.codex/skills/<airc-skills>/ (if Codex installed)
  install dir       $CLONE_DIR
  daemon            launchd / systemd-user / Task Scheduler unit (if installed)
  running daemon    airc stop (if airc is on PATH)

It will NOT remove:
  per-project .airc/ state in every dir you ran 'airc join' from
  gh auth, brew/apt packages (gh / cargo / openssh)
  other agents' configs

EOF

if [ "$ASSUME_YES" != "1" ]; then
  if [ ! -t 0 ]; then
    warn "Non-interactive run: pass --yes to confirm uninstall."
    exit 1
  fi
  printf "Type 'yes' to proceed: "
  read -r reply
  if [ "$reply" != "yes" ]; then
    info "Aborted."
    exit 0
  fi
fi

# 1. Stop the running daemon. `airc stop` asks the default-home daemon to
# shut down gracefully (removes its daemon.pid on exit); idempotent — a
# no-op exit is consumed by the `if`, so it never aborts the uninstall.
if command -v airc >/dev/null 2>&1; then
  if airc stop >/dev/null 2>&1; then
    ok "Stopped the running airc daemon (airc stop)"
  fi
  # 2. Uninstall daemon. No-op if not installed; we don't gate on a status
  # check because `airc daemon uninstall` already handles the absent case.
  if airc daemon uninstall >/dev/null 2>&1; then
    ok "Removed daemon (launchd / systemd-user / Task Scheduler)"
  fi
fi

# 3. Skill dirs. Walk every entry in the skills dir and drop any airc-owned
# copied skill directory. Also remove older symlinks that resolve into the
# clone so upgrades converge cleanly.
_remove_airc_owned_skills() {
  local skills_dir="$1"
  local removed=0 entry target
  [ -d "$skills_dir" ] || { echo 0; return; }
  for entry in "$skills_dir"/*; do
    if [ -L "$entry" ]; then
      target="$(readlink "$entry" 2>/dev/null || true)"
      case "$target" in
        "$CLONE_DIR"/*|"$CLONE_DIR")
          rm -f "$entry"
          removed=$((removed + 1)) ;;
      esac
    elif [ -d "$entry" ] && [ -f "$entry/.airc-skill" ]; then
      rm -rf "$entry"
      removed=$((removed + 1))
    fi
  done
  echo "$removed"
}
removed_skills_claude=$(_remove_airc_owned_skills "$SKILLS_TARGET")
removed_skills_codex=$(_remove_airc_owned_skills "${CODEX_SKILLS_TARGET:-$HOME/.codex/skills}")
[ "$removed_skills_claude" -gt 0 ] && ok "Removed $removed_skills_claude airc skill dir(s) from $SKILLS_TARGET"
[ "$removed_skills_codex"  -gt 0 ] && ok "Removed $removed_skills_codex airc skill dir(s) from ${CODEX_SKILLS_TARGET:-$HOME/.codex/skills}"

# 3b. Codex config.toml cleanup. Strip the airc-managed GH_TOKEN block
# (and the network-permission profile) if present. Keeps the rest of
# the user's Codex config untouched. Marker-bracketed for safe sed delete.
codex_config="$HOME/.codex/config.toml"
if [ -f "$codex_config" ]; then
  if grep -qF "AIRC-GH-TOKEN-START" "$codex_config" 2>/dev/null; then
    _tmp=$(mktemp)
    sed '/^# AIRC-GH-TOKEN-START/,/^# AIRC-GH-TOKEN-END/d' "$codex_config" > "$_tmp"
    mv "$_tmp" "$codex_config"
    ok "Removed airc GH_TOKEN injection from $codex_config"
  fi
  if grep -qF "AIRC-COMMAND-RULES-START" "$codex_config" 2>/dev/null; then
    _tmp=$(mktemp)
    sed '/^# AIRC-COMMAND-RULES-START/,/^# AIRC-COMMAND-RULES-END/d' "$codex_config" > "$_tmp"
    mv "$_tmp" "$codex_config"
    ok "Removed airc command-rules pre-approval from $codex_config"
  fi
  _airc=""
  if command -v airc >/dev/null 2>&1; then
    _airc=$(command -v airc)
  elif [ -x "$CLONE_DIR/target/release/airc" ]; then
    _airc="$CLONE_DIR/target/release/airc"
  elif [ -x "$CLONE_DIR/target/debug/airc" ]; then
    _airc="$CLONE_DIR/target/debug/airc"
  fi
  if [ -n "$_airc" ]; then
    if out=$("$_airc" codex-hook uninstall-hooks --codex-home "$HOME/.codex" 2>&1); then
      if [ -n "$out" ]; then
        printf '%s\n' "$out" | while IFS= read -r line; do
          ok "Codex AIRC hook: $line"
        done
      fi
    else
      warn "Could not uninstall Codex AIRC hook through airc: $out"
    fi
  fi
fi

# 4. Stale POSIX forwarders and Windows shim files on PATH.
removed_bins=0
for f in airc airc-core airc-core.exe airc.cmd airc.ps1; do
  if [ -L "$BIN_DIR/$f" ] || [ -f "$BIN_DIR/$f" ]; then
    # Symlinks: drop unconditionally (we own them).
    # Real files (airc.cmd / airc.ps1 on Windows): drop only if their
    # contents reference the clone, so we don't blow away an unrelated
    # binary a user happens to have at the same name.
    if [ -L "$BIN_DIR/$f" ]; then
      rm -f "$BIN_DIR/$f"
      removed_bins=$((removed_bins + 1))
    elif grep -q "$CLONE_DIR" "$BIN_DIR/$f" 2>/dev/null; then
      rm -f "$BIN_DIR/$f"
      removed_bins=$((removed_bins + 1))
    fi
  fi
done
[ "$removed_bins" -gt 0 ] && ok "Removed $removed_bins stale PATH file(s) from $BIN_DIR"

# 5. Clone dir. Last, since the steps above call into airc + read
# from the clone for the skill walk. Once this runs, `airc` is gone.
if [ -d "$CLONE_DIR" ]; then
  rm -rf "$CLONE_DIR"
  ok "Removed install dir: $CLONE_DIR"
fi

echo ""
ok "Uninstalled."
echo ""

if [ "$PURGE" = "1" ]; then
  echo "  --purge: per-project state to remove manually if you want a fully clean machine:"
  echo ""
  # Find .airc dirs under common project roots without scanning the whole
  # filesystem. Stop at depth 6 to avoid runaway descent into node_modules
  # / vendor trees.
  found_any=0
  for root in "$HOME/Development" "$HOME/Projects" "$HOME/work" "$HOME/src" "$HOME"; do
    [ -d "$root" ] || continue
    while IFS= read -r d; do
      echo "    rm -rf $d"
      found_any=1
    done < <(find "$root" -maxdepth 6 -type d -name ".airc" 2>/dev/null)
  done
  if [ "$found_any" = "0" ]; then
    echo "    (none found under \$HOME/{Development,Projects,work,src})"
  fi
  echo ""
  echo "  These hold your identity keys, peer records, and chat logs. Delete only if"
  echo "  you actually want them gone — they don't take much space and are useful for"
  echo "  recovery if you reinstall."
  echo ""
fi
