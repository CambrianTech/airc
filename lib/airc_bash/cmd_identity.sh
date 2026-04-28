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
# set_config_val, resolve_name, AIRC_HOME, AIRC_PYTHON, CONFIG, plus the
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
  # Intercept --help BEFORE building $msg from $* — vhsm-d1f4's verb-fuzz
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
  ensure_init
  local sub="${1:-show}"
  shift 2>/dev/null || true
  case "$sub" in
    show|"") _identity_show ;;
    set)     _identity_set "$@" ;;
    link)    _identity_link "$@" ;;
    import)  _identity_import "$@" ;;
    push)    _identity_push "$@" ;;
    -h|--help|help)
      echo "Usage:"
      echo "  airc identity show                            Print own identity"
      echo "  airc identity set [--pronouns X] [--role Y] [--bio \"…\"] [--status \"…\"]"
      echo "  airc identity link <platform> [handle]        Map this identity to a platform persona (omit handle to unlink)"
      echo "  airc identity import <platform>:<id>          Pull persona from platform (continuum)"
      echo "  airc identity push <platform>                 Send local fields to platform (continuum)"
      ;;
    *) die "Unknown identity subcommand: $sub (try: show, set, link, import, push)" ;;
  esac
}

_identity_show() {
  CONFIG="$CONFIG" "$AIRC_PYTHON" -c '
import json, os
try:
    c = json.load(open(os.environ["CONFIG"]))
except Exception:
    print("  (no config — run airc connect)"); raise SystemExit(0)
ident = c.get("identity", {}) or {}
fields = [
    ("name",     c.get("name", "?"),         ""),
    ("pronouns", ident.get("pronouns", ""),  "(unset)"),
    ("role",     ident.get("role", ""),      "(unset)"),
    ("bio",      ident.get("bio", ""),       "(unset)"),
    # status field is the IRC /away analog. Surface the airc away
    # command in the unset case so QA users (continuum-b741 2026-04-27)
    # do not see a half-baked empty field with no obvious setter.
    ("status",   ident.get("status", ""),    "(unset; airc away <msg> to set)"),
]
for k, v, fallback in fields:
    label = k + ":"
    value = v if v else fallback
    print(f"  {label:<11} {value}")
ints = ident.get("integrations", {}) or {}
if ints:
    print("  integrations:")
    for k, v in ints.items():
        print(f"    {k}: {v}")
else:
    print("  integrations: (none)")
'
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
  CONFIG="$CONFIG" \
    SET_PRONOUNS="$set_pronouns" PRONOUNS="$pronouns" \
    SET_ROLE="$set_role"         ROLE="$role" \
    SET_BIO="$set_bio"           BIO="$bio" \
    SET_STATUS="$set_status"     STATUS="$status" \
    "$AIRC_PYTHON" -c '
import json, os
c = json.load(open(os.environ["CONFIG"]))
ident = c.setdefault("identity", {})
for key, env_set, env_val in [
    ("pronouns", "SET_PRONOUNS", "PRONOUNS"),
    ("role",     "SET_ROLE",     "ROLE"),
    ("bio",      "SET_BIO",      "BIO"),
    ("status",   "SET_STATUS",   "STATUS"),
]:
    if os.environ.get(env_set) == "1":
        v = os.environ.get(env_val, "").strip()
        if v:
            ident[key] = v
        else:
            ident.pop(key, None)
json.dump(c, open(os.environ["CONFIG"], "w"), indent=2)
print("  identity updated.")
'
}

_identity_link() {
  local platform="${1:-}" handle="${2:-}"
  [ -z "$platform" ] && die "Usage: airc identity link <platform> [handle] (omit/blank handle to unlink)"
  CONFIG="$CONFIG" PLATFORM="$platform" HANDLE="$handle" "$AIRC_PYTHON" -c '
import json, os
c = json.load(open(os.environ["CONFIG"]))
ints = c.setdefault("identity", {}).setdefault("integrations", {})
platform = os.environ["PLATFORM"]
handle = os.environ.get("HANDLE", "").strip()
if handle:
    ints[platform] = handle
    print(f"  linked: {platform} -> {handle}")
else:
    ints.pop(platform, None)
    print(f"  unlinked: {platform}")
json.dump(c, open(os.environ["CONFIG"], "w"), indent=2)
'
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
  # ideem-local-4bef + continuum-b741 surfaced 2026-04-28.
  if _whois_in_scope "$AIRC_WRITE_DIR" "$target"; then
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
  # get_config_val_in / airc_core.config (#152 Phase 1). Pre-migration
  # this function had six inline python heredocs reading individual
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
      local peer_id; peer_id=$(printf '%s' "$remote_blob" | "$AIRC_PYTHON" -m airc_core.handshake get_field identity "{}" 2>/dev/null || echo "{}")
      local peer_host; peer_host=$(printf '%s' "$remote_blob" | "$AIRC_PYTHON" -m airc_core.handshake get_field host "" 2>/dev/null || echo "")
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
  NAME="$name" BLOB="$blob" HOST="$host" python3 <<'PYEOF'
import json, os
name = os.environ["NAME"]
host = os.environ.get("HOST", "")
try:
    ident = json.loads(os.environ.get("BLOB", "{}") or "{}")
except Exception:
    ident = {}
print(f"  name:      {name}")
fields = [("pronouns", ident.get("pronouns", "")),
          ("role",     ident.get("role", "")),
          ("bio",      ident.get("bio", "")),
          ("status",   ident.get("status", ""))]
for k, v in fields:
    label = k + ":"
    fallback = "(unset)"
    print(f"  {label:<11} {v if v else fallback}")
ints = ident.get("integrations", {}) or {}
if ints:
    print("  integrations:")
    for k, v in ints.items():
        print(f"    {k}: {v}")
else:
    print("  integrations: (none)")
if host:
    print(f"  host:      {host}")
PYEOF
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
  BLOB="$blob" CONFIG="$CONFIG" "$AIRC_PYTHON" -c '
import json, os
try:
    src = json.loads(os.environ["BLOB"])
except Exception:
    src = {}
c = json.load(open(os.environ["CONFIG"]))
ident = c.setdefault("identity", {})
for k in ("pronouns", "role", "bio"):
    v = src.get(k)
    if v:
        ident[k] = v
ints = ident.setdefault("integrations", {})
ints["continuum"] = src.get("name", "")
json.dump(c, open(os.environ["CONFIG"], "w"), indent=2)
print(f"  imported continuum:{src.get(\"name\", \"?\")} → pronouns={src.get(\"pronouns\", \"\")} role={src.get(\"role\", \"\")} bio set={bool(src.get(\"bio\"))}")
'
}

_identity_push_continuum() {
  if ! command -v continuum >/dev/null 2>&1; then
    die "continuum CLI not on PATH — install continuum before pushing."
  fi
  local handle; handle=$(CONFIG="$CONFIG" "$AIRC_PYTHON" -c '
import json, os
c = json.load(open(os.environ["CONFIG"]))
print(c.get("identity", {}).get("integrations", {}).get("continuum", ""))
' 2>/dev/null)
  [ -z "$handle" ] && die "No continuum handle linked. Run: airc identity link continuum <name>"
  CONFIG="$CONFIG" HANDLE="$handle" "$AIRC_PYTHON" -c '
import json, os, subprocess
c = json.load(open(os.environ["CONFIG"]))
ident = c.get("identity", {})
handle = os.environ["HANDLE"]
args = ["continuum", "persona", "update", handle]
for k in ("pronouns", "role", "bio"):
    v = ident.get(k)
    if v:
        args += [f"--{k}", v]
res = subprocess.run(args, capture_output=True, text=True)
if res.returncode != 0:
    print(f"  continuum push failed: {res.stderr.strip() or res.stdout.strip()}")
    raise SystemExit(1)
print(f"  pushed local identity to continuum:{handle}")
'
}
