# Sourced by airc. cmd_queue — issue-backed work queue primitives (airc#562).
#
# Function exported back to airc's dispatch:
#   cmd_queue — subcommand router. Verbs:
#                 add        — create a new queue card (GitHub issue, airc-queue label). [PR-1]
#                 list       — list open queue cards on a repo (or auto-detected).       [PR-1]
#                 claim      — set owner+status on an existing card.                     [PR-2]
#                 release    — clear owner (back to claimable pool).                     [PR-2]
#                 set-status — change status field with enum validation.                 [PR-2]
#                 nudge      — surface a card OR repo-scoped status sweep.                [PR-3+]
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
    nudge)
      _cmd_queue_nudge "$@"
      ;;
    *)
      die "queue: unknown subcommand: $subcmd (try: add, list, claim, release, set-status, nudge)"
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

  local issue_title="airc-queue: $title"
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
  airc queue nudge <issue-url> [--peer @handle] [--message "..."]
  airc queue nudge <owner/repo> [--message "..."] [--limit N]

DESCRIPTION
  Adds, lists, or mutates queue cards (GitHub issues with airc-queue
  label). Card fields follow the spec in continuum/.airc/QUEUE.md
  (sibling claude tab #1's continuum#1110).

VERB SCOPE
  add / list                   PR-1 (airc#566, merged)
  claim / release / set-status PR-2 (airc#568, merged)
  nudge                        PR-3 (card) + PR-4a (repo ping/pong sweep)
  heartbeat / stall detection  PR-4 (deferred)

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
  --owner <handle>       AIRC handle (default: this scope's resolve_name)
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
  # Best-effort airc handle for the current scope. Falls back to
  # "anonymous" if no scope (cmd_queue must work pre-init too —
  # outsiders may want to query/add cards before joining).
  if declare -F resolve_name >/dev/null 2>&1; then
    resolve_name
  else
    echo "anonymous"
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

  _airc_queue_mutate_card "$issue_url" "$dry_run" \
    "claim by $new_owner -> status=$new_status" \
    --set "owner=$new_owner" \
    --set "status=$new_status"
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

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_nudge_help
        return 0
        ;;
      --peer)     shift; target_peer="${1:-}" ;;
      --message)  shift; extra_message="${1:-}" ;;
      --limit)    shift; limit="${1:-20}" ;;
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
    _cmd_queue_nudge_repo "$target" "$target_peer" "$extra_message" "$limit" "$dry_run"
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
  local dry_run="$5"

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

  local nudge_text="repo-nudge: ${target_repo} — status sweep requested by ${actor}; open=${summary}"
  if [ -n "$extra_message" ]; then
    nudge_text="${nudge_text} — ${extra_message}"
  fi
  nudge_text="${nudge_text} — pong with: pong: ${target_repo} — <nick> — card=<${target_repo}#N|idle> state=<idle|coding|testing|reviewing|blocked> blocker=<none|...> next=<...> claim=<keep|release|none>"

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
  owner = current scope's resolve_name; default status = in-progress.
  Appends a "## Status log" line with timestamp + actor.

OPTIONS
  --owner <handle>   AIRC handle to set as owner (default: this scope).
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

_airc_queue_nudge_help() {
  cat <<'EOF'
airc queue nudge — surface a queue card OR run a repo status sweep

USAGE
  airc queue nudge <issue-url> [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo#N [--peer @handle] [--message "..."] [--dry-run]
  airc queue nudge owner/repo [--peer @handle] [--message "..."] [--limit N] [--dry-run]

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
