# Sourced by airc. cmd_queue — issue-backed work queue primitives (airc#562).
#
# Function exported back to airc's dispatch:
#   cmd_queue — subcommand router. Verbs:
#                 add        — create a new queue card (GitHub issue, airc-queue label). [PR-1]
#                 list       — list open queue cards on a repo (or auto-detected).       [PR-1]
#                 claim      — set owner+status on an existing card.                     [PR-2]
#                 release    — clear owner (back to claimable pool).                     [PR-2]
#                 set-status — change status field with enum validation.                 [PR-2]
#                 heartbeat  — stamp liveness on an owned card.
#                 stale      — list owned cards with missing/old heartbeats.
#                 next       — recommend claimable next work for idle agents.
#                 nudge      — surface a card OR repo-scoped status sweep.                [PR-3+]
#                 adopt      — convert an existing issue into a queue card.
#                 pongs      — summarize repo-nudge pong replies.
#                 availability — summarize queue owners + recent peer activity.
#                 close-merged — auto-close cards referenced by a merged PR.
#
# Verbs deferred to later PRs under airc#562:
#   - heartbeat + stall detection (PR-4)
#
# Why GitHub issues (consistent with cmd_knock / cmd_approve PR-1/PR-2):
#   1. GitHub already has the moderation surface (labels, close, spam).
#   2. Cards live where the work lives — same repo as the PR/issue they're
#      coordinating.
#   3. AIRC tooling is a CLIENT that creates/queries; no new substrate.
#   4. `gh` is already a hard dependency.
#
# Card body shape (mirrors continuum/.airc/QUEUE.md's per-card spec from
# sibling claude tab #1's continuum#1110):
#   - id              issue/PR number this card coordinates (or "self" if
#                     this queue issue IS the card)
#   - branch          feat/fix/chore/... (if PR-shaped)
#   - owner           AIRC handle (sub-tab disambiguated when needed)
#   - status          claimed / in-progress / blocked / review / merged
#   - blockers        comma-separated #NNNN (cross-repo OK: `airc#123`)
#   - env             mac-m5 / rtx5090-wsl2 / linux-amd64-any / any
#   - evidence        which gates ran + last sha
#   - next_action     one sentence
#   - last_heartbeat  ISO timestamp + commit sha
#
# The fields are intentionally LOOSE for v1 per Codex's guidance: schema-
# compat with forge-alloy settlement-event metadata but no hard lock on
# contract field names yet. PR-3/PR-4 will tighten as needed.
#
# External cross-references (resolved at call time):
#   die, resolve_name, AIRC_PYTHON; `gh` CLI.

cmd_queue() {
  # Top-level router. Validate + dispatch to _cmd_queue_<subcommand>.
  local subcmd="${1:-}"
  shift || true

  case "$subcmd" in
    -h|--help|"")
      _airc_queue_help
      [ -z "$subcmd" ] && return 1
      return 0
      ;;
    add)
      _cmd_queue_add "$@"
      ;;
    list|ls)
      _cmd_queue_list "$@"
      ;;
    claim)
      _cmd_queue_claim "$@"
      ;;
    release)
      _cmd_queue_release "$@"
      ;;
    set-status)
      _cmd_queue_set_status "$@"
      ;;
    heartbeat|touch)
      _cmd_queue_heartbeat "$@"
      ;;
    stale|stalled)
      _cmd_queue_stale "$@"
      ;;
    next|pick)
      _cmd_queue_next "$@"
      ;;
    nudge)
      _cmd_queue_nudge "$@"
      ;;
    adopt|import)
      _cmd_queue_adopt "$@"
      ;;
    pongs|pong-summary)
      _cmd_queue_pongs "$@"
      ;;
    availability|avail)
      _cmd_queue_availability "$@"
      ;;
    close-merged)
      _cmd_queue_close_merged "$@"
      ;;
    *)
      die "queue: unknown subcommand: $subcmd (try: add, list, claim, release, set-status, heartbeat, stale, next, nudge, adopt, pongs, availability, close-merged)"
      ;;
  esac
}

_cmd_queue_add() {
  # Create a new airc-queue card. Args:
  #   airc queue add <owner/repo> --title "<one-line>" [card-fields...]
  #
  # All card fields are flag-driven so `airc queue add owner/repo` followed
  # by --owner, --status, etc. is unambiguous (vs trying to encode position).

  local target_repo=""
  local title=""
  local card_id=""
  local card_branch=""
  local card_owner=""
  local card_status="claimed"
  local card_blockers=""
  local card_env=""
  local card_evidence=""
  local card_next_action=""
  local card_last_heartbeat=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_add_help
        return 0
        ;;
      --title)         shift; title="${1:-}" ;;
      --id)            shift; card_id="${1:-}" ;;
      --branch)        shift; card_branch="${1:-}" ;;
      --owner)         shift; card_owner="${1:-}" ;;
      --status)        shift; card_status="${1:-}" ;;
      --blockers)      shift; card_blockers="${1:-}" ;;
      --env)           shift; card_env="${1:-}" ;;
      --evidence)      shift; card_evidence="${1:-}" ;;
      --next-action)   shift; card_next_action="${1:-}" ;;
      --last-heartbeat) shift; card_last_heartbeat="${1:-}" ;;
      --dry-run)       dry_run=1 ;;
      -*) die "queue add: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue add: too many positional args. Got extra: $1 (use --title for the title)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ] || [ -z "$title" ]; then
    _airc_queue_add_help >&2
    return 1
  fi

  case "$target_repo" in
    */*) : ;;
    *) die "queue add: target must be owner/repo (e.g. CambrianTech/continuum), got: $target_repo" ;;
  esac

  # Status enum guard — keep the loose-but-checked. Operators get a clear
  # rejection for typos like "in-flight" → must be one of the canonical 5.
  case "$card_status" in
    claimed|in-progress|blocked|review|merged) : ;;
    *) die "queue add: --status must be one of: claimed, in-progress, blocked, review, merged (got: $card_status)" ;;
  esac

  # Default owner = the airc handle running this command. Sub-tab
  # disambiguation belongs to the operator if they share an airc handle
  # across multiple agents (today's pattern: claude tab #1 vs claude tab #2).
  if [ -z "$card_owner" ]; then
    card_owner=$(_airc_queue_resolve_name)
  fi

  # The airc-queue label and JSON envelope identify queue cards. Preserve the
  # caller's title exactly so GitHub Projects/Kanban stays readable.
  local issue_title="$title"
  local issue_body
  issue_body=$(_airc_queue_card_body \
    "$card_id" "$card_branch" "$card_owner" "$card_status" \
    "$card_blockers" "$card_env" "$card_evidence" \
    "$card_next_action" "$card_last_heartbeat")

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would post queue card:\n'
    printf '  repo:    %s\n' "$target_repo"
    printf '  title:   %s\n' "$issue_title"
    printf '  body:\n'
    printf '%s\n' "$issue_body" | sed 's/^/    /'
    return 0
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue add: 'gh' CLI is required. Install: https://cli.github.com/  Then: gh auth login"
  fi

  # Try with airc-queue label first; fall back to no label if it doesn't
  # exist yet on the target repo (same pattern as cmd_knock).
  # Body goes via --body-file (lib_gh.sh) — card bodies routinely embed
  # ```json``` fences and a status log of backticked refs (airc#571).
  local issue_url
  if issue_url=$(_airc_gh_safe_body "$issue_body" issue create \
    --repo "$target_repo" \
    --title "$issue_title" \
    --label "airc-queue"); then
    :
  elif issue_url=$(_airc_gh_safe_body "$issue_body" issue create \
    --repo "$target_repo" \
    --title "$issue_title"); then
    printf 'note: %s does not have an "airc-queue" label yet. Card posted without one.\n' "$target_repo" >&2
  else
    die "queue add: gh issue create failed: $issue_url"
  fi

  printf 'Queue card created: %s\n' "$issue_url"
}

_cmd_queue_list() {
  # List open airc-queue cards on a target repo. Defaults to the current
  # working directory's git remote if no --repo specified.

  local target_repo=""
  local filter_owner=""
  local filter_status=""
  local output_json=0
  local limit=30

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_list_help
        return 0
        ;;
      --repo)     shift; target_repo="${1:-}" ;;
      --owner)    shift; filter_owner="${1:-}" ;;
      --status)   shift; filter_status="${1:-}" ;;
      --limit)    shift; limit="${1:-30}" ;;
      --json)     output_json=1 ;;
      -*) die "queue list: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue list: too many positional args (use --owner / --status to filter)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    # Try to auto-detect from git remote in cwd.
    if target_repo=$(_airc_queue_detect_repo_from_cwd 2>/dev/null) && [ -n "$target_repo" ]; then
      :
    else
      die "queue list: no <owner/repo> given and could not detect one from \$PWD's git remote. Pass --repo owner/repo."
    fi
  fi

  case "$target_repo" in
    */*) : ;;
    *) die "queue list: target must be owner/repo, got: $target_repo" ;;
  esac

  if ! command -v gh >/dev/null 2>&1; then
    die "queue list: 'gh' CLI is required."
  fi

  # Pull all open airc-queue issues. We do filtering client-side rather
  # than via gh search because gh's search doesn't introspect issue body
  # contents — and our card fields live in the body.
  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,author,createdAt,updatedAt 2>&1); then
    die "queue list: gh issue list failed: $raw_json"
  fi

  # Parse + filter + render via python (more robust than bash jq + grep
  # gymnastics on multi-line bodies). The issue JSON goes through a temp
  # file because `python - <<'PYEOF'` already consumes stdin for the script.
  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-list.XXXXXX") || die "queue list: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  "$AIRC_PYTHON" - \
      "$target_repo" "$filter_owner" "$filter_status" "$output_json" "$raw_json_file" \
      <<'PYEOF'
import datetime, json, re, sys
repo = sys.argv[1]
filter_owner = sys.argv[2]
filter_status = sys.argv[3]
output_json = sys.argv[4] == "1"
raw_json_file = sys.argv[5]
now_utc = datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z")

with open(raw_json_file, "r", encoding="utf-8") as f:
    data = json.loads(f.read())

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)

def parse_card(body: str) -> dict:
    """Find the first ```json``` block that looks like a queue card."""
    for match in CARD_BLOCK_RE.finditer(body or ""):
        try:
            parsed = json.loads(match.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            return parsed
    return {}

cards = []
for issue in data:
    card = parse_card(issue.get("body", ""))
    if filter_owner and card.get("owner", "") != filter_owner:
        continue
    if filter_status and card.get("status", "") != filter_status:
        continue
    cards.append({
        "number": issue.get("number"),
        "title": issue.get("title", "").replace("airc-queue: ", "", 1),
        "url": issue.get("url"),
        "createdAt": issue.get("createdAt"),
        "updatedAt": issue.get("updatedAt"),
        "card": card,
    })

if output_json:
    print(json.dumps({"now_utc": now_utc, "repo": repo, "cards": cards}, indent=2))
else:
    if not cards:
        suffix = ""
        if filter_owner: suffix += f" owner={filter_owner}"
        if filter_status: suffix += f" status={filter_status}"
        print(f"# airc-queue — {repo}")
        print(f"now_utc: {now_utc}")
        print(f"No open airc-queue cards on {repo}{suffix}.")
        sys.exit(0)
    print(f"# airc-queue — {repo} ({len(cards)} open)")
    print(f"now_utc: {now_utc}")
    for entry in cards:
        c = entry["card"]
        print()
        print(f"## #{entry['number']} — {entry['title']}")
        print(f"  url:           {entry['url']}")
        if c.get("id"):              print(f"  id:            {c['id']}")
        if c.get("branch"):          print(f"  branch:        {c['branch']}")
        if c.get("owner"):           print(f"  owner:         {c['owner']}")
        if c.get("status"):          print(f"  status:        {c['status']}")
        if c.get("blockers"):        print(f"  blockers:      {c['blockers']}")
        if c.get("env"):             print(f"  env:           {c['env']}")
        if c.get("evidence"):        print(f"  evidence:      {c['evidence']}")
        if c.get("next_action"):     print(f"  next:          {c['next_action']}")
        if c.get("last_heartbeat"):  print(f"  last heartbeat:{c['last_heartbeat']}")
PYEOF
  local py_status=$?
  rm -f "$raw_json_file"
  return "$py_status"
}

_airc_queue_help() {
  cat <<'EOF'
airc queue — issue-backed work queue primitives (airc#562)

USAGE
  airc queue add <owner/repo> --title "<one-line>" [card-fields...]
  airc queue list [<owner/repo>] [--owner X] [--status Y] [--limit N] [--json]
  airc queue claim <issue-url> [--owner X] [--status Y]
  airc queue release <issue-url> [--reason "..."] [--status claimed|blocked]
  airc queue set-status <issue-url> <state>
  airc queue heartbeat <issue-url> [--owner X] [--status Y] [--note "..."]
  airc queue stale <owner/repo> [--stale-after 30m]
  airc queue next [<owner/repo>] [--limit N] [--json]
  airc queue nudge <issue-url> [--peer @handle] [--message "..."]
  airc queue nudge <owner/repo> [--message "..."] [--limit N]
  airc queue adopt <issue-url> [card-fields...] [--force]
  airc queue pongs <owner/repo> [--since 30m] [--sweep-id ID]
  airc queue availability <owner/repo> [--since 30m] [--stale-after 30m]
  airc queue close-merged <pr-url> [--merge-sha SHA] [--actor X] [--dry-run]

DESCRIPTION
  Adds, lists, or mutates queue cards (GitHub issues with airc-queue
  label). Card fields follow the spec in continuum/.airc/QUEUE.md
  (sibling claude tab #1's continuum#1110).

VERB SCOPE
  add / list                   PR-1 (airc#566, merged)
  claim / release / set-status PR-2 (airc#568, merged)
  heartbeat / stale            Stale-claim liveness primitives (airc#572)
  next / pick                  Idle-agent next-work recommendations
  nudge                        PR-3 (card) + PR-4a (repo ping/pong sweep)
  adopt / import               Backlog migration (airc#575)
  pongs / pong-summary          Repo-nudge response collection (airc#579)
  availability / avail          Live peer + stale-claim summary (airc#591)
  close-merged                 airc#576 — auto-close on PR merge to canary
  auto-release policy          Deferred; stale is read-only first

EOF
}

_airc_queue_add_help() {
  cat <<'EOF'
airc queue add — create a new queue card

USAGE
  airc queue add <owner/repo> --title "<one-line>" [card-fields...] [--dry-run]

REQUIRED
  <owner/repo>           Target GitHub repo (e.g. CambrianTech/continuum)
  --title "<text>"       One-line card title

CARD FIELDS (all optional; defaults shown)
  --id <ref>             Issue/PR this card coordinates (e.g. #1085, airc#562)
  --branch <name>        Branch name (e.g. fix/install-tier-name)
  --owner <handle>       Queue owner (default: current work identity from
                         `airc identity whoami`)
  --status <state>       claimed | in-progress | blocked | review | merged
                         (default: claimed)
  --blockers <list>      Comma-separated #NNNN (e.g. "#1085, airc#559")
  --env <tag>            mac-m5 | rtx5090-wsl2 | linux-amd64-any | any
  --evidence <text>      Gates run + sha (e.g. "prepush 61bdeb407: 27/27")
  --next-action <text>   One sentence on next step
  --last-heartbeat <ts>  ISO timestamp + sha (e.g. "2026-05-13T17:35Z @ 61bdeb407")

OPTIONS
  --dry-run              Print the card body that WOULD be posted; don't post.
  -h, --help             This help.

ENVIRONMENT
  AIRC_QUEUE_OWNER        Compatibility fallback for older launchers.
  AIRC_AGENT_NAME/NICK    Compatibility fallback for older launchers.

EXAMPLES
  airc queue add CambrianTech/continuum \\
    --title "Implement Lane B-Mac MetalMonitor adapter" \\
    --owner "claude-tab-2" \\
    --branch "feat/lane-c-mac-metal-adapter" \\
    --env "mac-m5" \\
    --status "claimed" \\
    --next-action "Wait for RTX substrate schema then wire MetalMonitor into seam metadata"

NOTES
  - 'gh' CLI must be authenticated.
  - The 'airc-queue' label is auto-applied if it exists on the target
    repo; otherwise the issue posts without one and a hint suggests
    creating it.
EOF
}

_airc_queue_list_help() {
  cat <<'EOF'
airc queue list — list open queue cards

USAGE
  airc queue list [<owner/repo>] [--owner X] [--status Y] [--limit N] [--json]

ARGUMENTS
  <owner/repo>           Target GitHub repo. If omitted, auto-detected
                         from the current directory's git remote.

OPTIONS
  --repo <owner/repo>    Alternative way to specify repo (vs positional).
  --owner <handle>       Filter to cards owned by this handle.
  --status <state>       Filter to cards in this state.
  --limit <N>            Max cards to fetch (default 30; gh hard cap 100).
  --json                 Emit JSON instead of human-readable text.
  -h, --help             This help.

EXAMPLES
  airc queue list CambrianTech/continuum
  airc queue list --status in-progress
  airc queue list --owner claude-tab-2 --json | jq '.[] | .url'

NOTES
  - Lists only OPEN airc-queue issues (closed = merged/done in PR-1).
  - Filters apply client-side after fetching matching issues by label.
EOF
}

_airc_queue_resolve_name() {
  # Best-effort queue owner for the current agent/session. Queue ownership
  # is not always identical to transport identity: multiple local agents may
  # share one AIRC scope/room handle while working separate cards. The
  # first-class session identity owns product behavior; env names remain
  # a compatibility path inside the identity resolver.
  if declare -F _identity_resolve_work_name >/dev/null 2>&1; then
    _identity_resolve_work_name
    return 0
  fi

  # Falls back to "anonymous" if no scope (cmd_queue must work pre-init too —
  # outsiders may want to query/add cards before joining).
  if declare -F resolve_name >/dev/null 2>&1; then
    resolve_name
  else
    echo "anonymous"
  fi
}

_airc_queue_heartbeat_value() {
  local ts sha
  ts=$(date -u +"%Y-%m-%dT%H:%MZ")
  sha=$(git rev-parse --short HEAD 2>/dev/null || true)
  if [ -n "$sha" ]; then
    printf '%s @ %s\n' "$ts" "$sha"
  else
    printf '%s\n' "$ts"
  fi
}

_airc_queue_detect_repo_from_cwd() {
  # Try to extract owner/repo from `git remote get-url origin`.
  # Returns non-zero if not in a git repo or remote isn't a known shape.
  local url
  if ! url=$(git config --get remote.origin.url 2>/dev/null) || [ -z "$url" ]; then
    return 1
  fi
  # Match https://github.com/owner/repo(.git) and git@github.com:owner/repo(.git).
  if [[ "$url" =~ github\.com[:/]([^/]+)/([^/]+)(\.git)?$ ]]; then
    local owner="${BASH_REMATCH[1]}"
    local repo="${BASH_REMATCH[2]%.git}"
    printf '%s/%s' "$owner" "$repo"
    return 0
  fi
  return 1
}

_airc_queue_card_body() {
  # Build the issue body. Markdown for humans + a JSON envelope for tooling.
  # Field args (positional, in order): id, branch, owner, status, blockers,
  # env, evidence, next_action, last_heartbeat.
  local id="$1" branch="$2" owner="$3" status="$4"
  local blockers="$5" env="$6" evidence="$7"
  local next_action="$8" last_heartbeat="$9"

  # Build the JSON envelope via python so we get correct escaping for
  # weird characters in evidence/next_action (operator-supplied free text).
  local card_json
  card_json=$("$AIRC_PYTHON" - "$id" "$branch" "$owner" "$status" \
      "$blockers" "$env" "$evidence" "$next_action" "$last_heartbeat" \
      <<'PYEOF'
import json, sys
keys = ["id","branch","owner","status","blockers","env","evidence","next_action","last_heartbeat"]
values = sys.argv[1:1+len(keys)]
card = {"kind": "airc-queue-card-v1"}
for k, v in zip(keys, values):
    if v:
        card[k] = v
print(json.dumps(card, indent=2))
PYEOF
)

  # Body uses printf rather than heredoc to dodge bash heredoc parser
  # edge cases (apostrophes inside $() — same trap that bit cmd_approve).
  printf '**airc-queue card**\n\n%s\n\n```json\n%s\n```\n\n%s\n' \
    'Coordinates work via the AIRC queue substrate (airc#562). Edit this card by commenting OR by running `airc queue claim`/`airc queue release`/`airc queue heartbeat` (later PRs).' \
    "$card_json" \
    'Close this issue when the work is done (status=merged/abandoned).'
}

# ──────────────────────────────────────────────────────────────────────
# PR-2: claim / release / set-status
# ──────────────────────────────────────────────────────────────────────
#
# These verbs MUTATE an existing card body in place. The shape of the
# operation is the same in all three:
#   1. Resolve <issue-url> to (repo, issue_num).
#   2. Fetch the current body via gh issue view.
#   3. Parse the JSON envelope (kind=airc-queue-card-v1) out of the body.
#   4. Mutate one or more fields (owner, status, possibly others later).
#   5. Append a one-line entry to the body's "## Status log" section so
#      the card carries chronological history readable to humans + tools.
#   6. Write the updated body back via gh issue edit.
#
# Why update-in-place (vs comments): tooling (`airc queue list`) reads
# the JSON envelope from the body; cards stay scannable at a glance
# without parsing every comment. The status log preserves history for
# operators reading the issue page.
#
# Design notes:
#   - All three verbs accept --dry-run for envelope preview.
#   - Owner defaults to current scope's resolve_name on `claim` (no flag
#     needed for self-claim — the common case).
#   - `release` clears owner field entirely (signals "unclaimed pool")
#     and sets status to "claimed" if it was "in-progress" (the only
#     status that implies someone is actively working).
#   - `set-status merged` is the natural "I'm done" signal but does NOT
#     close the issue automatically — operators close manually so the
#     queue tracks the closure event explicitly.

_cmd_queue_claim() {
  local issue_url=""
  local new_owner=""
  local new_status="in-progress"
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_claim_help
        return 0
        ;;
      --owner)    shift; new_owner="${1:-}" ;;
      --status)   shift; new_status="${1:-}" ;;
      --dry-run)  dry_run=1 ;;
      -*) die "queue claim: unknown flag: $1" ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "queue claim: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ]; then
    _airc_queue_claim_help >&2
    return 1
  fi

  # Default owner = current scope's airc handle. Sub-tab disambiguation
  # is the operator's job (claude tab #1 vs claude tab #2 today).
  if [ -z "$new_owner" ]; then
    new_owner=$(_airc_queue_resolve_name)
  fi

  case "$new_status" in
    claimed|in-progress|blocked|review|merged) : ;;
    *) die "queue claim: --status must be one of: claimed, in-progress, blocked, review, merged (got: $new_status)" ;;
  esac

  local heartbeat
  heartbeat=$(_airc_queue_heartbeat_value)

  _airc_queue_mutate_card "$issue_url" "$dry_run" \
    "claim by $new_owner -> status=$new_status" \
    --set "owner=$new_owner" \
    --set "status=$new_status" \
    --set "last_heartbeat=$heartbeat"
}

_cmd_queue_release() {
  local issue_url=""
  local reason=""
  local revert_to_status="claimed"
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_release_help
        return 0
        ;;
      --reason)   shift; reason="${1:-}" ;;
      --status)   shift; revert_to_status="${1:-}" ;;
      --dry-run)  dry_run=1 ;;
      -*) die "queue release: unknown flag: $1" ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "queue release: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ]; then
    _airc_queue_release_help >&2
    return 1
  fi

  case "$revert_to_status" in
    claimed|blocked) : ;;
    *) die "queue release: --status on release must be one of: claimed, blocked (got: $revert_to_status). Use set-status for in-progress/review/merged." ;;
  esac

  local releaser
  releaser=$(_airc_queue_resolve_name)
  local log_msg="released by $releaser -> status=$revert_to_status"
  if [ -n "$reason" ]; then
    log_msg="$log_msg ($reason)"
  fi

  _airc_queue_mutate_card "$issue_url" "$dry_run" \
    "$log_msg" \
    --clear owner \
    --set "status=$revert_to_status"
}

_cmd_queue_set_status() {
  local issue_url=""
  local new_status=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_set_status_help
        return 0
        ;;
      --dry-run)  dry_run=1 ;;
      -*) die "queue set-status: unknown flag: $1" ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        elif [ -z "$new_status" ]; then
          new_status="$1"
        else
          die "queue set-status: too many positional args (use: queue set-status <url> <state>)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ] || [ -z "$new_status" ]; then
    _airc_queue_set_status_help >&2
    return 1
  fi

  case "$new_status" in
    claimed|in-progress|blocked|review|merged) : ;;
    *) die "queue set-status: <state> must be one of: claimed, in-progress, blocked, review, merged (got: $new_status)" ;;
  esac

  local actor
  actor=$(_airc_queue_resolve_name)

  _airc_queue_mutate_card "$issue_url" "$dry_run" \
    "$actor -> status=$new_status" \
    --set "status=$new_status"
}

_cmd_queue_heartbeat() {
  local issue_url=""
  local owner=""
  local status=""
  local note=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_heartbeat_help
        return 0
        ;;
      --owner)   shift; owner="${1:-}" ;;
      --status)  shift; status="${1:-}" ;;
      --note)    shift; note="${1:-}" ;;
      --dry-run) dry_run=1 ;;
      -*) die "queue heartbeat: unknown flag: $1" ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "queue heartbeat: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ]; then
    _airc_queue_heartbeat_help >&2
    return 1
  fi

  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  if [ -n "$status" ]; then
    case "$status" in
      claimed|in-progress|blocked|review|merged) : ;;
      *) die "queue heartbeat: --status must be one of: claimed, in-progress, blocked, review, merged (got: $status)" ;;
    esac
  fi

  local heartbeat log_msg
  heartbeat=$(_airc_queue_heartbeat_value)
  log_msg="heartbeat by $owner"
  if [ -n "$status" ]; then
    log_msg="$log_msg -> status=$status"
  fi
  if [ -n "$note" ]; then
    log_msg="$log_msg ($note)"
  fi

  if [ -n "$status" ]; then
    _airc_queue_mutate_card "$issue_url" "$dry_run" \
      "$log_msg" \
      --set "owner=$owner" \
      --set "last_heartbeat=$heartbeat" \
      --set "status=$status"
  else
    _airc_queue_mutate_card "$issue_url" "$dry_run" \
      "$log_msg" \
      --set "owner=$owner" \
      --set "last_heartbeat=$heartbeat"
  fi
}

_cmd_queue_stale() {
  local target_repo=""
  local stale_after="30m"
  local limit=50
  local output_json=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_stale_help
        return 0
        ;;
      --repo)        shift; target_repo="${1:-}" ;;
      --stale-after) shift; stale_after="${1:-}" ;;
      --limit)       shift; limit="${1:-}" ;;
      --json)        output_json=1 ;;
      -*) die "queue stale: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue stale: too many positional args (use: queue stale <owner/repo> [--stale-after 30m])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue stale: no <owner/repo> given and could not detect one from \$PWD's git remote. Pass --repo owner/repo."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue stale: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue stale: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue stale: --limit must be >= 1 (got: $limit)"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue stale: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list --repo "$target_repo" --label airc-queue --state open --limit "$limit" \
    --json number,title,url,body,updatedAt 2>&1); then
    die "queue stale: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-stale.XXXXXX") || die "queue stale: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  "$AIRC_PYTHON" - "$target_repo" "$stale_after" "$output_json" "$raw_json_file" <<'PYEOF'
import json, re, sys
from datetime import datetime, timezone, timedelta

repo, stale_after, output_json_s, path = sys.argv[1:5]
output_json = output_json_s == "1"

def parse_duration(value: str) -> timedelta:
    m = re.fullmatch(r"\s*(\d+)\s*([smhd])\s*", value or "")
    if not m:
        print(f"queue stale: cannot parse --stale-after '{value}' (use 30m, 2h, 1d)", file=sys.stderr)
        sys.exit(2)
    amount = int(m.group(1))
    unit = m.group(2)
    if unit == "s":
        return timedelta(seconds=amount)
    if unit == "m":
        return timedelta(minutes=amount)
    if unit == "h":
        return timedelta(hours=amount)
    return timedelta(days=amount)

def parse_card(body: str):
    for m in re.finditer(r"```json\s*\n(.*?)\n\s*```", body or "", re.DOTALL):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            return parsed
    return None

def parse_heartbeat(value: str):
    if not value:
        return None
    m = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?Z)", value)
    if not m:
        return None
    raw = m.group(1)
    fmt = "%Y-%m-%dT%H:%M:%SZ" if raw.count(":") == 2 else "%Y-%m-%dT%H:%MZ"
    return datetime.strptime(raw, fmt).replace(tzinfo=timezone.utc)

with open(path, "r", encoding="utf-8") as f:
    issues = json.load(f)

threshold = parse_duration(stale_after)
now = datetime.now(timezone.utc)
rows = []
for issue in issues:
    card = parse_card(issue.get("body", ""))
    if not card:
        continue
    status = (card.get("status") or "").strip()
    owner = (card.get("owner") or "").strip()
    if status not in {"claimed", "in-progress", "review"}:
        continue
    reason = ""
    heartbeat = (card.get("last_heartbeat") or "").strip()
    hb_dt = parse_heartbeat(heartbeat)
    age_seconds = None
    if not owner:
        reason = "missing-owner"
    elif not hb_dt:
        reason = "missing-heartbeat"
    else:
        age = now - hb_dt
        age_seconds = int(age.total_seconds())
        if age > threshold:
            reason = "stale-heartbeat"
    if not reason:
        continue
    rows.append({
        "number": issue.get("number"),
        "title": issue.get("title") or "",
        "url": issue.get("url") or "",
        "status": status or "unknown",
        "owner": owner,
        "last_heartbeat": heartbeat,
        "age_seconds": age_seconds,
        "reason": reason,
        "next_action": card.get("next_action") or "",
    })

if output_json:
    print(json.dumps({"repo": repo, "now": now.isoformat().replace("+00:00", "Z"), "stale_after": stale_after, "cards": rows}, indent=2))
else:
    print(f"# airc-queue stale — {repo}")
    print(f"now_utc: {now.isoformat().replace('+00:00', 'Z')}")
    print(f"stale_after: {stale_after}")
    if not rows:
        print("No stale owned cards found.")
    for row in rows:
        print()
        print(f"## #{row['number']} — {row['title']}")
        print(f"  url:            {row['url']}")
        print(f"  status:         {row['status']}")
        if row["owner"]:
            print(f"  owner:          {row['owner']}")
        if row["last_heartbeat"]:
            print(f"  last heartbeat: {row['last_heartbeat']}")
        if row["age_seconds"] is not None:
            print(f"  heartbeat age:  {row['age_seconds']}s")
        print(f"  reason:         {row['reason']}")
        if row["next_action"]:
            print(f"  next:           {row['next_action']}")
PYEOF
  local py_status=$?
  rm -f "$raw_json_file"
  return "$py_status"
}

_cmd_queue_next() {
  # Recommend next claimable work for an idle agent. This is intentionally
  # action-shaped rather than dashboard-shaped: every row includes the exact
  # claim and lane commands an agent can run next.
  local target_repo=""
  local limit=30
  local output_json=0
  local idle_ping=0
  local owner=""
  local base="canary"
  local repo_root=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_next_help
        return 0
        ;;
      --repo)      shift; target_repo="${1:-}" ;;
      --limit)     shift; limit="${1:-30}" ;;
      --owner)     shift; owner="${1:-}" ;;
      --base)      shift; base="${1:-canary}" ;;
      --repo-root) shift; repo_root="${1:-}" ;;
      --idle-ping) idle_ping=1 ;;
      --json)      output_json=1 ;;
      -*) die "queue next: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue next: too many positional args (use: queue next <owner/repo>)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue next: no <owner/repo> given and could not detect one from \$PWD's git remote. Pass --repo owner/repo."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue next: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue next: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue next: --limit must be >= 1 (got: $limit)"
  fi

  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue next: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt 2>&1); then
    die "queue next: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-next.XXXXXX") || die "queue next: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  AIRC_QUEUE_NEXT_OWNER="$owner" \
  AIRC_QUEUE_NEXT_BASE="$base" \
  AIRC_QUEUE_NEXT_REPO_ROOT="$repo_root" \
  "$AIRC_PYTHON" - "$target_repo" "$output_json" "$raw_json_file" <<'PYEOF'
import json, os, re, sys

repo, output_json_raw, path = sys.argv[1:4]
output_json = output_json_raw == "1"
owner = os.environ.get("AIRC_QUEUE_NEXT_OWNER", "anonymous")
base = os.environ.get("AIRC_QUEUE_NEXT_BASE", "canary")
repo_root = os.environ.get("AIRC_QUEUE_NEXT_REPO_ROOT", "")

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)

def parse_card(body: str):
    for m in CARD_BLOCK_RE.finditer(body or ""):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            return parsed
    return None

def shquote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"

def score(status: str, card_owner: str) -> int:
    if status == "claimed" and not card_owner:
        return 0
    if status == "claimed":
        return 1
    if status == "blocked" and not card_owner:
        return 2
    if status == "review":
        return 3
    if status == "in-progress" and card_owner == owner:
        return 4
    return 9

with open(path, "r", encoding="utf-8") as f:
    issues = json.load(f)

rows = []
for issue in issues:
    card = parse_card(issue.get("body", "") or "")
    if not card:
        continue
    status = (card.get("status") or "claimed").strip()
    card_owner = (card.get("owner") or "").strip()
    rank = score(status, card_owner)
    if rank >= 9:
        continue
    ref = f"{repo}#{issue.get('number')}"
    branch = (card.get("branch") or "").strip()
    lane_cmd = f"airc lane create {shquote(ref)} --base {shquote(base)}"
    if branch:
        lane_cmd += f" --branch {shquote(branch)}"
    if repo_root:
        lane_cmd += f" --repo {shquote(repo_root)}"
    claim_cmd = f"airc queue claim {shquote(ref)} --owner {shquote(owner)}"
    rows.append({
        "rank": rank,
        "number": issue.get("number"),
        "title": (issue.get("title") or "").replace("airc-queue: ", "", 1),
        "url": issue.get("url") or "",
        "ref": ref,
        "status": status,
        "owner": card_owner,
        "branch": branch,
        "env": card.get("env") or "",
        "next_action": card.get("next_action") or "",
        "claim_command": claim_cmd,
        "lane_command": lane_cmd,
    })

rows.sort(key=lambda r: (r["rank"], r["number"] or 0))
payload = {"repo": repo, "owner": owner, "candidates": rows}

if output_json:
    print(json.dumps(payload, indent=2))
else:
    print(f"# airc-queue next — {repo}")
    print(f"owner: {owner}")
    if not rows:
        print("No claimable queue cards found.")
        print(f"Try: airc queue nudge {repo} --message \"idle agent looking for work\"")
    for idx, row in enumerate(rows[:10], start=1):
        owner_label = row["owner"] or "(unowned)"
        print()
        print(f"## {idx}. {row['ref']} — {row['title']}")
        print(f"  status: {row['status']} owner={owner_label}")
        if row["env"]:
            print(f"  env:    {row['env']}")
        if row["branch"]:
            print(f"  branch: {row['branch']}")
        if row["next_action"]:
            print(f"  next:   {row['next_action']}")
        print(f"  claim:  {row['claim_command']}")
        print(f"  lane:   {row['lane_command']}")
PYEOF
  local py_status=$?
  rm -f "$raw_json_file"
  if [ "$py_status" -ne 0 ]; then
    return "$py_status"
  fi

  if [ "$idle_ping" -eq 1 ]; then
    local ping_text="idle: ${owner} is looking for next work in ${target_repo}; ran airc queue next. Agents should claim a card or create one if they see missing work."
    if [ "$output_json" -eq 1 ]; then
      printf 'idle_ping: %s\n' "$ping_text" >&2
    fi
    if ! cmd_send "$ping_text"; then
      die "queue next: idle ping failed"
    fi
  fi
}

_cmd_queue_adopt() {
  # Convert an existing GitHub issue into an airc-queue card in place.
  # This is the backlog migration path: no duplicate card, no lost issue
  # context. The queue envelope is prepended and the original body is kept
  # under a details block for humans and future tooling.
  local issue_url=""
  local card_id=""
  local card_branch=""
  local card_owner=""
  local card_status="claimed"
  local card_blockers=""
  local card_env=""
  local card_evidence=""
  local card_next_action=""
  local card_last_heartbeat=""
  local dry_run=0
  local force=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_adopt_help
        return 0
        ;;
      --id)             shift; card_id="${1:-}" ;;
      --branch)         shift; card_branch="${1:-}" ;;
      --owner)          shift; card_owner="${1:-}" ;;
      --status)         shift; card_status="${1:-}" ;;
      --blockers)       shift; card_blockers="${1:-}" ;;
      --env)            shift; card_env="${1:-}" ;;
      --evidence)       shift; card_evidence="${1:-}" ;;
      --next-action)    shift; card_next_action="${1:-}" ;;
      --last-heartbeat) shift; card_last_heartbeat="${1:-}" ;;
      --force)          force=1 ;;
      --dry-run)        dry_run=1 ;;
      -*) die "queue adopt: unknown flag: $1" ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "queue adopt: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ]; then
    _airc_queue_adopt_help >&2
    return 1
  fi

  case "$card_status" in
    claimed|in-progress|blocked|review|merged) : ;;
    *) die "queue adopt: --status must be one of: claimed, in-progress, blocked, review, merged (got: $card_status)" ;;
  esac

  local parsed_issue repo issue_num
  if ! parsed_issue=$(_airc_queue_parse_issue_url "$issue_url"); then
    die "queue adopt: <issue-url> must be a GitHub issue URL or owner/repo#N (got: $issue_url)"
  fi
  repo="${parsed_issue%#*}"
  issue_num="${parsed_issue##*#}"

  if [ -z "$card_id" ]; then
    card_id="#$issue_num"
  fi
  if [ -z "$card_owner" ]; then
    card_owner=$(_airc_queue_resolve_name)
  fi
  if [ -z "$card_evidence" ]; then
    card_evidence="Adopted existing GitHub issue into airc queue."
  fi
  if [ -z "$card_next_action" ]; then
    card_next_action="Triage, claim, or close this adopted backlog card."
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue adopt: 'gh' CLI is required."
  fi

  local issue_json
  if ! issue_json=$(gh issue view "$issue_num" --repo "$repo" --json title,body 2>&1); then
    die "queue adopt: gh issue view failed for $repo#$issue_num: $issue_json"
  fi

  local issue_json_file body_file
  issue_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-adopt-json.XXXXXX") || die "queue adopt: mktemp failed"
  body_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-adopt-body.XXXXXX") || die "queue adopt: mktemp failed"
  printf '%s' "$issue_json" >"$issue_json_file"

  local queue_body
  queue_body=$(_airc_queue_card_body \
    "$card_id" "$card_branch" "$card_owner" "$card_status" \
    "$card_blockers" "$card_env" "$card_evidence" \
    "$card_next_action" "$card_last_heartbeat")
  printf '%s' "$queue_body" >"$body_file"

  local adopted_body
  adopted_body=$("$AIRC_PYTHON" - "$issue_json_file" "$body_file" "$force" <<'PYEOF'
import json, re, sys
issue_json_path, queue_body_path, force_raw = sys.argv[1:4]
force = force_raw == "1"

with open(issue_json_path, "r", encoding="utf-8") as f:
    issue = json.loads(f.read())
with open(queue_body_path, "r", encoding="utf-8") as f:
    queue_body = f.read().rstrip()

old_body = issue.get("body") or ""
CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
for m in CARD_BLOCK_RE.finditer(old_body):
    try:
        parsed = json.loads(m.group(1).strip())
    except Exception:
        continue
    if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
        if not force:
            print("queue adopt: issue already has an airc-queue-card-v1 envelope; pass --force to rewrite", file=sys.stderr)
            sys.exit(3)
        break

if old_body.strip():
    original = "\n\n## Original issue body\n\n<details>\n<summary>Pre-adoption body</summary>\n\n" + old_body.rstrip() + "\n\n</details>\n"
else:
    original = "\n\n## Original issue body\n\n_No pre-adoption body._\n"

print(queue_body + original, end="")
PYEOF
)
  local adopt_rc=$?
  if [ "$adopt_rc" -ne 0 ]; then
    rm -f "$issue_json_file" "$body_file"
    printf '%s\n' "$adopted_body" >&2
    return "$adopt_rc"
  fi
  rm -f "$issue_json_file" "$body_file"

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would adopt %s#%s into airc queue:\n' "$repo" "$issue_num"
    printf '  id:      %s\n' "$card_id"
    printf '  owner:   %s\n' "$card_owner"
    printf '  status:  %s\n' "$card_status"
    printf '  new body:\n'
    printf '%s\n' "$adopted_body" | sed 's/^/    /'
    return 0
  fi

  local edit_out
  if ! edit_out=$(_airc_gh_safe_body "$adopted_body" issue edit "$issue_num" \
    --repo "$repo"); then
    die "queue adopt: gh issue edit failed for $repo#$issue_num: $edit_out"
  fi

  local label_out
  if ! label_out=$(gh issue edit "$issue_num" --repo "$repo" --add-label "airc-queue" 2>&1); then
    printf 'note: could not add "airc-queue" label to %s#%s: %s\n' "$repo" "$issue_num" "$label_out" >&2
  fi

  printf 'Adopted %s#%s into airc queue.\n' "$repo" "$issue_num"
}

_cmd_queue_nudge() {
  # Surface a queue card to peers via airc msg (broadcast or DM), OR ask
  # everyone in a repo scope for a status pong. Card nudges annotate the
  # card's status log; repo nudges are pure broadcasts. NO status mutation.
  #
  # Args:
  #   airc queue nudge <issue-url> [--peer @handle] [--message "..."] [--dry-run]
  #   airc queue nudge <owner/repo> [--message "..."] [--limit N] [--dry-run]
  #
  # Repo-scoped nudge is the "Bueller?" path: ask all online agents in the
  # room to report current work, blocker, next action, and whether they keep
  # or release their claim. Later stale-claim automation consumes those pongs.

  local target=""
  local target_peer=""
  local extra_message=""
  local dry_run=0
  local limit=20
  local sweep_id=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_nudge_help
        return 0
        ;;
      --peer)     shift; target_peer="${1:-}" ;;
      --message)  shift; extra_message="${1:-}" ;;
      --limit)    shift; limit="${1:-20}" ;;
      --sweep-id) shift; sweep_id="${1:-}" ;;
      --dry-run)  dry_run=1 ;;
      -*) die "queue nudge: unknown flag: $1" ;;
      *)
        if [ -z "$target" ]; then
          target="$1"
        else
          die "queue nudge: too many positional args (use: queue nudge <url|owner/repo> [--peer @h] [--message ...])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target" ]; then
    _airc_queue_nudge_help >&2
    return 1
  fi

  # Normalize --peer: strip a single leading '@' if present, validate non-empty.
  # Joel's protocol-not-client + handle convention: peers identified by AIRC
  # whois name (claude-tab-#1, codex, continuum-8e97, etc.). NOT gh login.
  if [ -n "$target_peer" ]; then
    target_peer="${target_peer#@}"
    if [ -z "$target_peer" ]; then
      die "queue nudge: --peer expects a handle (got empty after stripping @)"
    fi
  fi

  # owner/repo with no # means repo-scoped status sweep. Keep this before
  # issue parsing so "CambrianTech/continuum" is valid nudge input.
  if [[ "$target" =~ ^[^/]+/[^#]+$ ]]; then
    _cmd_queue_nudge_repo "$target" "$target_peer" "$extra_message" "$limit" "$sweep_id" "$dry_run"
    return $?
  fi

  _cmd_queue_nudge_card "$target" "$target_peer" "$extra_message" "$dry_run"
}

_cmd_queue_nudge_repo() {
  # Repo-scoped ping/pong status sweep. Broadcasts a status request with a
  # compact queue snapshot. Agents reply manually today; future monitor glue
  # can auto-pong and feed stale-claim expiry.
  local target_repo="$1"
  local target_peer="$2"
  local extra_message="$3"
  local limit="$4"
  local sweep_id="$5"
  local dry_run="$6"

  case "$target_repo" in
    */*) : ;;
    *) die "queue nudge: target repo must be owner/repo, got: $target_repo" ;;
  esac

  case "$limit" in
    ''|*[!0-9]*) die "queue nudge: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue nudge: --limit must be >= 1 (got: $limit)"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue nudge: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt 2>&1); then
    die "queue nudge: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-repo-nudge.XXXXXX") || die "queue nudge: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  local summary
  if ! summary=$("$AIRC_PYTHON" - "$raw_json_file" <<'PYEOF'
import json, re, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    data = json.load(f)
CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
items = []
for issue in data:
    card = {}
    for m in CARD_BLOCK_RE.finditer(issue.get("body", "") or ""):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            card = parsed
            break
    if not card:
        continue
    title = (issue.get("title", "") or "").replace("airc-queue: ", "", 1)
    status = (card.get("status") or "unknown").strip()
    owner = (card.get("owner") or "").strip()
    branch = (card.get("branch") or "").strip()
    bit = f"#{issue.get('number')} {status}"
    if owner:
        bit += f" owner={owner}"
    if branch:
        bit += f" branch={branch}"
    if title:
        bit += f" '{title[:60]}'"
    items.append(bit)
if items:
    print("; ".join(items[:10]))
else:
    print("no open queue cards")
PYEOF
  ); then
    rm -f "$raw_json_file"
    die "queue nudge: could not summarize queue cards for $target_repo"
  fi
  rm -f "$raw_json_file"

  local actor
  actor=$(_airc_queue_resolve_name)
  if [ -z "$sweep_id" ]; then
    sweep_id=$(date -u +"%Y%m%dT%H%M%SZ")
  fi

  local nudge_text="repo-nudge: ${target_repo} — sweep=${sweep_id} — status sweep requested by ${actor}; open=${summary}"
  if [ -n "$extra_message" ]; then
    nudge_text="${nudge_text} — ${extra_message}"
  fi
  nudge_text="${nudge_text} — pong with: pong: ${target_repo} — sweep=${sweep_id} — <nick> — card=<${target_repo}#N|idle> state=<idle|coding|testing|reviewing|blocked> blocker=<none|...> next=<...> claim=<keep|release|none>"

  if [ "$dry_run" = "1" ]; then
    echo "  [dry-run] would broadcast repo status sweep: ${nudge_text}"
    return 0
  fi

  if [ -n "$target_peer" ]; then
    if ! cmd_send "@${target_peer}" "$nudge_text"; then
      die "queue nudge: cmd_send to @${target_peer} failed"
    fi
  else
    if ! cmd_send "$nudge_text"; then
      die "queue nudge: cmd_send broadcast failed"
    fi
  fi
}

_cmd_queue_nudge_card() {
  local issue_url="$1"
  local target_peer="$2"
  local extra_message="$3"
  local dry_run="$4"

  local parsed_issue repo issue_num
  if ! parsed_issue=$(_airc_queue_parse_issue_url "$issue_url"); then
    die "queue nudge: <issue-url> must be a GitHub issue URL or owner/repo#N (got: $issue_url)"
  fi
  repo="${parsed_issue%#*}"
  issue_num="${parsed_issue##*#}"

  if ! command -v gh >/dev/null 2>&1; then
    die "queue nudge: 'gh' CLI is required."
  fi

  # Verify the issue exists + has a kind=airc-queue-card-v1 envelope. Pull
  # title + current status for the broadcast text. Use temp file to dodge
  # stdin contention with the python heredoc (standard idiom in this module
  # per Codex's review fixes on PR-1+PR-2).
  local issue_blob
  if ! issue_blob=$(gh issue view "$issue_num" --repo "$repo" --json title,body 2>&1); then
    die "queue nudge: gh issue view failed for $repo#$issue_num: $issue_blob"
  fi

  local issue_file
  issue_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-nudge-issue.XXXXXX") || die "queue nudge: mktemp failed"
  printf '%s' "$issue_blob" >"$issue_file"

  local card_meta
  if ! card_meta=$("$AIRC_PYTHON" - "$issue_file" <<'PYEOF'
import json, re, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    issue = json.load(f)
title = issue.get("title", "(no title)")
body = issue.get("body", "") or ""
CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
card = None
for m in CARD_BLOCK_RE.finditer(body):
    try:
        parsed = json.loads(m.group(1).strip())
    except Exception:
        continue
    if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
        card = parsed
        break
if card is None:
    print("ERR:no-envelope", file=sys.stderr)
    sys.exit(2)
status = (card.get("status") or "").strip() or "unknown"
owner = (card.get("owner") or "").strip()
# Tab-separated for shell-friendly parse.
print(f"{title}\t{status}\t{owner}")
PYEOF
  ); then
    rm -f "$issue_file"
    die "queue nudge: $repo#$issue_num is not a valid airc-queue-card-v1 envelope"
  fi
  rm -f "$issue_file"

  local card_title card_status card_owner
  card_title=$(printf '%s' "$card_meta" | awk -F'\t' '{print $1}')
  card_status=$(printf '%s' "$card_meta" | awk -F'\t' '{print $2}')
  card_owner=$(printf '%s' "$card_meta" | awk -F'\t' '{print $3}')

  local actor
  actor=$(_airc_queue_resolve_name)

  # Compose nudge broadcast text. One-line, prefixed with "nudge:" so peers
  # can filter (per QUEUE.md broadcast hooks in continuum#1110 .airc/).
  local nudge_text
  if [ -n "$target_peer" ]; then
    nudge_text="nudge: ${repo}#${issue_num} → @${target_peer} — ${card_title} (status=${card_status}"
  else
    nudge_text="nudge: ${repo}#${issue_num} — ${card_title} (status=${card_status}"
  fi
  if [ -n "$card_owner" ]; then
    nudge_text="${nudge_text}, owner=${card_owner}"
  fi
  nudge_text="${nudge_text})"
  if [ -n "$extra_message" ]; then
    nudge_text="${nudge_text} — ${extra_message}"
  fi
  nudge_text="${nudge_text} — claim with: airc queue claim ${repo}#${issue_num}"

  # Status log message for the card body annotation.
  local log_msg
  if [ -n "$target_peer" ]; then
    log_msg="$actor nudged @${target_peer}"
  else
    log_msg="$actor nudged (broadcast)"
  fi
  if [ -n "$extra_message" ]; then
    log_msg="${log_msg}: ${extra_message}"
  fi

  if [ "$dry_run" = "1" ]; then
    echo "  [dry-run] would broadcast: ${nudge_text}"
    echo "  [dry-run] would annotate ${repo}#${issue_num} status log: ${log_msg}"
    return 0
  fi

  # Send the nudge. DM if --peer set, broadcast otherwise. Use --internal so
  # the nudge doesn't trigger the post-send-poll loop on the sender's side
  # (Codex/non-Monitor runtimes already have their own inbox poll cadence;
  # we don't want every nudge to also dump the sender's inbox to stdout).
  # Actually NOT --internal — recipients should see the broadcast in their
  # inbox stream like normal traffic. The post-send-poll fires on sender
  # side, not on receiver side, so it's a sender-side ergonomic that
  # nudge senders probably want. Leave it as a normal send.
  if [ -n "$target_peer" ]; then
    if ! cmd_send "@${target_peer}" "$nudge_text"; then
      die "queue nudge: cmd_send to @${target_peer} failed"
    fi
  else
    if ! cmd_send "$nudge_text"; then
      die "queue nudge: cmd_send broadcast failed"
    fi
  fi

  # Annotate the card body via the same mutate path used by claim/release/
  # set-status. NO actual field changes — log_msg-only entry to the status
  # log. The mutate helper appends the entry; absence of --set/--clear
  # leaves field values untouched.
  _airc_queue_mutate_card "$issue_url" 0 "$log_msg"
}

_cmd_queue_pongs() {
  # Summarize repo-nudge replies already present in the local AIRC log.
  # This intentionally reads messages.jsonl directly rather than sending
  # more traffic: repo-nudge is the wakeup, pongs is the audit pass.
  local target_repo=""
  local since="30m"
  local sweep_id=""
  local limit=200
  local output_json=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_pongs_help
        return 0
        ;;
      --since)    shift; since="${1:-}" ;;
      --sweep-id) shift; sweep_id="${1:-}" ;;
      --limit)    shift; limit="${1:-200}" ;;
      --json)     output_json=1 ;;
      -*) die "queue pongs: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue pongs: too many positional args (use: queue pongs <owner/repo>)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    _airc_queue_pongs_help >&2
    return 1
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue pongs: target repo must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue pongs: --limit must be a positive integer (got: $limit)" ;;
  esac

  if ! command -v gh >/dev/null 2>&1; then
    die "queue pongs: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt 2>&1); then
    die "queue pongs: gh issue list failed for $target_repo: $raw_json"
  fi

  local cards_file messages_file
  cards_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-pongs-cards.XXXXXX") || die "queue pongs: mktemp failed"
  messages_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-pongs-log.XXXXXX") || die "queue pongs: mktemp failed"
  printf '%s' "$raw_json" >"$cards_file"
  if [ -f "$MESSAGES" ]; then
    tail -"$limit" "$MESSAGES" >"$messages_file" 2>/dev/null || true
  else
    : >"$messages_file"
  fi

  AIRC_QUEUE_PONGS_SINCE="$since" "$AIRC_PYTHON" - \
      "$target_repo" "$sweep_id" "$output_json" "$cards_file" "$messages_file" \
      <<'PYEOF'
import datetime, json, os, re, sys

repo, sweep_id, output_json_raw, cards_path, messages_path = sys.argv[1:6]
output_json = output_json_raw == "1"
since_arg = os.environ.get("AIRC_QUEUE_PONGS_SINCE", "30m")

def parse_since(value: str):
    if not value:
        return None
    m = re.fullmatch(r"(\d+)([smhd])", value)
    if m:
        n = int(m.group(1))
        unit = m.group(2)
        delta = {
            "s": datetime.timedelta(seconds=n),
            "m": datetime.timedelta(minutes=n),
            "h": datetime.timedelta(hours=n),
            "d": datetime.timedelta(days=n),
        }[unit]
        return datetime.datetime.now(datetime.timezone.utc) - delta
    try:
        dt = datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=datetime.timezone.utc)
        return dt
    except ValueError:
        print(f"queue pongs: cannot parse --since '{value}'", file=sys.stderr)
        sys.exit(2)

since_dt = parse_since(since_arg)

with open(cards_path, "r", encoding="utf-8") as f:
    issues = json.load(f)

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
owners = {}
cards = []
for issue in issues:
    card = {}
    for m in CARD_BLOCK_RE.finditer(issue.get("body", "") or ""):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            card = parsed
            break
    if not card:
        continue
    owner = (card.get("owner") or "").strip()
    number = issue.get("number")
    if owner:
        owners.setdefault(owner, []).append(f"{repo}#{number}")
    cards.append({"number": number, "owner": owner, "status": card.get("status", "")})

PONG_RE = re.compile(rf"\bpong:\s*{re.escape(repo)}\b(?P<body>.*)", re.IGNORECASE)
FIELD_RE = re.compile(r"\b([a-zA-Z_][a-zA-Z0-9_-]*)=<([^>]*)>|\b([a-zA-Z_][a-zA-Z0-9_-]*)=([^\s—]+)")

responders = {}
with open(messages_path, "r", encoding="utf-8") as f:
    for line in f:
        try:
            msg = json.loads(line)
        except Exception:
            continue
        ts = msg.get("ts") or ""
        if since_dt is not None:
            try:
                dt = datetime.datetime.fromisoformat(ts.replace("Z", "+00:00"))
                if dt.tzinfo is None:
                    dt = dt.replace(tzinfo=datetime.timezone.utc)
            except ValueError:
                continue
            if dt <= since_dt:
                continue
        text = msg.get("msg") or ""
        if "repo-nudge:" in text and "pong with:" in text:
            continue
        m = PONG_RE.search(text)
        if not m:
            continue
        fields = {}
        for fm in FIELD_RE.finditer(text):
            key = fm.group(1) or fm.group(3)
            value = fm.group(2) if fm.group(1) else fm.group(4)
            fields[key] = value
        if sweep_id and fields.get("sweep") != sweep_id:
            continue
        sender = msg.get("from") or "?"
        nick = sender
        # Expected text has: pong: repo — sweep=X — <nick> — card=...
        parts = [p.strip() for p in text.split("—")]
        for part in parts:
            if part and not part.startswith("pong:") and "=" not in part:
                nick = part
                break
        responders[nick] = {
            "nick": nick,
            "sender": sender,
            "ts": ts,
            "card": fields.get("card", ""),
            "state": fields.get("state", ""),
            "blocker": fields.get("blocker", ""),
            "next": fields.get("next", ""),
            "claim": fields.get("claim", ""),
            "sweep": fields.get("sweep", ""),
        }

missing = sorted([owner for owner in owners if owner not in responders])
payload = {
    "repo": repo,
    "sweep_id": sweep_id,
    "since": since_arg,
    "responders": list(responders.values()),
    "missing_owners": missing,
    "open_owner_cards": owners,
    "open_cards": cards,
}

if output_json:
    print(json.dumps(payload, indent=2))
else:
    label = f" sweep={sweep_id}" if sweep_id else ""
    print(f"# airc-queue pongs — {repo}{label}")
    print(f"since: {since_arg}")
    if responders:
        print(f"responders ({len(responders)}):")
        for item in payload["responders"]:
            print(f"  - {item['nick']}: card={item['card'] or '?'} state={item['state'] or '?'} blocker={item['blocker'] or '?'} next={item['next'] or '?'} claim={item['claim'] or '?'}")
    else:
        print("responders: none")
    if missing:
        print(f"missing owners ({len(missing)}): {', '.join(missing)}")
    else:
        print("missing owners: none")
PYEOF
  local py_status=$?
  rm -f "$cards_file" "$messages_file"
  return "$py_status"
}

_cmd_queue_availability() {
  # Read-only queue-owner + live-room availability summary. This is the
  # operator view for "who is awake, which claimed cards need a nudge, and
  # what exact repo-nudge command should keep the flywheel moving?"
  local target_repo=""
  local since="30m"
  local stale_after="30m"
  local limit=200
  local sweep_id=""
  local output_json=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_availability_help
        return 0
        ;;
      --since)        shift; since="${1:-}" ;;
      --stale-after)  shift; stale_after="${1:-}" ;;
      --sweep-id)     shift; sweep_id="${1:-}" ;;
      --limit)        shift; limit="${1:-200}" ;;
      --json)         output_json=1 ;;
      -*) die "queue availability: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue availability: too many positional args (use: queue availability <owner/repo>)"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    _airc_queue_availability_help >&2
    return 1
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue availability: target repo must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue availability: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue availability: --limit must be >= 1 (got: $limit)"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue availability: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt 2>&1); then
    die "queue availability: gh issue list failed for $target_repo: $raw_json"
  fi

  local cards_file messages_file
  cards_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-availability-cards.XXXXXX") || die "queue availability: mktemp failed"
  messages_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-availability-log.XXXXXX") || die "queue availability: mktemp failed"
  printf '%s' "$raw_json" >"$cards_file"
  if [ -f "$MESSAGES" ]; then
    tail -"$limit" "$MESSAGES" >"$messages_file" 2>/dev/null || true
  else
    : >"$messages_file"
  fi

  if [ -z "$sweep_id" ]; then
    sweep_id=$(date -u +"%Y%m%dT%H%M%SZ")
  fi

  AIRC_QUEUE_AVAILABILITY_SINCE="$since" \
  AIRC_QUEUE_AVAILABILITY_STALE_AFTER="$stale_after" \
  "$AIRC_PYTHON" - "$target_repo" "$sweep_id" "$output_json" "$cards_file" "$messages_file" <<'PYEOF'
import datetime, json, os, re, sys

repo, sweep_id, output_json_raw, cards_path, messages_path = sys.argv[1:6]
output_json = output_json_raw == "1"
since_arg = os.environ.get("AIRC_QUEUE_AVAILABILITY_SINCE", "30m")
stale_after_arg = os.environ.get("AIRC_QUEUE_AVAILABILITY_STALE_AFTER", "30m")

def parse_duration(value: str) -> datetime.timedelta:
    m = re.fullmatch(r"\s*(\d+)\s*([smhd])\s*", value or "")
    if not m:
        print(f"queue availability: cannot parse duration '{value}' (use 30m, 2h, 1d)", file=sys.stderr)
        sys.exit(2)
    n = int(m.group(1))
    return {
        "s": datetime.timedelta(seconds=n),
        "m": datetime.timedelta(minutes=n),
        "h": datetime.timedelta(hours=n),
        "d": datetime.timedelta(days=n),
    }[m.group(2)]

def parse_since(value: str) -> datetime.datetime:
    if re.fullmatch(r"\s*\d+\s*[smhd]\s*", value or ""):
        return datetime.datetime.now(datetime.timezone.utc) - parse_duration(value)
    try:
        dt = datetime.datetime.fromisoformat((value or "").replace("Z", "+00:00"))
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=datetime.timezone.utc)
        return dt
    except ValueError:
        print(f"queue availability: cannot parse --since '{value}'", file=sys.stderr)
        sys.exit(2)

def parse_ts(value: str):
    if not value:
        return None
    m = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(?::\d{2})?Z)", value)
    if not m:
        return None
    raw = m.group(1)
    fmt = "%Y-%m-%dT%H:%M:%SZ" if raw.count(":") == 2 else "%Y-%m-%dT%H:%MZ"
    return datetime.datetime.strptime(raw, fmt).replace(tzinfo=datetime.timezone.utc)

def age_label(seconds):
    if seconds is None:
        return "unknown"
    seconds = max(0, int(seconds))
    if seconds < 60:
        return f"{seconds}s"
    if seconds < 3600:
        return f"{seconds // 60}m"
    if seconds < 86400:
        return f"{seconds // 3600}h"
    return f"{seconds // 86400}d"

CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
def parse_card(body: str):
    for m in CARD_BLOCK_RE.finditer(body or ""):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            return parsed
    return None

now = datetime.datetime.now(datetime.timezone.utc)
since_dt = parse_since(since_arg)
stale_after = parse_duration(stale_after_arg)

with open(cards_path, "r", encoding="utf-8") as f:
    issues = json.load(f)

cards = []
owner_cards = {}
for issue in issues:
    card = parse_card(issue.get("body", "") or "")
    if not card:
        continue
    status = (card.get("status") or "unknown").strip()
    owner = (card.get("owner") or "").strip()
    hb = (card.get("last_heartbeat") or "").strip()
    hb_dt = parse_ts(hb)
    hb_age = int((now - hb_dt).total_seconds()) if hb_dt else None
    reason = ""
    if status in {"claimed", "in-progress", "review"}:
        if not owner:
            reason = "missing-owner"
        elif not hb_dt:
            reason = "missing-heartbeat"
        elif now - hb_dt > stale_after:
            reason = "stale-heartbeat"
    row = {
        "number": issue.get("number"),
        "title": (issue.get("title") or "").replace("airc-queue: ", "", 1),
        "url": issue.get("url") or "",
        "status": status,
        "owner": owner,
        "last_heartbeat": hb,
        "heartbeat_age_seconds": hb_age,
        "heartbeat_age": age_label(hb_age),
        "availability_reason": reason,
        "next_action": card.get("next_action") or "",
    }
    cards.append(row)
    if owner and status in {"claimed", "in-progress", "review"}:
        owner_cards.setdefault(owner, []).append(f"{repo}#{issue.get('number')}")

PONG_RE = re.compile(rf"\bpong:\s*{re.escape(repo)}\b(?P<body>.*)", re.IGNORECASE)
FIELD_RE = re.compile(r"\b([a-zA-Z_][a-zA-Z0-9_-]*)=<([^>]*)>|\b([a-zA-Z_][a-zA-Z0-9_-]*)=([^\s—]+)")
recent_activity = {}
responders = {}
with open(messages_path, "r", encoding="utf-8") as f:
    for line in f:
        try:
            msg = json.loads(line)
        except Exception:
            continue
        ts_raw = msg.get("ts") or ""
        try:
            ts = datetime.datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            if ts.tzinfo is None:
                ts = ts.replace(tzinfo=datetime.timezone.utc)
        except ValueError:
            continue
        if ts <= since_dt:
            continue
        sender = msg.get("from") or "?"
        if sender and sender != "airc":
            prev = recent_activity.get(sender)
            if prev is None or ts > prev["ts_dt"]:
                recent_activity[sender] = {"peer": sender, "ts": ts_raw, "ts_dt": ts, "age": age_label((now - ts).total_seconds())}
        text = msg.get("msg") or ""
        if "repo-nudge:" in text and "pong with:" in text:
            continue
        m = PONG_RE.search(text)
        if not m:
            continue
        fields = {}
        for fm in FIELD_RE.finditer(text):
            key = fm.group(1) or fm.group(3)
            value = fm.group(2) if fm.group(1) else fm.group(4)
            fields[key] = value
        parts = [p.strip() for p in text.split("—")]
        nick = sender
        for part in parts:
            if part and not part.startswith("pong:") and "=" not in part:
                nick = part
                break
        responders[nick] = {
            "nick": nick,
            "sender": sender,
            "ts": ts_raw,
            "sweep": fields.get("sweep", ""),
            "card": fields.get("card", ""),
            "state": fields.get("state", ""),
            "blocker": fields.get("blocker", ""),
            "next": fields.get("next", ""),
            "claim": fields.get("claim", ""),
        }

missing_owners = sorted([owner for owner in owner_cards if owner not in responders and owner not in recent_activity])
stale_cards = [c for c in cards if c["availability_reason"]]
recent = sorted(recent_activity.values(), key=lambda r: r["ts_dt"], reverse=True)
for item in recent:
    item.pop("ts_dt", None)

payload = {
    "repo": repo,
    "now": now.isoformat().replace("+00:00", "Z"),
    "since": since_arg,
    "stale_after": stale_after_arg,
    "sweep_id": sweep_id,
    "cards": cards,
    "stale_cards": stale_cards,
    "responders": list(responders.values()),
    "recent_activity": recent,
    "missing_owners": missing_owners,
    "owner_cards": owner_cards,
    "suggested_nudge": f"airc queue nudge {repo} --sweep-id {sweep_id}",
    "suggested_pongs": f"airc queue pongs {repo} --sweep-id {sweep_id} --since {since_arg}",
}

if output_json:
    print(json.dumps(payload, indent=2))
else:
    print(f"# airc-queue availability — {repo}")
    print(f"now_utc: {payload['now']}")
    print(f"since: {since_arg}")
    print(f"stale_after: {stale_after_arg}")
    print(f"open_cards: {len(cards)}")
    if responders:
        print(f"repo-nudge responders ({len(responders)}):")
        for item in payload["responders"]:
            print(f"  - {item['nick']}: card={item['card'] or '?'} state={item['state'] or '?'} blocker={item['blocker'] or '?'} next={item['next'] or '?'}")
    else:
        print("repo-nudge responders: none")
    if recent:
        print(f"recent room activity ({len(recent)}):")
        for item in recent[:10]:
            print(f"  - {item['peer']}: last seen {item['age']} ago")
    else:
        print("recent room activity: none")
    if stale_cards:
        print(f"attention needed ({len(stale_cards)}):")
        for card in stale_cards:
            owner = card["owner"] or "(unowned)"
            print(f"  - {repo}#{card['number']} {card['status']} owner={owner} reason={card['availability_reason']} heartbeat={card['heartbeat_age']}")
    else:
        print("attention needed: none")
    if missing_owners:
        print(f"missing owners ({len(missing_owners)}): {', '.join(missing_owners)}")
    else:
        print("missing owners: none")
    print("next:")
    print(f"  {payload['suggested_nudge']}")
    print(f"  {payload['suggested_pongs']}")
PYEOF
  local py_status=$?
  rm -f "$cards_file" "$messages_file"
  return "$py_status"
}

_cmd_queue_close_merged() {
  # Auto-close queue cards explicitly completed by a merged PR.
  #
  # Args:
  #   airc queue close-merged <pr-url|owner/repo#PR> [--merge-sha SHA] [--actor X] [--allow-cross-repo] [--dry-run]
  #
  # GitHub's native "Closes #N" only triggers when the PR merges into the
  # default branch. AIRC uses canary first, so queue cards need a canary-time
  # close path. Plain mentions ("Refs #N", "See #N") are intentionally not
  # close targets; they may describe related work that remains open.
  #
  # Cross-repo close (--allow-cross-repo) requires gh to be authenticated
  # with a token that has issues:write on the OTHER repo (continuum#1174):
  # the workflow's auto-issued GITHUB_TOKEN is repo-scoped and can't close
  # cross-repo refs. Operator supplies a fine-grained PAT or GitHub App
  # installation token via the workflow's GH_TOKEN env. Without the flag
  # (default), cross-repo refs are detected + reported but NOT closed —
  # preserves backward compatibility with existing repo-scoped workflows.
  local pr_url=""
  local merge_sha=""
  local actor=""
  local dry_run=0
  local allow_cross_repo=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_close_merged_help
        return 0
        ;;
      --merge-sha)        shift; merge_sha="${1:-}" ;;
      --actor)            shift; actor="${1:-}" ;;
      --dry-run)          dry_run=1 ;;
      --allow-cross-repo) allow_cross_repo=1 ;;
      -*) die "queue close-merged: unknown flag: $1" ;;
      *)
        if [ -z "$pr_url" ]; then
          pr_url="$1"
        else
          die "queue close-merged: too many positional args (use: queue close-merged <pr-url> [--merge-sha SHA])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$pr_url" ]; then
    _airc_queue_close_merged_help >&2
    return 1
  fi

  local pr_repo pr_num
  if [[ "$pr_url" =~ ^https://github\.com/([^/]+/[^/]+)/(pull|pulls|issues)/([0-9]+) ]]; then
    pr_repo="${BASH_REMATCH[1]}"
    pr_num="${BASH_REMATCH[3]}"
  elif [[ "$pr_url" =~ ^([^/]+/[^/]+)#([0-9]+)$ ]]; then
    pr_repo="${BASH_REMATCH[1]}"
    pr_num="${BASH_REMATCH[2]}"
  else
    die "queue close-merged: <pr-url> must be a GitHub PR URL or owner/repo#N (got: $pr_url)"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue close-merged: 'gh' CLI is required."
  fi

  local pr_blob
  if ! pr_blob=$(gh pr view "$pr_num" --repo "$pr_repo" --json title,body,mergedAt,mergeCommit,baseRefName,url 2>&1); then
    die "queue close-merged: gh pr view failed for $pr_repo#$pr_num: $pr_blob"
  fi

  local pr_file
  pr_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-pr.XXXXXX") || die "queue close-merged: mktemp failed"
  printf '%s' "$pr_blob" >"$pr_file"

  local pr_meta
  if ! pr_meta=$("$AIRC_PYTHON" - "$pr_file" <<'PYEOF'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    pr = json.load(f)
merged_at = pr.get("mergedAt") or ""
base_ref = pr.get("baseRefName") or ""
merge_commit = pr.get("mergeCommit") or {}
sha = (merge_commit.get("oid") if isinstance(merge_commit, dict) else "") or ""
body = pr.get("body") or ""
title = pr.get("title") or ""
url = pr.get("url") or ""
print(f"{merged_at}\t{base_ref}\t{sha}\t{url}\t{len(title)}\t{len(body)}")
PYEOF
  ); then
    rm -f "$pr_file"
    die "queue close-merged: PR JSON parse failed"
  fi

  local pr_merged_at pr_base_ref pr_sha pr_canonical_url pr_title_len pr_body_len
  pr_merged_at=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $1}')
  pr_base_ref=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $2}')
  pr_sha=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $3}')
  pr_canonical_url=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $4}')
  pr_title_len=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $5}')
  pr_body_len=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $6}')

  if [ -z "$pr_merged_at" ]; then
    rm -f "$pr_file"
    die "queue close-merged: PR $pr_repo#$pr_num is not merged (mergedAt empty). Refusing to close cards from an unmerged PR."
  fi

  if [ -z "$merge_sha" ]; then
    merge_sha="$pr_sha"
  fi
  if [ -z "$merge_sha" ]; then
    rm -f "$pr_file"
    die "queue close-merged: no merge SHA available (passed nor in PR metadata). Refusing to close — status-log entry would have no anchor."
  fi

  if [ -z "$actor" ]; then
    actor=$(_airc_queue_resolve_name)
  fi

  local refs_file
  refs_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-refs.XXXXXX") || die "queue close-merged: mktemp failed"
  if ! "$AIRC_PYTHON" - "$pr_file" "$pr_repo" >"$refs_file" <<'PYEOF'
import json, re, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    pr = json.load(f)
default_repo = sys.argv[2]
body = pr.get("body") or ""
title = pr.get("title") or ""
text = f"{title}\n{body}"

CLOSING_KEYWORD_RE = re.compile(
    r'\b(?:close[sd]?|fix(?:e[sd]|ed)?|resolve[sd]?)\b\s*:?\s*',
    re.IGNORECASE,
)
REF_RE = re.compile(
    r'\b([A-Za-z0-9][A-Za-z0-9._-]*)/([A-Za-z0-9][A-Za-z0-9._-]*)#(\d+)\b'
    r'|(?<![A-Za-z0-9_/])#(\d+)\b'
)

seen = set()
for keyword in CLOSING_KEYWORD_RE.finditer(text):
    # Match GitHub-style close intent. Refs must appear immediately after the
    # closing keyword, with optional comma/"and" continuations. This avoids
    # closing cards for prose like "Fix the UI. See #100 for follow-up."
    pos = keyword.end()
    first = True
    while True:
        tail = text[pos:]
        if not first:
            sep = re.match(r'\s*(?:,|and)\s+', tail, re.IGNORECASE)
            if not sep:
                break
            pos += sep.end()
            tail = text[pos:]
        ref = REF_RE.match(tail)
        if not ref:
            break
        if ref.group(4):
            key = f"{default_repo}#{ref.group(4)}"
        else:
            key = f"{ref.group(1)}/{ref.group(2)}#{ref.group(3)}"
        if key not in seen:
            seen.add(key)
            print(key)
        pos += ref.end()
        first = False
PYEOF
  then
    rm -f "$pr_file" "$refs_file"
    die "queue close-merged: ref-parser failed"
  fi
  rm -f "$pr_file"

  local refs=()
  if [ -s "$refs_file" ]; then
    while IFS= read -r line; do
      [ -n "$line" ] && refs+=("$line")
    done <"$refs_file"
  fi
  rm -f "$refs_file"

  printf 'queue close-merged: PR %s merged into %s @ %s\n' "$pr_canonical_url" "$pr_base_ref" "${merge_sha:0:8}"
  printf 'queue close-merged: scanned %d title/body closing refs (PR title %d chars, body %d chars)\n' "${#refs[@]}" "$pr_title_len" "$pr_body_len"

  if [ "${#refs[@]}" -eq 0 ]; then
    printf 'queue close-merged: no queue-card closing refs in PR title/body — nothing to close.\n'
    return 0
  fi

  local closed_count=0 skipped_count=0 errored_count=0 cross_repo_count=0
  local ref ref_repo ref_num
  for ref in "${refs[@]}"; do
    ref_repo="${ref%#*}"
    ref_num="${ref##*#}"

    if [ "$ref_repo" != "$pr_repo" ]; then
      if [ "$allow_cross_repo" -eq 0 ]; then
        printf '  [cross-repo] %s — skipped (--allow-cross-repo not set; default GITHUB_TOKEN is repo-scoped)\n' "$ref"
        cross_repo_count=$((cross_repo_count + 1))
        continue
      fi
      # Fall through to normal close path. gh's auth context (GH_TOKEN
      # env or `gh auth login`) decides whether the close actually
      # succeeds — if the token doesn't have issues:write on $ref_repo,
      # the close call fails loudly with the gh API error and we count
      # it as errored (NOT a silent skip). Operator supplies the
      # broader-scoped token via the workflow's GH_TOKEN secret.
      printf '  [cross-repo] %s — attempting close (--allow-cross-repo set; relying on gh auth scope)\n' "$ref"
      cross_repo_count=$((cross_repo_count + 1))
    fi

    local issue_body
    if ! issue_body=$(gh issue view "$ref_num" --repo "$ref_repo" --json body --jq .body 2>&1); then
      printf '  [skip]       %s — gh issue view failed (likely a PR ref, not an issue)\n' "$ref"
      skipped_count=$((skipped_count + 1))
      continue
    fi

    local envelope_status
    envelope_status=$(printf '%s' "$issue_body" | "$AIRC_PYTHON" -c '
import json, re, sys
body = sys.stdin.read()
CARD_BLOCK_RE = re.compile(r"```json\s*\n(.*?)\n\s*```", re.DOTALL)
for m in CARD_BLOCK_RE.finditer(body):
    try:
        parsed = json.loads(m.group(1).strip())
    except Exception:
        continue
    if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
        print((parsed.get("status") or "").strip() or "unknown")
        sys.exit(0)
print("not-a-card")
')

    case "$envelope_status" in
      not-a-card)
        printf '  [skip]       %s — not an airc-queue card (no envelope)\n' "$ref"
        skipped_count=$((skipped_count + 1))
        continue
        ;;
      merged)
        # Status mutation is idempotent, but closure is not. A prior
        # heartbeat/set-status may have moved the card to merged before
        # close-merged runs; if the GitHub issue is still open, this
        # command must still close it so merged cards leave the live queue.
        ;;
    esac

    local log_msg="merged via PR ${pr_canonical_url} @ ${merge_sha:0:8} (closed by ${actor})"

    if [ "$dry_run" -eq 1 ]; then
      if [ "$envelope_status" = "merged" ]; then
        printf '  [dry-run]    %s — would close already status=merged card\n' "$ref"
      else
        printf '  [dry-run]    %s — would set status=merged + close (was: %s)\n' "$ref" "$envelope_status"
      fi
      closed_count=$((closed_count + 1))
      continue
    fi

    if [ "$envelope_status" = "merged" ]; then
      if ! _airc_queue_mutate_card "$ref" 0 "$log_msg" >/dev/null 2>&1; then
        printf '  [error]      %s — status-log mutation failed\n' "$ref"
        errored_count=$((errored_count + 1))
        continue
      fi
    elif ! _airc_queue_mutate_card "$ref" 0 "$log_msg" --set "status=merged" >/dev/null 2>&1; then
      printf '  [error]      %s — status mutation failed\n' "$ref"
      errored_count=$((errored_count + 1))
      continue
    fi

    local close_out
    if ! close_out=$(gh issue close "$ref_num" --repo "$ref_repo" --reason completed 2>&1); then
      printf '  [error]      %s — gh issue close failed: %s\n' "$ref" "$close_out"
      errored_count=$((errored_count + 1))
      continue
    fi

    if [ "$envelope_status" = "merged" ]; then
      printf '  [closed]     %s — already status=merged, issue closed\n' "$ref"
    else
      printf '  [closed]     %s — status=merged, issue closed (was: %s)\n' "$ref" "$envelope_status"
    fi
    closed_count=$((closed_count + 1))
  done

  printf 'queue close-merged: %d closed, %d skipped, %d errored, %d cross-repo (out of %d refs)\n' \
    "$closed_count" "$skipped_count" "$errored_count" "$cross_repo_count" "${#refs[@]}"

  if [ "$errored_count" -gt 0 ]; then
    return 1
  fi
  return 0
}

_airc_queue_mutate_card() {
  # Update an existing card body in place.
  # Args: <issue_url> <dry_run> <log_msg> [--set field=value | --clear field]...
  #
  # Fetches the current body, parses the kind=airc-queue-card-v1 envelope,
  # mutates per the --set/--clear flags, appends a "## Status log" entry,
  # and writes the new body via gh issue edit.

  local issue_url="$1"; shift
  local dry_run="$1"; shift
  local log_msg="$1"; shift

  # Remaining args are --set field=value / --clear field pairs. We pass
  # them as a single space-separated string to the python helper because
  # bash arrays don't survive a heredoc cleanly.
  local mutations=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --set)
        shift
        mutations="${mutations}set:$1"$'\n'
        ;;
      --clear)
        shift
        mutations="${mutations}clear:$1"$'\n'
        ;;
      *) die "queue mutate: unknown internal mutation arg: $1" ;;
    esac
    shift || true
  done

  local parsed_issue repo issue_num
  if ! parsed_issue=$(_airc_queue_parse_issue_url "$issue_url"); then
    die "queue: <issue-url> must be a GitHub issue URL or owner/repo#N (got: $issue_url)"
  fi
  repo="${parsed_issue%#*}"
  issue_num="${parsed_issue##*#}"

  if ! command -v gh >/dev/null 2>&1; then
    die "queue: 'gh' CLI is required."
  fi

  local current_body
  if ! current_body=$(gh issue view "$issue_num" --repo "$repo" --json body --jq .body 2>&1); then
    die "queue: gh issue view failed for $repo#$issue_num: $current_body"
  fi

  # Hand to python: parse envelope, apply mutations, rewrite body with
  # status-log entry. Python heredoc handles edge cases (escaping, regex)
  # better than bash here. Body + mutations passed via temp files to
  # dodge stdin contention with the heredoc.
  local body_file mut_file
  body_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-body.XXXXXX") || die "queue: mktemp failed"
  mut_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-muts.XXXXXX") || die "queue: mktemp failed"
  printf '%s' "$current_body" >"$body_file"
  printf '%s' "$mutations" >"$mut_file"

  local timestamp
  timestamp=$(date -u +"%Y-%m-%dT%H:%MZ")

  local new_body
  if ! new_body=$("$AIRC_PYTHON" - "$body_file" "$mut_file" "$log_msg" "$timestamp" <<'PYEOF'
import json, re, sys
body_path, mut_path, log_msg, timestamp = sys.argv[1:5]

with open(body_path, "r", encoding="utf-8") as f:
    body = f.read()
with open(mut_path, "r", encoding="utf-8") as f:
    mutations_raw = f.read().strip().splitlines()

# Find the kind=airc-queue-card-v1 JSON block.
CARD_BLOCK_RE = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
match = None
for m in CARD_BLOCK_RE.finditer(body):
    try:
        parsed = json.loads(m.group(1).strip())
    except Exception:
        continue
    if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
        match = m
        card = parsed
        break
if match is None:
    print("queue mutate: no kind=airc-queue-card-v1 envelope found in body", file=sys.stderr)
    sys.exit(2)

# Apply mutations.
for raw in mutations_raw:
    if raw.startswith("set:"):
        keyval = raw[4:]
        if "=" not in keyval:
            print(f"queue mutate: malformed --set: {keyval}", file=sys.stderr)
            sys.exit(2)
        k, v = keyval.split("=", 1)
        card[k.strip()] = v.strip()
    elif raw.startswith("clear:"):
        k = raw[6:].strip()
        if k in card:
            del card[k]
    else:
        # Empty line from trailing newline; ignore.
        if raw.strip():
            print(f"queue mutate: malformed mutation: {raw}", file=sys.stderr)
            sys.exit(2)

new_envelope = json.dumps(card, indent=2)
new_block = "```json\n" + new_envelope + "\n```"

# Replace the original block with the new one.
body_with_new_envelope = body[:match.start()] + new_block + body[match.end():]

# Append to ## Status log section. If it doesn't exist yet, create it.
log_line = f"- {timestamp} — {log_msg}"
LOG_HEADER = "## Status log"
if LOG_HEADER in body_with_new_envelope:
    # Append to existing section: insert after the header line.
    body_with_log = body_with_new_envelope.replace(
        LOG_HEADER, LOG_HEADER + "\n\n" + log_line, 1
    )
    # Above replaces the FIRST match; entries pile in reverse-chrono
    # at the top of the section. Newest-first reads better at a glance.
else:
    # Create the section at the end of the body.
    body_with_log = body_with_new_envelope.rstrip() + "\n\n" + LOG_HEADER + "\n\n" + log_line + "\n"

print(body_with_log, end="")
PYEOF
); then
    rm -f "$body_file" "$mut_file"
    die "queue mutate: python helper failed: $new_body"
  fi
  rm -f "$body_file" "$mut_file"

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would update %s#%s:\n' "$repo" "$issue_num"
    printf '  log:  %s\n' "$log_msg"
    printf '  new body:\n'
    printf '%s\n' "$new_body" | sed 's/^/    /'
    return 0
  fi

  # --body-file via lib_gh.sh — mutated card bodies always contain a
  # ```json``` fence and a Status log of backticked refs (airc#571).
  local edit_out
  if ! edit_out=$(_airc_gh_safe_body "$new_body" issue edit "$issue_num" \
    --repo "$repo"); then
    die "queue mutate: gh issue edit failed for $repo#$issue_num: $edit_out"
  fi

  printf 'Updated %s#%s: %s\n' "$repo" "$issue_num" "$log_msg"
}

_airc_queue_parse_issue_url() {
  # Parse a GitHub issue URL or owner/repo#N short form. Prints
  # owner/repo#N on success. Avoid bash namerefs here: macOS still ships
  # bash 3.x, and `local -n` breaks exactly where Codex runs.
  local url="$1"
  if [[ "$url" =~ ^https://github\.com/([^/]+/[^/]+)/issues/([0-9]+) ]]; then
    printf '%s#%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
    return 0
  elif [[ "$url" =~ ^([^/]+/[^/]+)#([0-9]+)$ ]]; then
    printf '%s#%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
    return 0
  fi
  return 1
}

_airc_queue_claim_help() {
  cat <<'EOF'
airc queue claim — take ownership of a queue card

USAGE
  airc queue claim <issue-url> [--owner X] [--status Y] [--dry-run]
  airc queue claim owner/repo#N [--owner X] [--status Y] [--dry-run]

DESCRIPTION
  Sets the card's owner field and status to indicate active work. Default
  owner = current work identity from `airc identity whoami`; default status = in-progress.
  Appends a "## Status log" line with timestamp + actor.

OPTIONS
  --owner <handle>   Queue owner to set (default: current work identity).
  --status <state>   New status (default: in-progress).
  --dry-run          Print the new body that WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_release_help() {
  cat <<'EOF'
airc queue release — give up ownership of a queue card

USAGE
  airc queue release <issue-url> [--reason "..."] [--status claimed|blocked] [--dry-run]
  airc queue release owner/repo#N [--reason "..."] [--status claimed|blocked] [--dry-run]

DESCRIPTION
  Clears the owner field (back to the unclaimed pool) and sets status to
  "claimed" (default) or "blocked" if --status blocked. Appends a status
  log line with timestamp, actor, and optional reason.

OPTIONS
  --reason "<text>"  Brief explanation logged with the release.
  --status <state>   New status: claimed or blocked (default: claimed).
                     For in-progress/review/merged use `airc queue set-status`.
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_set_status_help() {
  cat <<'EOF'
airc queue set-status — change the status field on a queue card

USAGE
  airc queue set-status <issue-url> <state> [--dry-run]
  airc queue set-status owner/repo#N <state> [--dry-run]

ARGUMENTS
  <state>            One of: claimed, in-progress, blocked, review, merged.

OPTIONS
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.

NOTES
  - Does NOT close the issue automatically when set to merged. Operators
    close manually so the queue tracks closure events explicitly.
EOF
}

_airc_queue_heartbeat_help() {
  cat <<'EOF'
airc queue heartbeat — stamp liveness on a queue card

USAGE
  airc queue heartbeat <issue-url> [--owner X] [--status Y] [--note "..."] [--dry-run]
  airc queue heartbeat owner/repo#N [--owner X] [--status Y] [--note "..."] [--dry-run]

DESCRIPTION
  Sets owner (default: current work identity) and last_heartbeat to the
  current UTC timestamp plus git SHA when available. Optionally updates
  status. Appends a Status log line so humans can see that work is alive.

OPTIONS
  --owner <handle>   Queue owner to record (default: current work identity).
  --status <state>   Optional status update: claimed, in-progress, blocked,
                     review, or merged.
  --note "<text>"    Short context appended to the status log.
  --dry-run          Print what WOULD be written; don't edit.
  -h, --help         This help.
EOF
}

_airc_queue_stale_help() {
  cat <<'EOF'
airc queue stale — list owned queue cards with missing/old heartbeats

USAGE
  airc queue stale [<owner/repo>] [--stale-after 30m] [--limit N] [--json]

DESCRIPTION
  Read-only stale-claim scan. It lists open airc-queue cards in claimed,
  in-progress, or review state when they have an owner but no heartbeat,
  no owner, or a last_heartbeat older than --stale-after. It does not
  release or mutate cards; humans/agents can nudge, heartbeat, or release.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --stale-after <dur>      Duration threshold: 30m, 2h, 1d (default: 30m).
  --limit <N>              Max issues to fetch (default 50).
  --json                   Emit machine-readable JSON.
  -h, --help               This help.
EOF
}

_airc_queue_next_help() {
  cat <<'EOF'
airc queue next — recommend next claimable work for idle agents

USAGE
  airc queue next [<owner/repo>] [--owner HANDLE] [--limit N] [--json]
  airc queue pick [<owner/repo>] [--idle-ping]

DESCRIPTION
  Action-oriented flywheel primitive. Scans open airc-queue cards and ranks
  claimable work so an agent that just finished a task can immediately pick
  another one without waiting for a human. Every candidate includes exact
  `airc queue claim ...` and `airc lane create ...` commands.

OPTIONS
  --repo <owner/repo>      Alternative way to specify repo.
  --owner <handle>         Agent/work identity to use in claim commands.
                           Default: current AIRC work identity.
  --base <branch>          Base branch for suggested lane create commands
                           (default: canary).
  --repo-root <path>       Repo path for suggested lane create commands.
  --limit <N>              Max issues to fetch (default 30).
  --idle-ping              Broadcast that this agent is idle and looking
                           for work after printing recommendations.
  --json                   Emit machine-readable JSON.
  -h, --help               This help.

RANKING
  1. unowned claimed cards
  2. owned claimed cards
  3. unowned blocked cards
  4. review cards
  5. this owner's in-progress cards

NOTES
  - This does not mutate cards by itself. Agents should run the printed
    claim/lane commands, then heartbeat while working.
  - Monitor/automation layers should call this after a merge, release, or
    idle timeout; humans should not need to manually wake agents.
EOF
}

_airc_queue_adopt_help() {
  cat <<'EOF'
airc queue adopt — convert an existing issue into a queue card

USAGE
  airc queue adopt <issue-url> [card-fields...] [--force] [--dry-run]
  airc queue adopt owner/repo#N [card-fields...] [--force] [--dry-run]

DESCRIPTION
  Prepends the standard airc-queue JSON envelope to an existing GitHub issue,
  preserves the original issue body under "Original issue body", and applies
  the airc-queue label when possible. This is the backlog migration path:
  existing issues become queue-managed cards without creating duplicates.

CARD FIELDS (all optional; defaults shown)
  --id <ref>             Issue/PR this card coordinates (default: #N)
  --branch <name>        Branch name, if known.
  --owner <handle>       Queue owner (default: current work identity)
  --status <state>       claimed | in-progress | blocked | review | merged
                         (default: claimed)
  --blockers <list>      Comma-separated blockers.
  --env <tag>            Environment/capability tag.
  --evidence <text>      Why this was adopted or what proof exists.
  --next-action <text>   One sentence on the next step.
  --last-heartbeat <ts>  ISO timestamp + sha, if known.

OPTIONS
  --force                Rewrite even if a queue envelope already exists.
  --dry-run              Print the adopted body that WOULD be posted.
  -h, --help             This help.

EXAMPLES
  airc queue adopt CambrianTech/continuum#914 \\
    --owner codex \\
    --status claimed \\
    --env ui/browser \\
    --next-action "Decide whether this stale UI issue still applies."
EOF
}

_airc_queue_nudge_help() {
  cat <<'EOF'
airc queue nudge — surface a queue card OR run a repo status sweep

USAGE
  airc queue nudge <issue-url> [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo#N [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo [--peer @handle] [--message "..."] [--limit N] [--sweep-id ID] [--dry-run]

ARGUMENTS
  <issue-url>        GitHub issue URL OR owner/repo#N reference. Card-scoped
                     nudge verifies kind=airc-queue-card-v1 and annotates it.
  owner/repo         Repo-scoped "Bueller" nudge. Broadcasts a status sweep
                     request to agents working in the current AIRC room/scope.

OPTIONS
  --peer @handle     DM the nudge to a specific peer. Default: broadcast to
                     the current scope's room.
  --message "..."    Optional one-line explanation appended to the nudge
                     ("nudge: #1125 — pickup needed before EOD" etc.).
  --limit N          Repo-scoped mode only: max queue cards to summarize
                     from the repo (default: 20).
  --sweep-id ID      Repo-scoped mode only: explicit sweep id. Default is
                     current UTC timestamp; replies should include sweep=ID.
  --dry-run          Print the broadcast text + status-log entry that
                     WOULD be written; don't send or edit.
  -h, --help         This help.

CARD-SCOPED MODE
  1. Verifies the issue is a real airc-queue card (envelope exists).
  2. Composes a one-line nudge: "nudge:<repo>#<N> [→ @peer] — <title> (<status>)
     [— <message>]"
  3. Sends via airc msg (broadcast OR DM if --peer), so peers see it in
     their inbox stream alongside other AIRC traffic.
  4. Appends a status-log entry to the card body recording who nudged + when
     + target peer (if any). Same _airc_queue_mutate_card path as
     claim/release/set-status — no new wire format.

REPO-SCOPED MODE
  - Lists open airc-queue cards on owner/repo, summarizes status/owner/branch.
  - Broadcasts a "repo-nudge:" ping asking online agents to pong with:
      identity, card/PR, state, blocker, next action, and keep/release claim.
  - Does NOT mutate cards yet. Future stale-claim automation consumes pongs.

NOTES
  - Nudge is the ACTION; stale-claim policy lives upstream/downstream.
  - Heartbeat / stall-detection (auto-pickup of cards whose owner went
    silent) is intentionally out of scope here — see airc#562 PR-4
    backlog and `.airc/ASSEMBLY-LINE.md` in continuum#1110.
  - Status fields are NOT changed by nudge. Use airc queue set-status if
    you need to mark a card differently.
EOF
}

_airc_queue_pongs_help() {
  cat <<'EOF'
airc queue pongs — summarize repo-nudge replies from the AIRC log

USAGE
  airc queue pongs <owner/repo> [--since 30m] [--sweep-id ID] [--limit N] [--json]
  airc queue pong-summary <owner/repo> [--since 30m] [--sweep-id ID]

DESCRIPTION
  Reads local AIRC messages.jsonl for `pong: owner/repo ...` replies,
  summarizes responders, and compares them to owners of open airc-queue
  cards. This is the audit half of `airc queue nudge owner/repo`.

OPTIONS
  --since <when>      ISO timestamp or relative window: 60s, 5m, 1h, 2d.
                      Default: 30m.
  --sweep-id <ID>     Only include pongs with sweep=<ID>.
  --limit <N>         Max queue cards and log lines to inspect (default 200).
  --json              Emit machine-readable summary.
  -h, --help          This help.

EXPECTED PONG
  pong: owner/repo — sweep=ID — <nick> — card=<owner/repo#N|idle>
    state=<idle|coding|testing|reviewing|blocked> blocker=<none|...>
    next=<...> claim=<keep|release|none>
EOF
}

_airc_queue_availability_help() {
  cat <<'EOF'
airc queue availability — summarize live queue ownership and peer activity

USAGE
  airc queue availability <owner/repo> [--since 30m] [--stale-after 30m] [--limit N] [--json]
  airc queue avail <owner/repo> [--since 30m]

DESCRIPTION
  Read-only flywheel / stale-claim view. Combines open airc-queue cards, last heartbeat
  fields, local AIRC messages, and repo-nudge pongs into one operator
  summary so agents can see who appears active, which claims need attention,
  and which nudge/pongs commands to run next.

OPTIONS
  --since <dur|ISO>       Recent-message window (default 30m).
  --stale-after <dur>     Claim heartbeat age considered stale (default 30m).
  --sweep-id <ID>         Suggested sweep id for the next repo nudge.
                          Defaults to current UTC timestamp.
  --limit <N>             Max queue cards / log lines to inspect (default 200).
  --json                  Emit machine-readable JSON.
  -h, --help              This help.

NOTES
  - Does not mutate cards or send messages.
  - For stale/missing owners, run the printed `airc queue nudge ...` command,
    then later run the printed `airc queue pongs ...` command.
EOF
}

_airc_queue_close_merged_help() {
  cat <<'EOF'
airc queue close-merged — auto-close queue cards completed by a merged PR

USAGE
  airc queue close-merged <pr-url> [--merge-sha SHA] [--actor X] [--allow-cross-repo] [--dry-run]
  airc queue close-merged owner/repo#PR [--merge-sha SHA] [--actor X] [--allow-cross-repo] [--dry-run]

ARGUMENTS
  <pr-url>            GitHub PR URL (https://github.com/.../pull/N) OR
                      owner/repo#N short form. PR must already be merged.

OPTIONS
  --merge-sha SHA     Merge commit SHA for the audit trail. If omitted,
                      pulled from PR metadata (mergeCommit.oid).
  --actor X           Identity recorded in the status-log entry. Defaults
                      to current work identity. CI passes
                      "github-actions" so the audit trail names the system.
  --allow-cross-repo  Attempt to close cross-repo queue-card refs (default:
                      report-only). Requires gh to be authenticated with a
                      token that has issues:write on the OTHER repo —
                      typically a fine-grained PAT or GitHub App
                      installation token, supplied via the workflow's
                      GH_TOKEN secret. Without this flag, cross-repo refs
                      are detected + reported but NOT closed (preserves
                      backward compat with existing repo-scoped workflows).
                      See continuum#1174 for the design rationale.
  --dry-run           Show what WOULD be closed; don't mutate or close.
  -h, --help          This help.

WHAT IT DOES
  1. Fetches the PR body via gh.
  2. Validates the PR is actually merged.
  3. Parses the body for same-repo and cross-repo queue-card closing refs
     with GitHub-style closing keywords (Closes/Fixes/Resolves).
  4. For same-repo queue cards: sets status=merged and closes the issue.
  5. Cross-repo closing refs are reported. With --allow-cross-repo set,
     attempts the close (gh's auth scope decides if it actually succeeds —
     failures count as errored, not silent skips). Without the flag, they
     are skipped with a count in the summary.
  Plain mentions like "Refs #N" are ignored so doc-only PRs do not close
  implementation cards.
EOF
}
