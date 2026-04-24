#!/usr/bin/env bash
#
# AIRC installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
#
# Clones the repo, puts `airc` on PATH, symlinks skills into ~/.claude/skills/

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/airc.git"
CLONE_DIR="${AIRC_DIR:-$HOME/.airc-src}"
# BIN_DIR + SKILLS_TARGET respect env-var overrides so test harnesses
# (and packagers, distros, etc.) can point install.sh at a sandbox
# instead of stomping ~/.local/bin and ~/.claude/skills. Pre-fix, a
# test passing BIN_DIR=/tmp/foo would be silently ignored and the
# real ~/.local/bin/airc symlink would get rewritten to point at the
# test dir — caught when our own canary test corrupted the real install.
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SKILLS_TARGET="${SKILLS_TARGET:-$HOME/.claude/skills}"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ]; then
  info "Updating existing install"
  # Recovery: if the install dir is on a non-channel branch (e.g. someone
  # / some AI checked out a feature branch for testing and forgot to
  # switch back), the ff-pull below fails with cryptic "Not possible to
  # fast-forward". Worse, the user can't escape via `airc canary` if
  # they're on a pre-channels binary — `canary` is an unknown command
  # there. So install.sh itself takes responsibility: detect non-channel
  # branches + auto-switch to the saved channel (or main) before pulling.
  CURRENT_BRANCH=$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
  SAVED_CHANNEL=""
  [ -f "$CLONE_DIR/.channel" ] && SAVED_CHANNEL=$(tr -d '[:space:]' < "$CLONE_DIR/.channel")
  TARGET_BRANCH="${SAVED_CHANNEL:-main}"
  case "$CURRENT_BRANCH" in
    main|canary)
      # On a known channel — leave it alone unless the saved channel
      # disagrees (e.g. user just `airc channel canary`'d but didn't
      # update yet).
      if [ -n "$SAVED_CHANNEL" ] && [ "$SAVED_CHANNEL" != "$CURRENT_BRANCH" ]; then
        info "Saved channel '$SAVED_CHANNEL' differs from current branch '$CURRENT_BRANCH' — switching"
        git -C "$CLONE_DIR" fetch --quiet origin "$SAVED_CHANNEL"
        git -C "$CLONE_DIR" checkout -q "$SAVED_CHANNEL" \
          || git -C "$CLONE_DIR" checkout -q -B "$SAVED_CHANNEL" "origin/$SAVED_CHANNEL"
      fi
      ;;
    *)
      info "Install dir on '$CURRENT_BRANCH' (not a known channel) — switching to '$TARGET_BRANCH'"
      git -C "$CLONE_DIR" fetch --quiet origin "$TARGET_BRANCH" || {
        echo "ERROR: Couldn't fetch origin/$TARGET_BRANCH. Network? gh auth?" >&2
        exit 1
      }
      git -C "$CLONE_DIR" checkout -q "$TARGET_BRANCH" \
        || git -C "$CLONE_DIR" checkout -q -B "$TARGET_BRANCH" "origin/$TARGET_BRANCH" \
        || {
          cat >&2 <<EOF
ERROR: Couldn't switch $CLONE_DIR to '$TARGET_BRANCH'.
Recover manually:
  cd $CLONE_DIR
  git fetch origin
  git status               # see why checkout was blocked
  git stash                # if you have local edits worth keeping
  git checkout $TARGET_BRANCH
  git pull --ff-only
  bash install.sh
EOF
          exit 1
        }
      ;;
  esac
  if ! git -C "$CLONE_DIR" pull --ff-only --quiet 2>&1; then
    cat >&2 <<EOF
ERROR: Couldn't fast-forward $CLONE_DIR (currently on $(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)).
Likely cause: local edits or a divergent history.
Recover with:
  cd $CLONE_DIR
  git status
  git stash               # if you have local edits worth keeping
  git fetch origin
  git reset --hard origin/$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)
  bash install.sh
EOF
    exit 1
  fi
else
  info "Installing AIRC"
  git clone --quiet "$REPO_URL" "$CLONE_DIR"
fi

# ── airc on PATH ───────────────────────────────────────────────────────

mkdir -p "$BIN_DIR"
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/airc"
# Back-compat: `relay` still works for muscle-memory and stale docs.
# The airc binary detects the invocation name and behaves identically.
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/relay"

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && ! grep -q 'airc' "$rc"; then
      echo 'export PATH="$HOME/.local/bin:$PATH"  # airc' >> "$rc"
      ok "Added ~/.local/bin to PATH in $(basename "$rc")"
      break
    fi
  done
  export PATH="$BIN_DIR:$PATH"
fi

# ── Skills into Claude Code ─────────────────────────────────────────────

if [ -d "$CLONE_DIR/skills" ]; then
  mkdir -p "$SKILLS_TARGET"

  # Clean up old symlinks from previous installs.
  # Includes the airc-classic skill names (connect/send/rename/disconnect) that
  # were renamed to IRC-canonical (join/msg/nick/quit) — leaving the old symlinks
  # in place would shadow the new skills with stale content.
  for old in "$SKILLS_TARGET"/relay-* "$SKILLS_TARGET"/monitor "$SKILLS_TARGET"/setup "$SKILLS_TARGET"/uninstall \
             "$SKILLS_TARGET"/connect "$SKILLS_TARGET"/send "$SKILLS_TARGET"/rename "$SKILLS_TARGET"/disconnect; do
    [ -L "$old" ] && rm "$old" 2>/dev/null
  done

  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    # If the target is a real directory (from a pre-rename hand-install
    # or an old copy-based installer), it shadows the new symlink. Nuke it.
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      rm -rf "$target"
    elif [ -L "$target" ]; then
      rm "$target"
    fi
    ln -sf "$skill_dir" "$target"
    ok "Skill: /$skill_name"
  done
fi

# ── Tailscale (optional, cross-machine only) ─────────────────────────────
#
# airc works fully local — multiple tabs on the same machine pair via
# localhost / LAN IP, no Tailscale needed. Tailscale is the wire ONLY
# for cross-machine mesh (bigmama-wsl ↔ joels-macbook, office ↔ home,
# coworker's laptop on a different network).
#
# Install-time behavior:
#   - If tailscale CLI is present AND daemon is up → skip silently, we're good.
#   - If tailscale is missing OR daemon is down → Y/N prompt. Default N
#     because local-only is the common case. Y does install + sudoers +
#     start daemon + `tailscale up --ssh --accept-routes`.
#   - Non-interactive stdin (curl | bash, CI, AIRC_TAILSCALE=skip) → skip
#     without prompting. User can re-run `bash install.sh` in a real
#     terminal when they want cross-machine.
#   - AIRC_TAILSCALE=yes forces the setup without prompting (scripted
#     provisioning). AIRC_TAILSCALE=skip forces skip.

airc_setup_tailscale() {
  local ts_status="missing"
  if command -v tailscale >/dev/null 2>&1; then
    if tailscale status >/dev/null 2>&1; then
      ts_status="up"
    else
      ts_status="installed-but-down"
    fi
  fi

  # Already good → nothing to do.
  [ "$ts_status" = "up" ] && { ok "Tailscale: up ($(tailscale ip -4 2>/dev/null | head -1))"; return 0; }

  # Resolve intent: env override > TTY prompt > silent skip.
  local intent="${AIRC_TAILSCALE:-}"
  if [ -z "$intent" ]; then
    if [ -t 0 ] && [ -t 1 ]; then
      echo ""
      case "$ts_status" in
        missing)
          echo "  Tailscale is not installed. airc needs it only for CROSS-MACHINE"
          echo "  mesh (laptop ↔ desktop, you ↔ coworker). Local-only (multiple"
          echo "  tabs on this machine) works without it."
          ;;
        installed-but-down)
          echo "  Tailscale is installed but the daemon is not running. Same"
          echo "  story — only needed for cross-machine mesh."
          ;;
      esac
      printf "  Install/start Tailscale now? [y/N] "
      local reply=""
      read -r reply
      case "$reply" in y|Y|yes|YES) intent="yes" ;; *) intent="skip" ;; esac
    else
      intent="skip"
    fi
  fi

  if [ "$intent" != "yes" ]; then
    info "Skipping Tailscale setup (local-only airc works as-is)"
    echo "     Cross-machine later: re-run bash install.sh in a terminal,"
    echo "     or set AIRC_TAILSCALE=yes and re-run."
    return 0
  fi

  # Install CLI if missing.
  if [ "$ts_status" = "missing" ]; then
    info "Installing Tailscale (will prompt for sudo once)"
    if ! curl -fsSL https://tailscale.com/install.sh | sh; then
      echo "  ⚠ Tailscale install failed. Skipping — airc still works local-only." >&2
      return 0
    fi
  fi

  # Passwordless sudoers for tailscale + tailscaled so the daemon can be
  # started non-interactively later (by `airc status --probe`, by a
  # systemd unit, by a future `airc self-repair`). Written only if
  # missing. Matches continuum/src/scripts/install-tailscale.sh's
  # sudoers line so they coexist safely.
  if [ ! -f /etc/sudoers.d/airc-tailscale ]; then
    info "Writing /etc/sudoers.d/airc-tailscale (passwordless sudo for tailscale + tailscaled)"
    # Derive the tailscaled path. Common locations.
    local tsd_bin=""
    for cand in /usr/sbin/tailscaled /usr/bin/tailscaled /usr/local/sbin/tailscaled /usr/local/bin/tailscaled; do
      [ -x "$cand" ] && tsd_bin="$cand" && break
    done
    local ts_bin
    ts_bin=$(command -v tailscale 2>/dev/null || echo "/usr/bin/tailscale")
    # Only write if we actually found a tailscaled.
    if [ -n "$tsd_bin" ]; then
      # Use `sudo tee` so the install works even when install.sh itself
      # wasn't run as root.
      printf '%s ALL=(ALL) NOPASSWD: %s, %s\n' "$USER" "$ts_bin" "$tsd_bin" \
        | sudo tee /etc/sudoers.d/airc-tailscale >/dev/null
      sudo chmod 440 /etc/sudoers.d/airc-tailscale
    else
      echo "  ⚠ Couldn't locate tailscaled binary; skipping sudoers setup." >&2
    fi
  fi

  # Start the daemon if it's not already running.
  if ! tailscale status >/dev/null 2>&1; then
    info "Starting tailscaled"
    local tsd_bin=""
    for cand in /usr/sbin/tailscaled /usr/bin/tailscaled /usr/local/sbin/tailscaled /usr/local/bin/tailscaled; do
      [ -x "$cand" ] && tsd_bin="$cand" && break
    done
    if [ -n "$tsd_bin" ]; then
      sudo "$tsd_bin" --state=/var/lib/tailscale/tailscaled.state > /tmp/tailscaled.airc.log 2>&1 &
      disown 2>/dev/null || true
      # Poll up to 5s for daemon to come up.
      local i
      for i in 1 2 3 4 5 6 7 8 9 10; do
        tailscale status >/dev/null 2>&1 && break
        sleep 0.5
      done
    fi
    if ! tailscale status >/dev/null 2>&1; then
      echo "  ⚠ tailscaled didn't come up within 5s. Check: /tmp/tailscaled.airc.log" >&2
      return 0
    fi
  fi

  # Bring the node up on the tailnet + enable Tailscale SSH (for
  # tailscale's own SSH infrastructure; airc uses its own SSH identity
  # over port 7547 but --ssh gives the user console access via
  # `tailscale ssh` which is handy for debugging).
  info "Running: sudo tailscale up --ssh --accept-routes"
  if sudo tailscale up --ssh --accept-routes 2>&1; then
    ok "Tailscale: up ($(tailscale ip -4 2>/dev/null | head -1))"
  else
    echo "  ⚠ 'tailscale up' failed (likely needs login). Run manually:" >&2
    echo "     sudo tailscale up --ssh --accept-routes" >&2
  fi
}

airc_setup_tailscale

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  airc join                       # auto-#general (joins existing or hosts)"
echo "  airc join <gist-id>             # cross-account share via gist id"
echo "  airc msg @<peer> <message>      # DM a peer (or omit @peer to broadcast)"
echo ""
