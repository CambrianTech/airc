# Sourced by airc. cmd_rename — change identity name + propagate.
#
# Function exported back to airc's dispatch:
#   cmd_rename  — sanitize new name (a-z 0-9 -), write to config.json,
#                 emit a [rename] system event so peers update their
#                 local peer files, and recurse into sibling scopes
#                 (#179 — multi-scope propagation: a rename in the
#                 project scope also bumps the .general sidecar's
#                 nick so peers see one consistent identity).
#
# Flags:
#   --no-propagate  recursion guard for the multi-scope walk; the
#                   sub-call writes its own scope without re-entering.
#
# External cross-references (call-time): die, ensure_init, resolve_name,
# get_config_val, set_config_val, AIRC_HOME, AIRC_WRITE_DIR, MESSAGES.
#
# Extracted from airc as part of #152 Phase 3 file split — the final
# structural sweep.

cmd_rename() {
  # Parse flags. --no-propagate is the recursion guard for sibling-scope
  # propagation (#179): when cmd_rename recurses into `airc rename` for
  # each sibling scope, it passes --no-propagate so the sub-call does
  # its own scope's work without re-recursing into us.
  local no_propagate=0
  local new_name=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --no-propagate) no_propagate=1; shift ;;
      -h|--help|"")
        echo "Usage: airc rename <new-name>"
        echo "  Renames this identity and broadcasts [rename] to paired peers."
        echo "  --no-propagate    skip sibling-scope propagation (internal — used during recursion)"
        [ -z "${1:-}" ] && exit 1 || exit 0 ;;
      -*) die "Unknown flag: $1 (try: airc rename --help)" ;;
      *)
        [ -n "$new_name" ] && die "rename takes one name (got '$new_name' and '$1')"
        new_name="$1"; shift ;;
    esac
  done
  [ -z "$new_name" ] && { echo "Usage: airc rename <new-name>"; exit 1; }
  # Sanitize: lowercase, replace non-[a-z0-9-] with '-', collapse runs of
  # dashes, strip leading/trailing dashes, then cap. The post-sanitization
  # leading-dash strip matters because input like `.foo` becomes `-foo`
  # after the `[^a-z0-9-]` replacement and would slip past the case check
  # above — making the resulting name unreachable by `airc whois` /
  # `airc kick` (both reject leading-dash). Caught by Copilot review on
  # PR #75 follow-up.
  local _input="$new_name"
  new_name=$(echo "$new_name" \
    | tr '[:upper:]' '[:lower:]' \
    | sed 's/[^a-z0-9-]/-/g' \
    | sed 's/--*/-/g; s/^-*//; s/-*$//' \
    | cut -c1-24 \
    | sed 's/-*$//')
  [ -z "$new_name" ] && die "Invalid name (must be a-z 0-9 -)"
  [ ! -f "$CONFIG" ] && die "Not initialized — run 'airc connect' first"

  # Announce sanitization (vhsm-d1f4 caught 2026-04-29: 'two words' →
  # 'two-words', 'VHSMD1F4' → 'vhsmd1f4' silently). Pre-fix the user
  # had no signal that the name they typed wasn't the name that landed.
  if [ "$_input" != "$new_name" ]; then
    echo "  Sanitized: '$_input' → '$new_name' (allowed charset: a-z 0-9 -)"
  fi

  local old_name; old_name=$(get_config_val name "")
  if [ "$old_name" = "$new_name" ]; then
    echo "  Already named '$new_name'."
    return
  fi

  # Collision check (continuum-b741 + ideem-local-4bef caught
  # 2026-04-29: renaming to an active peer's name was accepted
  # silently, both peers then visible as the same name, DM routing
  # ambiguous). Two-source roster:
  #   1. PEERS_DIR — peers we've directly paired with via handshake.
  #   2. Recent unique 'from' values in our local messages.jsonl —
  #      catches peers we've HEARD from via gist polling but never
  #      paired with. Post-3c, with gh-substrate, this is the more
  #      common roster (you see everyone's broadcasts even if you
  #      never paired).
  # 200-line scan is cheap and catches the realistic case. Anything
  # older than that is fair game even if names overlap.
  if [ -d "$PEERS_DIR" ] && [ -f "$PEERS_DIR/$new_name.json" ]; then
    die "name collision: '$new_name' is already a paired peer (run 'airc peers' to see the roster)"
  fi
  if [ -f "$MESSAGES" ]; then
    # 2026-05-02 QA caught: my OWN historical nick (visible in
    # messages.jsonl from before my last rename) was being treated as
    # an "active peer" collision, blocking the natural rename-back
    # workflow (rename to test, rename back to original). Fix: walk
    # the [rename] chain inside the same 200-line window to mark all
    # nicks that were US, exclude them from the collision set. The
    # check still blocks renaming TO another peer's nick — original
    # safety property preserved.
    if tail -200 "$MESSAGES" 2>/dev/null \
         | AIRC_NEW_NAME="$new_name" AIRC_OLD_NAME="$old_name" "$AIRC_PYTHON" -c "
import sys, os, json, re
target = os.environ.get('AIRC_NEW_NAME', '')
my_current = os.environ.get('AIRC_OLD_NAME', '')
seen = set()
my_history = {my_current}  # current nick is always 'mine'
_rn = re.compile(r'\[rename\] old=([a-z0-9-]+) new=([a-z0-9-]+)')
for line in sys.stdin:
    try:
        m = json.loads(line)
        fr = m.get('from')
        msg = m.get('msg', '') or ''
        if fr:
            seen.add(fr)
        # Trace [rename] chain: if either side of a rename was us,
        # both sides are us. Multi-pass would be more correct, but
        # the linear pass catches the common case (consecutive renames).
        mm = _rn.match(msg)
        if mm:
            old_n, new_n = mm.group(1), mm.group(2)
            if old_n in my_history or new_n in my_history:
                my_history.add(old_n)
                my_history.add(new_n)
    except Exception:
        pass
sys.exit(0 if (target in seen and target not in my_history) else 1)
" 2>/dev/null; then
      die "name collision: '$new_name' has been seen as an active (foreign) peer in this room (use 'airc logs' to verify)"
    fi
  fi

  # Phase 1: write the new name into THIS scope's config (the truth-
  # layer effect for this scope). Goes through airc_core.config rather
  # than an inline-python heredoc — the heredoc was quoting-fragile
  # (would have broken on a name containing a single quote — currently
  # safe because the sanitizer keeps names in [a-z0-9-], but a sharp
  # edge in code that's about to recurse).
  "$AIRC_PYTHON" -m airc_core.config set_name --config "$CONFIG" --name "$new_name"
  echo "  Renamed: $old_name → $new_name"

  # Phase 2: propagate the config write to sibling scopes BEFORE
  # broadcasting (#179 — vhsm-d1f4 + ideem-local-4bef caught 2026-04-28
  # that nick rename only updated the current scope's config, leaving
  # any sidecar to broadcast under the OLD name).
  #
  # Order matters: configs first, broadcast last. cmd_send calls die()
  # if the scope's monitor is down, and die() is `exit 1` (kills the
  # whole shell, ignoring our `|| true`). Doing configs first means a
  # broadcast failure after this point cannot prevent propagation.
  #
  # --no-propagate prevents the sub-call from recursing back into us.
  # Each sibling scope writes its own config AND broadcasts in its own
  # room's host_target.
  if [ "$no_propagate" != "1" ]; then
    local _primary _parent _primary_base _sibling
    _primary=$(_primary_scope_for "$AIRC_WRITE_DIR")
    _parent=$(dirname "$_primary")
    _primary_base=$(basename "$_primary")
    # Glob all sibling sidecars (named <primary>.<room>) — does NOT
    # match the primary itself (which has no trailing `.<room>`).
    for _sibling in "$_parent/$_primary_base".*; do
      [ -d "$_sibling" ] || continue
      [ -f "$_sibling/config.json" ] || continue
      [ "$_sibling" = "$AIRC_WRITE_DIR" ] && continue
      AIRC_HOME="$_sibling" "$0" rename --no-propagate "$new_name" \
        || echo "  warn: rename propagation to $_sibling failed (exit $?)" >&2
    done
    # If WE are a sidecar (current scope != primary), also rename the
    # primary scope.
    if [ "$AIRC_WRITE_DIR" != "$_primary" ] && [ -f "$_primary/config.json" ]; then
      AIRC_HOME="$_primary" "$0" rename --no-propagate "$new_name" \
        || echo "  warn: rename propagation to primary $_primary failed (exit $?)" >&2
    fi
  fi

  # Phase 3: best-effort broadcast in this scope. Include a stable
  # `host` field so receivers can find THIS peer's record even if their
  # name-keyed lookup would miss (a prior rename marker got dropped;
  # their peer file for us still sits under an older name). host is
  # immutable per machine+user.
  #
  # --internal tells cmd_send to append-and-return rather than die()
  # when this scope's monitor is down. [rename] is informational;
  # receivers heal via monitor_formatter's host-fallback on next
  # traffic regardless of whether they saw this specific event.
  local my_host; my_host="$(whoami)@$(get_host)"
  cmd_send --internal "[rename] old=$old_name new=$new_name host=$my_host" >/dev/null || true
}
