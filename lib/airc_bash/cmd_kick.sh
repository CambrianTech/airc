# Sourced by airc. cmd_kick — host-only peer eviction.
#
# Function exported to airc's dispatch:
#   cmd_kick — forcibly remove a paired peer (IRC /kick analog).
#              Emits a system event, drops the peer's SSH pubkey from
#              authorized_keys, deletes the peer file. The kicked
#              peer's tail loop dies on the closed pipe; future SSH
#              auth attempts fail because their key is gone.
#
# External cross-references (call-time): die, ensure_init, get_config_val,
# resolve_name, AIRC_HOME, AIRC_WRITE_DIR, MESSAGES.
#
# Extracted from airc as part of #152 Phase 3 file split. Standalone
# (not bundled with identity) because kick is host moderation, not
# identity — separating now also lets the identity bundle pull cleanly
# in the next PR.

cmd_kick() {
  # Host-only: forcibly remove a paired peer. IRC analog: /kick <user>.
  # Steps: emit a system event, drop their SSH pubkey from authorized_keys,
  # remove the peer file. The kicked peer's tail loop dies on the closed
  # pipe AND any future auth attempts fail because their key is gone from
  # authorized_keys — they can't silently keep operating after a kick.
  # They can re-pair via airc connect (no ban yet) — for that, see future
  # `airc ban`.
  ensure_init
  local target="${1:-}"
  case "$target" in
    -h|--help|"")
      echo "Usage: airc kick <peer> [reason]   (or: airc kick @peer)"
      echo "  Host-only. Removes peer's SSH pubkey + peer file."
      [ -z "$target" ] && return 1
      return 0 ;;
  esac
  # Accept @-prefix for parity with `airc msg @peer`. QA pass found
  # the @-rejection inconsistent — users who write @ for DM naturally
  # write @ for kick. Strip BEFORE validation; the rest of the function
  # uses $target (not $1).
  target="${target#@}"
  _validate_peer_name "$target"
  shift || true
  local reason="${*:-no reason given}"

  # Joiner role check — kicking only makes sense as host.
  local host_target; host_target=$(get_config_val host_target "")
  if [ -n "$host_target" ]; then
    die "kick: only the room host can kick. You are a joiner of $host_target — talk to the host."
  fi

  local peer_file="$PEERS_DIR/$target.json"
  if [ ! -f "$peer_file" ]; then
    die "kick: '$target' not in peers list (try: airc peers)"
  fi

  # Read the joiner's SSH pubkey from the peer JSON record (the host
  # handshake stores it there — `<peer>.pub` holds the SIGNING pubkey,
  # not the SSH auth key, so we can't use that file). Without this,
  # kick would leave the joiner's SSH key in authorized_keys and the
  # peer could keep authenticating despite the "kick" — caught by
  # Copilot review on PR #73.
  local peer_ssh_pub
  peer_ssh_pub=$(PEER_FILE="$peer_file" "$AIRC_PYTHON" -c '
import json, os
try:
    p = json.load(open(os.environ["PEER_FILE"]))
    print((p.get("ssh_pub") or "").strip())
except Exception:
    pass
' 2>/dev/null || echo "")

  if [ -n "$peer_ssh_pub" ] && [ -f "$HOME/.ssh/authorized_keys" ]; then
    # grep -v returns 1 when every line matches (or the file is empty);
    # both are fine outcomes here, so eat the exit code.
    grep -vF "$peer_ssh_pub" "$HOME/.ssh/authorized_keys" > "$HOME/.ssh/authorized_keys.tmp" 2>/dev/null || true
    [ -f "$HOME/.ssh/authorized_keys.tmp" ] && mv "$HOME/.ssh/authorized_keys.tmp" "$HOME/.ssh/authorized_keys"
    chmod 600 "$HOME/.ssh/authorized_keys" 2>/dev/null || true
  fi

  # Remove peer files (rm -f is set-e-safe). The .pub here is the
  # signing key file, separate from authorized_keys.
  rm -f "$peer_file" "$PEERS_DIR/$target.pub"

  # Emit a system event so the kicked peer (and others) see it in the
  # tail stream. Reuse cmd_send's plumbing.
  cmd_send "[kick] $target ($reason)" >/dev/null 2>&1 || true

  if [ -n "$peer_ssh_pub" ]; then
    echo "  Kicked $target ($reason). SSH key removed from authorized_keys; peer file gone."
  else
    echo "  Kicked $target ($reason). Peer file gone, but no SSH key recorded for this peer — they were paired before #34's handshake update; their authorized_keys entry survived. Run airc peers to confirm."
  fi
  echo "  They can re-pair via airc connect; for permanent ban, see future 'airc ban'."
}
