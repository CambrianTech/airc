# Sourced by airc. Release-info cluster — cmd_update + cmd_channel + cmd_version.
#
# Functions exported back to airc's dispatch:
#   cmd_update   — `git pull` the install dir on the active channel and
#                  re-run install.sh so new skills get symlinked. Idempotent.
#                  --channel <name> switches branch first.
#   cmd_channel  — show or set the release channel (canary | main) without
#                  pulling. Lightweight inverse of `airc canary`.
#   cmd_version  — print the running install's git rev + branch + path.
#                  Same shape as `airc --version` / `airc -v`.
#
# Bundled because all three answer the same user question: "what release
# am I on, and how do I move?" External cross-references (call-time): die,
# AIRC_CHANNEL (env), the install_dir resolver in airc top-level.
#
# Extracted from airc as part of #152 Phase 3 file split — the final
# structural sweep.

cmd_update() {
  # Refresh install dir AND re-run install.sh so new skills get symlinked
  # into ~/.claude/skills/ and old ones get cleaned up. install.sh is
  # idempotent — it handles the pull, the binary symlink, and the skill
  # directory refresh in one pass. Does NOT teardown or reconnect.
  #
  # Channels (#40 followup): airc supports release channels for opt-in
  # pre-merge testing. main = stable; canary = features-not-yet-promoted.
  # The chosen channel persists in $AIRC_DIR/.channel so subsequent
  # `airc update` (no args) keeps the user on their chosen track.
  #   airc update                    # stay on current channel (default: main)
  #   airc update --channel canary   # switch to canary + update
  #   airc update --channel main     # switch back to main + update
  #   airc channel                   # show current channel without updating
  local dir="${AIRC_DIR:-$HOME/.airc-src}"
  local channel_file="$dir/.channel"
  local requested_channel=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --channel|-c)
        requested_channel="${2:-}"
        [ -z "$requested_channel" ] && die "Usage: airc update --channel <name>"
        shift 2
        ;;
      --canary) requested_channel="canary"; shift ;;
      --main)   requested_channel="main";   shift ;;
      *) shift ;;
    esac
  done

  if [ ! -d "$dir/.git" ]; then
    die "No git checkout at $dir. Reinstall: curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash"
  fi

  # Determine target channel: explicit request > saved preference > main.
  local channel
  if [ -n "$requested_channel" ]; then
    channel="$requested_channel"
  elif [ -f "$channel_file" ]; then
    channel=$(cat "$channel_file" 2>/dev/null | tr -d '[:space:]')
    [ -z "$channel" ] && channel="main"
  else
    channel="main"
  fi

  # Switch to the target branch BEFORE pulling. install.sh will then ff-pull
  # whatever branch is checked out. Fail loud if the channel doesn't exist
  # on origin — silently falling back to main would defeat the opt-in test
  # purpose.
  local before; before=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null)
  local current_branch; current_branch=$(git -C "$dir" rev-parse --abbrev-ref HEAD 2>/dev/null)
  if [ "$current_branch" != "$channel" ]; then
    git -C "$dir" fetch --quiet origin "$channel" 2>/dev/null \
      || die "Channel '$channel' not found on origin. Try: airc channel (to see options)."
    git -C "$dir" checkout -q "$channel" 2>/dev/null \
      || git -C "$dir" checkout -q -B "$channel" "origin/$channel" 2>/dev/null \
      || die "Failed to checkout '$channel'. Resolve manually in $dir."
  fi

  if [ ! -x "$dir/install.sh" ]; then
    die "install.sh missing at $dir. Reinstall via curl|bash."
  fi
  AIRC_DIR="$dir" bash "$dir/install.sh" || die "install.sh failed."

  # Persist channel choice AFTER successful update so a failed switch
  # doesn't leave a dangling preference for a broken state.
  echo "$channel" > "$channel_file"

  local after; after=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null)
  if [ "$before" = "$after" ]; then
    echo "  Already at ${after} on channel '${channel}'. Skills refreshed."
  else
    echo "  Updated: ${before} -> ${after} on channel '${channel}'. Skills refreshed."
  fi

  # Stale-running-monitor detection (vhsm-d1f4's gotcha 2026-04-28):
  # bash sources its functions in-memory at process start; an airc
  # connect that's been running since BEFORE this update is still
  # executing the old version. We can't auto-restart safely (would
  # interrupt active SSH sessions), so we print a loud, action-shaped
  # warning ONLY when a monitor is actually running in the current
  # scope. Silent when nothing is running.
  if [ -f "$AIRC_WRITE_DIR/airc.pid" ]; then
    local _pid; _pid=$(awk '{print $1; exit}' "$AIRC_WRITE_DIR/airc.pid" 2>/dev/null)
    if [ -n "$_pid" ] && kill -0 "$_pid" 2>/dev/null; then
      echo ""
      echo "  ⚠  A running airc monitor (PID ${_pid}) is still on the OLD code."
      echo "     Restart to pick up ${after}:"
      echo ""
      echo "       airc teardown && airc connect"
      echo ""
    fi
  fi
}

# ── cmd_channel: show or set the release channel without pulling ──────
# `airc channel`           → print current channel + how to switch
# `airc channel canary`    → set preferred channel; doesn't pull (use
#                            `airc update` after to actually switch)
# Allows the AI / human to inspect + decide before the heavier update.
cmd_channel() {
  local dir="${AIRC_DIR:-$HOME/.airc-src}"
  local channel_file="$dir/.channel"
  local current="main"
  [ -f "$channel_file" ] && current=$(cat "$channel_file" 2>/dev/null | tr -d '[:space:]')
  [ -z "$current" ] && current="main"

  local target="${1:-}"
  if [ -z "$target" ]; then
    echo "  Channel: $current"
    echo "  Available channels (any branch on origin can be a channel):"
    echo "    main      — stable, what most users run"
    echo "    canary    — features queued for the next main merge; opt-in testing"
    echo "  Switch:"
    echo "    airc channel <name>           # set preference (run 'airc update' after)"
    echo "    airc update --channel <name>  # set + pull in one step"
    return 0
  fi

  echo "$target" > "$channel_file"
  echo "  Channel preference set: '$target'. Run 'airc update' to actually switch + pull."
}

cmd_version() {
  # Report git state for whichever airc actually ran. Prefer the binary's
  # own directory so a dev-checkout run doesn't lie about AIRC_DIR.
  local self; self="$(realpath "$0" 2>/dev/null || echo "$0")"
  local here; here="$(dirname "$self")"
  local dir=""
  if [ -d "$here/.git" ]; then
    dir="$here"
  elif [ -d "${AIRC_DIR:-$HOME/.airc-src}/.git" ]; then
    dir="${AIRC_DIR:-$HOME/.airc-src}"
  fi
  if [ -z "$dir" ]; then
    echo "  unknown (no git metadata found)"
    return
  fi
  local sha subject branch dirty
  sha=$(git -C "$dir" rev-parse --short HEAD 2>/dev/null)
  subject=$(git -C "$dir" log -1 --format=%s 2>/dev/null)
  branch=$(git -C "$dir" rev-parse --abbrev-ref HEAD 2>/dev/null)
  dirty=""
  [ -n "$(git -C "$dir" status --porcelain 2>/dev/null)" ] && dirty=" (dirty)"
  echo "  airc ${sha}${dirty} on ${branch}"
  [ -n "$subject" ] && echo "  ${subject}"
  echo "  install: $dir"
}
