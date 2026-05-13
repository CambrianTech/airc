# Sourced by airc. cmd_queue — issue-backed work queue primitives (airc#562 PR-1).
#
# Function exported back to airc's dispatch:
#   cmd_queue — subcommand router. Verbs in PR-1:
#                 add    — create a new queue card (GitHub issue, airc-queue label).
#                 list   — list open queue cards on a repo (or auto-detected).
#
# Verbs deferred to later PRs under airc#562:
#   - claim / release / state transitions (PR-2)
#   - nudge (broadcast to idle peers) (PR-3)
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
    *)
      die "queue: unknown subcommand: $subcmd (try: add, list)"
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
  local issue_url
  if issue_url=$(gh issue create \
    --repo "$target_repo" \
    --title "$issue_title" \
    --body "$issue_body" \
    --label "airc-queue" 2>&1); then
    :
  elif issue_url=$(gh issue create \
    --repo "$target_repo" \
    --title "$issue_title" \
    --body "$issue_body" 2>&1); then
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

DESCRIPTION
  Adds a queue card (GitHub issue with airc-queue label) or lists open
  cards filtered by owner / status. Card fields follow the spec in
  continuum/.airc/QUEUE.md (sibling claude tab #1's continuum#1110).

PR-1 SCOPE
  Only `add` + `list`. Coming in later PRs under airc#562:
    - claim / release / state transitions (PR-2)
    - nudge — broadcast to idle peers (PR-3)
    - heartbeat / stall detection (PR-4)

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
