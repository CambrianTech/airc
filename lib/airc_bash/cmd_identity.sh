# Sourced by airc. Identity bundle — agent persona ops (issue #34).
#
# Functions exported back to airc's dispatch:
#   cmd_away      — set/clear away status (IRC /away alias for
#                   `identity set --status`).
#   cmd_identity  — verb router (show|set|link|import|push).
#   cmd_whois     — print identity of self / host / paired peer / cross-peer
#                   via host. Resolves cross-account peers by tunneling
#                   through the host's whois cache.
#
# Private helpers (all `_identity_*`):
#   _identity_show / _identity_set / _identity_link  — local CRUD on
#     config.json's `identity` block.
#   _identity_import / _identity_push  — verb routers for cross-platform
#     persona linking (issue #34 v2).
#   _identity_import_continuum / _identity_push_continuum  — concrete
#     adapters for continuum (the only platform implemented today).
#
# External cross-references (call-time): die, ensure_init, get_config_val,
# set_config_val, resolve_name, airc_rs_bin, AIRC_HOME, CONFIG, plus the
# continuum CLI on PATH for import/push.
#
# Extracted from airc as part of #152 Phase 3 file split. The bundle is
# already cohesive (every helper is `_identity_*`, every public verb is
# about presence/persona) so it goes to ONE file, not three.

# ── Identity (issue #34) ────────────────────────────────────────────────
#
# Structured agent persona, layered on top of the bootstrap name from
# derive_name. Stored under config.json's `identity` key (single-file
# scope: `name` already lives in config.json, identity fields sit
# alongside). Five fields:
#
#   pronouns      — she/they/he/it; used by skill narrators for grammar
#   role          — short hyphenated tag, e.g. "device-link-orchestrator"
#   bio           — one-line free-form, IRC-realname analog
#   status        — mutable "what I'm working on now" (Slack-like)
#   integrations  — { platform: handle } mappings to other platforms
#                   (continuum, slack, telegram) so airc identity can
#                   adopt or be adopted by canonical persona elsewhere
#
# Skill-side bootstrap prompts the agent to fill these on first /join
# (set AIRC_NO_IDENTITY_PROMPT=1 to skip — used by integration tests).
# v1: airc identity show/set/link locally; airc whois on self.
# v2 (deferred): peer WHOIS over SSH; live continuum/slack import/push.

# IRC /away: short alias for `airc identity set --status ...`. With a
# message, marks the agent as away. Without args, clears the status
# (back from away). Adheres to IRC convention; the longer form
# (airc identity set --status) still works for scripted state changes.
cmd_away() {
  ensure_init
  # Intercept --help BEFORE building $msg from $* — verb fuzzing
  # 2026-04-28 caught `airc away --help` writing "--help" as the status
  # string. Same anti-pattern as #231/#236; same shape fix.
  case "${1:-}" in
    -h|--help)
      echo "Usage:"
      echo "  airc away             clear away status (back)"
      echo "  airc away <message>   set away status to <message>"
      return 0 ;;
  esac
  if [ $# -eq 0 ]; then
    _identity_set --status "" >/dev/null
    echo "  back — away cleared."
  else
    local msg="$*"
    _identity_set --status "$msg" >/dev/null
    echo "  away: $msg"
  fi
}

cmd_identity() {
  local sub="${1:-show}"
  shift 2>/dev/null || true
  case "$sub" in
    whoami|register|adopt|-h|--help|help) ;;
    *) ensure_init ;;
  esac
  case "$sub" in
    show|"") _identity_show ;;
    whoami)  _identity_whoami ;;
    register|adopt) _identity_register "$@" ;;
    set)     _identity_set "$@" ;;
    link)    _identity_link "$@" ;;
    import)  _identity_import "$@" ;;
    push)    _identity_push "$@" ;;
    -h|--help|help)
      echo "Usage:"
      echo "  airc identity show                            Print own identity"
      echo "  airc identity whoami                          Print transport + work identity"
      echo "  airc identity register --name <handle>        Set this session's queue/work identity"
      echo "  airc identity set [--pronouns X] [--role Y] [--bio \"…\"] [--status \"…\"]"
      echo "  airc identity link <platform> [handle]        Map this identity to a platform persona (omit handle to unlink)"
      echo "  airc identity import <platform>:<id>          Pull persona from platform (continuum)"
      echo "  airc identity push <platform>                 Send local fields to platform (continuum)"
      ;;
    *) die "Unknown identity subcommand: $sub (try: show, whoami, register, set, link, import, push)" ;;
  esac
}

_identity_session_file() {
  local transport_name="${1:-}"
  [ -z "$transport_name" ] && transport_name="anonymous"
  mkdir -p "$AIRC_WRITE_DIR/sessions" 2>/dev/null || true
  "$(airc_rs_bin)" identity session-file --write-dir "$AIRC_WRITE_DIR" --transport-name "$transport_name"
}

_identity_default_work_name() {
  local transport_name="${1:-anonymous}"
  local session_file="${2:-}"
  "$(airc_rs_bin)" identity default-work-name --transport-name "$transport_name" --session-file "$session_file"
}

_identity_read_work_name() {
  local session_file="$1"
  [ -f "$session_file" ] || return 1
  "$(airc_rs_bin)" identity read-work-name --session-file "$session_file"
}

_identity_write_work_session() {
  local session_file="$1" name="$2" transport_name="$3"
  _validate_peer_name "$name"
  "$(airc_rs_bin)" identity write-work-session --session-file "$session_file" --name "$name" --transport-name "$transport_name"
}

_identity_resolve_work_name() {
  local transport_name session_file saved_name default_name
  if declare -F resolve_name >/dev/null 2>&1; then
    transport_name=$(resolve_name)
  else
    transport_name="anonymous"
  fi

  session_file=$(_identity_session_file "$transport_name")
  if saved_name=$(_identity_read_work_name "$session_file" 2>/dev/null); then
    printf '%s\n' "$saved_name"
    return 0
  fi

  # Compatibility shim for existing automation. This is no longer the
  # product path; first-class session state is written immediately below.
  if [ -n "${AIRC_QUEUE_OWNER:-}" ]; then
    _identity_write_work_session "$session_file" "$AIRC_QUEUE_OWNER" "$transport_name"
    printf '%s\n' "$AIRC_QUEUE_OWNER"
    return 0
  fi
  if [ -n "${AIRC_AGENT_NAME:-}" ]; then
    _identity_write_work_session "$session_file" "$AIRC_AGENT_NAME" "$transport_name"
    printf '%s\n' "$AIRC_AGENT_NAME"
    return 0
  fi
  if [ -n "${AIRC_AGENT_NICK:-}" ]; then
    _identity_write_work_session "$session_file" "$AIRC_AGENT_NICK" "$transport_name"
    printf '%s\n' "$AIRC_AGENT_NICK"
    return 0
  fi

  default_name=$(_identity_default_work_name "$transport_name" "$session_file")
  _identity_write_work_session "$session_file" "$default_name" "$transport_name"
  printf '%s\n' "$default_name"
}

_identity_whoami() {
  local transport_name session_file work_name
  transport_name=$(resolve_name)
  session_file=$(_identity_session_file "$transport_name")
  work_name=$(_identity_resolve_work_name)
  printf '  transport:  %s\n' "$transport_name"
  printf '  work:       %s\n' "$work_name"
  printf '  session:    %s\n' "$session_file"
}

_identity_register() {
  local name=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --name) name="${2:-}"; shift 2 ;;
      -h|--help)
        echo "Usage:"
        echo "  airc identity register --name <handle>"
        echo "  airc identity adopt <handle>"
        return 0 ;;
      --*) die "Unknown flag: $1" ;;
      *)  name="$1"; shift ;;
    esac
  done
  [ -n "$name" ] || die "Usage: airc identity register --name <handle>"
  local transport_name session_file
  transport_name=$(resolve_name)
  session_file=$(_identity_session_file "$transport_name")
  _identity_write_work_session "$session_file" "$name" "$transport_name"
  printf '  work identity: %s\n' "$name"
  printf '  session: %s\n' "$session_file"
}

# Identity bootstrap nudge (#146). Called once after a successful
# `airc connect` join. If pronouns/role/bio are all (unset), print a
# one-time prompt encouraging the user to set them. Idempotent: the
# nudge file under $AIRC_WRITE_DIR/.identity_nudged_v1 records that
# we've nudged, so we don't nag every reconnect. The skill /join can
# still drive interactive bootstrap; this is the binary-side fallback
# for users running airc directly.
_identity_bootstrap_nudge_if_unset() {
  local nudge_file="$AIRC_WRITE_DIR/.identity_nudged_v1"
  [ -f "$nudge_file" ] && return 0
  "$(airc_rs_bin)" identity nudge-needed --config "$CONFIG"
  local nudge_status=$?
  if [ "$nudge_status" = "2" ]; then
    echo ""
    echo "  Tip: set your identity so peers know who they are talking to. One-line example:"
    echo "    airc identity set --pronouns they --role 'your role' --bio 'one-sentence bio'"
    echo "  Done? Suppress this nudge: touch $nudge_file"
    echo ""
  elif [ "$nudge_status" != "0" ]; then
    return "$nudge_status"
  fi
  : > "$nudge_file" 2>/dev/null || true
}

_identity_show() {
  "$(airc_rs_bin)" identity show-config --config "$CONFIG"
}

_identity_set() {
  local pronouns="" role="" bio="" status=""
  local set_pronouns=0 set_role=0 set_bio=0 set_status=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --pronouns) pronouns="${2:-}"; set_pronouns=1; shift 2 ;;
      --role)     role="${2:-}";     set_role=1;     shift 2 ;;
      --bio)      bio="${2:-}";      set_bio=1;      shift 2 ;;
      --status)   status="${2:-}";   set_status=1;   shift 2 ;;
      *) die "Unknown flag: $1 (use --pronouns/--role/--bio/--status)" ;;
    esac
  done
  if [ "$set_pronouns" = 0 ] && [ "$set_role" = 0 ] && [ "$set_bio" = 0 ] && [ "$set_status" = 0 ]; then
    die "Pass at least one of --pronouns / --role / --bio / --status"
  fi
  # Length caps (caught 2026-04-29: 4KB bio stored
  # verbatim → broke peer rendering + ate gist quota). Bio is one line
  # of context, not a manifesto. Hard caps with loud rejection.
  local _max_pronouns=64
  local _max_role=128
  local _max_bio=512
  local _max_status=256
  if [ "$set_pronouns" = 1 ] && [ "${#pronouns}" -gt "$_max_pronouns" ]; then
    die "pronouns too long (${#pronouns} chars; max $_max_pronouns)"
  fi
  if [ "$set_role" = 1 ] && [ "${#role}" -gt "$_max_role" ]; then
    die "role too long (${#role} chars; max $_max_role)"
  fi
  if [ "$set_bio" = 1 ] && [ "${#bio}" -gt "$_max_bio" ]; then
    die "bio too long (${#bio} chars; max $_max_bio — bios are one-liners, not manifestos)"
  fi
  if [ "$set_status" = 1 ] && [ "${#status}" -gt "$_max_status" ]; then
    die "status too long (${#status} chars; max $_max_status)"
  fi
  local args=(identity set-config --config "$CONFIG")
  [ "$set_pronouns" = 1 ] && args+=(--pronouns "$pronouns")
  [ "$set_role" = 1 ] && args+=(--role "$role")
  [ "$set_bio" = 1 ] && args+=(--bio "$bio")
  [ "$set_status" = 1 ] && args+=(--status "$status")
  "$(airc_rs_bin)" "${args[@]}"
}

_identity_link() {
  local platform="${1:-}" handle="${2:-}"
  [ -z "$platform" ] && die "Usage: airc identity link <platform> [handle] (omit/blank handle to unlink)"
  "$(airc_rs_bin)" identity link-config --config "$CONFIG" --platform "$platform" --handle "$handle"
}

# WHOIS: prints identity for self, host, paired peer, or other peer of
# our host. Identity blobs are exchanged at pair-handshake time and
# cached locally — no round-trip needed for self/host/local-peer. Cross-
# peer (we're a joiner asking about another joiner of our host) falls
# back to a single SSH read of the host's peer file.
#
# Cross-scope (issue #134): walks sibling scopes (.airc + .airc.<room>)
# so a project-tab whois can find a peer who's only in the #general
# sidecar's host. Without this, JOIN events in the sidecar room emit
# names that whois can't resolve, breaking the IRC mental model where
# every room member is reachable.
cmd_whois() {
  ensure_init
  local target="${1:-}"
  local my_name; my_name=$(get_name)

  # Help-flag intercept — without this, --help flowed into target and
  # got rejected by _validate_peer_name with the unhelpful "must not
  # start with '-'" error.
  case "$target" in
    -h|--help)
      echo "Usage:"
      echo "  airc whois                         show own identity"
      echo "  airc whois <peer>                  show peer's identity (paired or via host)"
      return 0 ;;
  esac

  # Self — same identity across all scopes, no walk needed.
  if [ -z "$target" ] || [ "$target" = "$my_name" ]; then
    _identity_show
    return 0
  fi

  # Reject path-traversal / shell-injection in target before it touches
  # filesystem paths (local <scope>/peers/<target>.json) or remote SSH
  # cmds (cat $host_airc_home/peers/<target>.json) in any scope.
  _validate_peer_name "$target"

  # Phase 2B.3 onward: only the primary scope. Sibling sidecar scopes
  # are no longer spawned; any leftover .airc.<word> dirs have stale
  # peer records that would resurface as ghosts (e.g. integration-test
  # fixtures from /tmp/airc-trace2 surviving for days).
  #
  # Phase 2C+ TODO: cross-mesh whois resolution (ask the host for a
  # peer record we don't have locally). Without that, indirect peers
  # in the singleton mesh — peers paired with the host but not with
  # us directly — return "no record". Tracked as the wart that
  # surfaced during QA 2026-04-28.
  if _whois_in_scope "$AIRC_WRITE_DIR" "$target"; then
    return 0
  fi
  local _client_id; _client_id=$(airc_client_id 2>/dev/null || true)
  if "$(airc_rs_bin)" collaboration observed-whois \
      --home "$AIRC_WRITE_DIR" --my-name "$my_name" --peer-name "$target" --client-id "$_client_id"; then
    return 0
  fi

  echo "  whois: no record for '$target' (try airc peers to list paired peers)"
  return 1
}

# Per-scope whois lookup. Returns 0 + prints if found; non-zero if not.
# Args: scope-dir, target-name. Caller has already validated target.
_whois_in_scope() {
  local scope="$1" target="$2"
  local scope_config="$scope/config.json"
  local scope_peers="$scope/peers"
  [ -f "$scope_config" ] || return 1

  # All scope-local config + peer file reads route through
  # get_config_val_in / airc-rs config. Pre-migration
  # this function had six inline JSON snippets reading individual
  # JSON fields — each a silent-fail vector with bash-substituted
  # SCOPE_CONFIG / PEER_FILE env vars. Now: one CLI per read.
  #
  # Host of this scope (we're a joiner, target is the host we paired with).
  local host_name; host_name=$(get_config_val_in "$scope_config" host_name "")
  if [ -n "$host_name" ] && [ "$target" = "$host_name" ]; then
    local host_id_blob; host_id_blob=$(get_config_val_in "$scope_config" host_identity "{}")
    local host_target_addr; host_target_addr=$(get_config_val_in "$scope_config" host_target "")
    _whois_pretty "$target" "$host_id_blob" "$host_target_addr"
    return 0
  fi

  # Local peer file under this scope. Same get_config_val_in shape —
  # peer files are JSON-shaped just like config.json.
  local peer_file="$scope_peers/$target.json"
  if [ -f "$peer_file" ]; then
    local blob; blob=$(get_config_val_in "$peer_file" identity "{}")
    local host; host=$(get_config_val_in "$peer_file" host "")
    _whois_pretty "$target" "$blob" "$host"
    return 0
  fi

  # Cross-peer via this scope's host (we're a joiner; query host's peer
  # file remotely). Skipped when we're the host of this scope (no
  # host_target). The SSH key for this scope is at $scope/identity/ssh_key
  # — relay_ssh picks up IDENTITY_DIR from the env, so we set it for the
  # subprocess.
  local host_target_addr; host_target_addr=$(get_config_val_in "$scope_config" host_target "")
  local host_airc_home; host_airc_home=$(get_config_val_in "$scope_config" host_airc_home "")
  if [ -n "$host_target_addr" ] && [ -n "$host_airc_home" ]; then
    local remote_blob
    remote_blob=$(IDENTITY_DIR="$scope/identity" relay_ssh "$host_target_addr" "cat $host_airc_home/peers/$target.json 2>/dev/null" 2>/dev/null || true)
    if [ -n "$remote_blob" ]; then
      local peer_id; peer_id=$(printf '%s' "$remote_blob" | "$(airc_rs_bin)" gist get .identity "{}" 2>/dev/null || echo "{}")
      local peer_host; peer_host=$(printf '%s' "$remote_blob" | "$(airc_rs_bin)" gist get .host 2>/dev/null || echo "")
      _whois_pretty "$target" "$peer_id" "$peer_host"
      return 0
    fi
  fi

  return 1
}

# Pretty-print an identity blob (JSON string) for a named peer.
# Args: name, identity-json, host (any may be empty).
_whois_pretty() {
  local name="$1" blob="${2:-{\}}" host="${3:-}"
  "$(airc_rs_bin)" identity pretty --name "$name" --identity-json "$blob" --host "$host"
}

# cmd_kick extracted to lib/airc_bash/cmd_kick.sh
# (#152 Phase 3 file split). Host-only peer eviction lives in its own
# file rather than the identity bundle — kick is moderation, not
# identity — and pulling it out first makes the surrounding identity
# block contiguous for the next extraction PR.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_kick.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_kick.sh
  source "$_airc_lib_dir/airc_bash/cmd_kick.sh"
else
  echo "ERROR: airc_bash/cmd_kick.sh not found via lib-dir resolver." >&2
  exit 1
fi

# ── Identity import/push (issue #34 v2) ─────────────────────────────────
#
# Cross-platform persona linking. The basic shape: airc has an opt-in
# tool wrapper for each known platform. If the platform's CLI is on PATH
# AND a matching profile is found, pull/push fields. Otherwise: clear
# error pointing at the manual `airc identity link <platform> <handle>`.
#
# v1 supports: continuum (the high-leverage internal case). slack/
# telegram/discord are stubs that error with platform-install hints —
# they're scaffolding for future PRs, not productionized integrations.

_identity_import() {
  local spec="${1:-}"
  [ -z "$spec" ] && die "Usage: airc identity import <platform>:<id>"
  local platform="${spec%%:*}"
  local id="${spec#*:}"
  if [ "$platform" = "$spec" ] || [ -z "$id" ]; then
    die "Usage: airc identity import <platform>:<id> (got '$spec' — missing colon?)"
  fi
  case "$platform" in
    continuum)
      _identity_import_continuum "$id" ;;
    slack|telegram|discord)
      die "import from $platform not yet implemented. For now, run: airc identity link $platform <handle>"
      ;;
    *)
      die "Unknown platform '$platform'. Supported: continuum (v1). slack/telegram/discord stubbed."
      ;;
  esac
}

_identity_push() {
  local platform="${1:-}"
  [ -z "$platform" ] && die "Usage: airc identity push <platform>"
  case "$platform" in
    continuum)
      _identity_push_continuum ;;
    slack|telegram|discord)
      die "push to $platform not yet implemented. For now, run: airc identity link $platform <handle>"
      ;;
    *)
      die "Unknown platform '$platform'. Supported: continuum (v1). slack/telegram/discord stubbed."
      ;;
  esac
}

# Continuum integration: shells out to a `continuum` binary if it's on
# PATH. Expected interface (best-effort — we degrade gracefully if the
# binary doesn't support these subcommands yet):
#   continuum persona show <name>          → prints JSON {pronouns, role, bio, ...}
#   continuum persona update <name> --bio ...  → updates the persona
# If continuum isn't installed, link() the handle anyway so the mapping
# is recorded for future syncs.
_identity_import_continuum() {
  local id="$1"
  if ! command -v continuum >/dev/null 2>&1; then
    echo "  continuum CLI not on PATH — recording link only."
    echo "  Once you install continuum, re-run: airc identity import continuum:$id"
    _identity_link continuum "$id"
    return 0
  fi
  local blob; blob=$(continuum persona show "$id" 2>/dev/null || true)
  if [ -z "$blob" ]; then
    echo "  continuum persona '$id' not found — recording link only."
    _identity_link continuum "$id"
    return 0
  fi
  # Parse the JSON; merge into our identity. Empty fields skip; existing
  # fields get overwritten (the user's intent: "I want to BE this persona").
  "$(airc_rs_bin)" identity import-continuum --config "$CONFIG" --blob "$blob"
}

_identity_push_continuum() {
  if ! command -v continuum >/dev/null 2>&1; then
    die "continuum CLI not on PATH — install continuum before pushing."
  fi
  local handle; handle=$("$(airc_rs_bin)" identity continuum-handle --config "$CONFIG" 2>/dev/null)
  [ -z "$handle" ] && die "No continuum handle linked. Run: airc identity link continuum <name>"
  "$(airc_rs_bin)" identity push-continuum --config "$CONFIG" --handle "$handle"
}
