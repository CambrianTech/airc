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
#                 metronome  — configure automatic queue-next idle pulses.
#                 nudge      — surface a card OR repo-scoped status sweep.                [PR-3+]
#                 adopt      — convert an existing issue into a queue card.
#                 pongs      — summarize repo-nudge pong replies.
#                 availability — summarize queue owners + recent peer activity.
#                 close-merged — auto-close cards referenced by a merged PR.
#                 staleness — warn when a PR branch would revert base work.
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

# Help text is split out so cmd_queue.sh keeps behavior logic readable.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_queue_help.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_queue_help.sh
  source "$_airc_lib_dir/airc_bash/cmd_queue_help.sh"
else
  echo "ERROR: airc_bash/cmd_queue_help.sh not found via lib-dir resolver." >&2
  return 1 2>/dev/null || exit 1
fi

# Shared card parsing/mutation primitives used by queue verbs.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_queue_card.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_queue_card.sh
  source "$_airc_lib_dir/airc_bash/cmd_queue_card.sh"
else
  echo "ERROR: airc_bash/cmd_queue_card.sh not found via lib-dir resolver." >&2
  return 1 2>/dev/null || exit 1
fi

# close-merged is a large PR/issue workflow; keep it out of the core router.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_queue_close_merged.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_queue_close_merged.sh
  source "$_airc_lib_dir/airc_bash/cmd_queue_close_merged.sh"
else
  echo "ERROR: airc_bash/cmd_queue_close_merged.sh not found via lib-dir resolver." >&2
  return 1 2>/dev/null || exit 1
fi

# plan is the cohesive queue dashboard: priorities, lanes, active owners,
# stale claims, and concrete next actions.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_queue_plan.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_queue_plan.sh
  source "$_airc_lib_dir/airc_bash/cmd_queue_plan.sh"
else
  echo "ERROR: airc_bash/cmd_queue_plan.sh not found via lib-dir resolver." >&2
  return 1 2>/dev/null || exit 1
fi

# steward is the read-only PM digestion layer over the queue plan.
if [ -n "${_airc_lib_dir:-}" ] && [ -f "$_airc_lib_dir/airc_bash/cmd_queue_steward.sh" ]; then
  # shellcheck source=lib/airc_bash/cmd_queue_steward.sh
  source "$_airc_lib_dir/airc_bash/cmd_queue_steward.sh"
else
  echo "ERROR: airc_bash/cmd_queue_steward.sh not found via lib-dir resolver." >&2
  return 1 2>/dev/null || exit 1
fi

cmd_queue() {
  # Top-level router. Validate + dispatch to _cmd_queue_<subcommand>.
  local subcmd="${1:-}"
  shift || true

  case "$subcmd" in
    -h|--help)
      _airc_queue_help
      return 0
      ;;
    "")
      _cmd_queue_plan "$@"
      ;;
    --repo|--limit|--stale-after|--owner|--json)
      _cmd_queue_plan "$subcmd" "$@"
      ;;
    */*)
      _cmd_queue_plan "$subcmd" "$@"
      ;;
    add)
      _cmd_queue_add "$@"
      ;;
    plan|priorities|kanban)
      _cmd_queue_plan "$@"
      ;;
    steward|digest|pm)
      _cmd_queue_steward "$@"
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
    dispatch|handout)
      _cmd_queue_dispatch "$@"
      ;;
    metronome|pulse)
      _cmd_queue_metronome "$@"
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
    staleness|stale-pr)
      _cmd_queue_staleness "$@"
      ;;
    *)
      die "queue: unknown subcommand: $subcmd (try: plan, add, list, claim, release, set-status, heartbeat, stale, next, dispatch, metronome, nudge, adopt, pongs, availability, close-merged, staleness)"
      ;;
  esac
}

# _cmd_queue_dispatch — personalized hand-out of the next claimable card
# to a specific idle agent (continuum#1192).
#
# `airc queue metronome` already broadcasts a "queue is open" pulse to
# the room, but a broadcast ≠ "for me" — agents see the pulse and stay
# idle because nothing names them. This verb closes that loop by
# computing the top candidate FOR a named agent and DM'ing them the
# exact claim + lane commands.
#
# Usage:
#   airc queue dispatch <agent> [<owner/repo>] [--message "..."] [--limit N] [--repo-root PATH] [--dry-run]
#
#   <agent>      target peer (with or without leading @)
#   <owner/repo> defaults to detected from $PWD (same rules as queue next)
#   --message    optional one-line suffix to add to the DM
#   --dry-run    print what would be sent, do not actually DM
#
# Behavior: runs `queue next --owner <agent> --json --limit 1` to get
# the top candidate, then DMs the agent a one-line message containing:
#   - card title
#   - issue URL
#   - the exact `airc queue claim` command they should run
#   - the exact `airc lane create` command (if branch is suggested)
_cmd_queue_dispatch() {
  local target_agent=""
  local target_repo=""
  local extra_message=""
  local limit=10
  local repo_root=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        cat <<'EOF'
airc queue dispatch — personalized hand-out of next claimable card

USAGE
  airc queue dispatch <agent> [<owner/repo>] [--message "..."] [--limit N] [--repo-root PATH] [--dry-run]

ARGS
  <agent>           target peer name (with or without leading @)
  <owner/repo>      default: detected from $PWD's git remote

FLAGS
  --message TEXT    extra context to append to the DM
  --limit N         max queue cards to scan before picking top ranked card
  --repo-root PATH  include repo path in suggested lane command
  --dry-run         print what would be sent, do not actually DM

DESCRIPTION
  Computes the top claimable card FOR the named agent (via the same
  ranker as `queue next --owner <agent>`) and DMs them the one-line
  hand-out with the exact `airc queue claim` + `airc lane create`
  commands ready to copy. Closes the metronome's broadcast→personalized
  gap (continuum#1192).

EXAMPLES
  airc queue dispatch @bigmama
  airc queue dispatch claude-tab-2 CambrianTech/continuum
  airc queue dispatch @codex-main --message "you've been idle 5min — pickup?"
  airc queue dispatch @anvil --dry-run
EOF
        return 0
        ;;
      --message)
        shift
        extra_message="${1:-}"
        ;;
      --limit)
        shift
        limit="${1:-10}"
        ;;
      --repo-root)
        shift
        repo_root="${1:-}"
        ;;
      --dry-run)
        dry_run=1
        ;;
      -*)
        die "queue dispatch: unknown flag: $1"
        ;;
      *)
        if [ -z "$target_agent" ]; then
          target_agent="$1"
        elif [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue dispatch: too many positional args (expected: <agent> [<owner/repo>])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_agent" ]; then
    die "queue dispatch: missing <agent> (try: airc queue dispatch @<peer>)"
  fi
  # Strip leading '@' so the lookup name matches what queue uses internally.
  target_agent="${target_agent#@}"

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue dispatch: no <owner/repo> given and could not detect from \$PWD's git remote. Pass owner/repo explicitly."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue dispatch: target must be owner/repo, got: $target_repo" ;;
  esac

  if ! command -v gh >/dev/null 2>&1; then
    die "queue dispatch: 'gh' CLI is required."
  fi
  case "$limit" in
    ''|*[!0-9]*) die "queue dispatch: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue dispatch: --limit must be >= 1 (got: $limit)"
  fi

  # Pull the top candidate via JSON output of the existing next ranker.
  # Tee to a tempfile so the python parser can read structured input
  # without having to re-shell to gh again. Scan a bounded set, then DM
  # the first ranked result.
  local next_json
  local next_args=("$target_repo" --owner "$target_agent" --limit "$limit" --json)
  if [ -n "$repo_root" ]; then
    next_args+=(--repo-root "$repo_root")
  fi
  if ! next_json=$(_cmd_queue_next "${next_args[@]}"); then
    die "queue dispatch: queue next lookup failed for $target_agent on $target_repo"
  fi
  if [ -z "$next_json" ]; then
    die "queue dispatch: queue next returned empty stdout. Try 'airc queue next $target_repo --owner $target_agent --limit 1 --json' directly to debug."
  fi

  # Write JSON to a tempfile and pass the path as argv. We can't use a
  # stdin pipe here because the heredoc that carries the python source
  # below also redirects stdin (only one fd 0).
  local next_json_file
  next_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-dispatch.XXXXXX") || die "queue dispatch: mktemp failed"
  printf '%s' "$next_json" >"$next_json_file"

  if [ -z "$next_json" ] || [ "$next_json" = "[]" ] || [ "$next_json" = "{}" ]; then
    die "queue dispatch: no claimable cards for $target_agent on $target_repo (queue is empty for this agent)"
  fi

  # Extract the top candidate's fields. The next-ranker's JSON shape is
  # `{ "candidates": [{ "number", "title", "url", "claim_command",
  # "lane_command", ... }] }`.
  #
  # Read the JSON from a tempfile (not stdin) because the heredoc carrying
  # the python source uses fd 0; can't redirect stdin twice.
  local dm_text
  dm_text=$("$(airc_rs_bin)" queue-card dispatch-message \
    --target-agent "$target_agent" \
    --extra-message "$extra_message" \
    --next-json-file "$next_json_file")
  rm -f "$next_json_file"
  if [ -z "$dm_text" ]; then
    die "queue dispatch: failed to format hand-out from queue-next output"
  fi

  if [ "$dry_run" = "1" ]; then
    echo "[dry-run] would DM @${target_agent}:"
    echo "${dm_text}"
    return 0
  fi

  if ! cmd_send "@${target_agent}" "$dm_text"; then
    die "queue dispatch: cmd_send to @${target_agent} failed"
  fi
  echo "Dispatched top card to @${target_agent} on ${target_repo}."
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
  local owner_explicit=0
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
      --owner)         shift; card_owner="${1:-}"; owner_explicit=1 ;;
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
  if [ "$card_owner" = "unclaimed" ]; then
    card_owner=""
  fi
  if [ -z "$card_owner" ] && [ "$owner_explicit" -eq 0 ]; then
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
  local check_staleness=0
  local no_fetch_staleness=0
  local repo_root=""

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
      --check-staleness) check_staleness=1 ;;
      --no-fetch-staleness) no_fetch_staleness=1 ;;
      --repo-root) shift; repo_root="${1:-}" ;;
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

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-list.XXXXXX") || die "queue list: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  local list_args=(queue-card list --repo "$target_repo" --owner "$filter_owner" --status "$filter_status" --raw-json-file "$raw_json_file")
  [ "$output_json" -eq 1 ] && list_args+=(--json)
  "$(airc_rs_bin)" "${list_args[@]}"
  local py_status=$?
  rm -f "$raw_json_file"
  if [ "$py_status" -eq 0 ] && [ "$output_json" -eq 0 ] && [ "$check_staleness" -eq 1 ]; then
    _airc_queue_list_staleness_sweep "$target_repo" "$filter_status" "$repo_root" "$limit" "$no_fetch_staleness"
    py_status=$?
  fi
  return "$py_status"
}

_airc_queue_list_staleness_sweep() {
  local target_repo="$1"
  local filter_status="$2"
  local repo_root="$3"
  local limit="$4"
  local no_fetch="$5"

  if [ -n "$filter_status" ] && [ "$filter_status" != "review" ]; then
    return 0
  fi
  if [ -z "$repo_root" ]; then
    repo_root="."
  fi
  if ! git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
    printf '\nstaleness: skipped (repo root not a git checkout: %s)\n' "$repo_root" >&2
    return 0
  fi
  if ! command -v gh >/dev/null 2>&1; then
    printf '\nstaleness: skipped (gh CLI unavailable)\n' >&2
    return 0
  fi

  local raw_json raw_json_file refs_file
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body 2>&1); then
    printf '\nstaleness: skipped (gh issue list failed: %s)\n' "$raw_json" >&2
    return 0
  fi

  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-list-stale-json.XXXXXX") || return 1
  refs_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-list-stale-refs.XXXXXX") || { rm -f "$raw_json_file"; return 1; }
  printf '%s' "$raw_json" >"$raw_json_file"

  "$AIRC_PYTHON" - "$target_repo" "$raw_json_file" >"$refs_file" <<'PYEOF'
import json, re, sys
repo, path = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    issues = json.load(f)
card_re = re.compile(r'```json\s*\n(.*?)\n\s*```', re.DOTALL)
for issue in issues:
    card = {}
    for m in card_re.finditer(issue.get("body", "") or ""):
        try:
            parsed = json.loads(m.group(1).strip())
        except Exception:
            continue
        if isinstance(parsed, dict) and parsed.get("kind") == "airc-queue-card-v1":
            card = parsed
            break
    if (card.get("status") or "").strip() != "review":
        continue
    text = "\n".join([
        str(issue.get("title") or ""),
        str(issue.get("body") or ""),
        str(card.get("next_action") or ""),
        str(card.get("evidence") or ""),
    ])
    refs = []
    for m in re.finditer(r'https://github\.com/([^/]+/[^/]+)/(?:pull|pulls)/(\d+)|\b([A-Za-z0-9][A-Za-z0-9._-]*)/([A-Za-z0-9][A-Za-z0-9._-]*)#(\d+)\b|(?<![A-Za-z0-9_/])#(\d+)\b', text):
        if m.group(1):
            refs.append(f"{m.group(1)}#{m.group(2)}")
        elif m.group(3):
            refs.append(f"{m.group(3)}/{m.group(4)}#{m.group(5)}")
        elif m.group(6):
            refs.append(f"{repo}#{m.group(6)}")
    for ref in dict.fromkeys(refs):
        print(ref)
        break
PYEOF

  if [ -s "$refs_file" ]; then
    printf '\n# staleness sweep — review cards\n'
    while IFS= read -r ref; do
      [ -z "$ref" ] && continue
      if [ "$no_fetch" = "1" ]; then
        _cmd_queue_staleness "$ref" --repo-root "$repo_root" --limit-lines 12 --no-fetch || true
      else
        _cmd_queue_staleness "$ref" --repo-root "$repo_root" --limit-lines 12 || true
      fi
    done <"$refs_file"
  fi

  rm -f "$raw_json_file" "$refs_file"
  return 0
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

  "$(airc_rs_bin)" queue-card body \
    --id "$id" \
    --branch "$branch" \
    --owner "$owner" \
    --status "$status" \
    --blockers "$blockers" \
    --env "$env" \
    --evidence "$evidence" \
    --next-action "$next_action" \
    --last-heartbeat "$last_heartbeat"
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
  local force=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_claim_help
        return 0
        ;;
      --owner)    shift; new_owner="${1:-}" ;;
      --status)   shift; new_status="${1:-}" ;;
      --dry-run)  dry_run=1 ;;
      --force)    force=1 ;;
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

  _airc_queue_claim_guard "$issue_url" "$new_owner" "$force"

  _airc_queue_mutate_card "$issue_url" "$dry_run" \
    "claim by $new_owner -> status=$new_status" \
    --set "owner=$new_owner" \
    --set "status=$new_status" \
    --set "last_heartbeat=$heartbeat"
}

_airc_queue_claim_guard() {
  local issue_url="$1" new_owner="$2" force="$3"

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

  local body_file
  body_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-claim.XXXXXX") || die "queue: mktemp failed"
  printf '%s' "$current_body" >"$body_file"

  local fields
  if ! fields=$("$(airc_rs_bin)" queue-card claim-fields --body-file "$body_file"); then
    rm -f "$body_file"
    die "$fields"
  fi
  rm -f "$body_file"

  local current_owner current_status
  current_owner=$(printf '%s\n' "$fields" | sed -n '1p')
  current_status=$(printf '%s\n' "$fields" | sed -n '2p')

  if [ "$force" -eq 0 ] \
    && [ -n "$current_owner" ] \
    && [ "$current_owner" != "$new_owner" ] \
    && [ "$current_status" != "merged" ]; then
    die "queue claim: card already claimed by '$current_owner' (status=${current_status:-unknown}). Use --force to take over, or pick a different card via airc queue next."
  fi
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

  local stale_args=(queue-card stale --repo "$target_repo" --stale-after "$stale_after" --raw-json-file "$raw_json_file")
  [ "$output_json" -eq 1 ] && stale_args+=(--json)
  "$(airc_rs_bin)" "${stale_args[@]}"
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

  local next_args=(queue-card next --repo "$target_repo" --owner "$owner" --base "$base" --repo-root "$repo_root" --raw-json-file "$raw_json_file")
  [ "$output_json" -eq 1 ] && next_args+=(--json)
  "$(airc_rs_bin)" "${next_args[@]}"
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

_cmd_queue_metronome() {
  # Configure monitor-loop queue-next pulses. This is the automated half of
  # `queue next`: monitor can periodically call the same primitive and
  # broadcast candidates when an agent/session is quiet.
  #
  # airc#607 / continuum#1192 fan-out: when --all is set, the monitor
  # consumer iterates the active channel roster (recent senders in this
  # scope's messages.jsonl) and dispatches per agent — closes the gap
  # where a single-owner metronome could only ever feed one agent and
  # left the rest of the room idle. The longer-term Rust port lives in
  # airc#628 (Rust queue-dispatch substrate); this bash branch is the
  # bridge so the immediate UX bug isn't blocked on that lane.
  local target_repo=""
  local interval=300
  local owner=""
  local limit=10
  local repo_root=""
  local off=0
  local status=0
  local all_roster=0
  local roster_window=86400

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_metronome_help
        return 0
        ;;
      off|disable)
        off=1
        ;;
      status)
        status=1
        ;;
      --interval)       shift; interval="${1:-300}" ;;
      --owner)          shift; owner="${1:-}" ;;
      --limit)          shift; limit="${1:-10}" ;;
      --repo-root)      shift; repo_root="${1:-}" ;;
      --all|--roster)   all_roster=1 ;;
      --roster-window)  shift; roster_window="${1:-86400}" ;;
      -*) die "queue metronome: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue metronome: too many positional args (use: queue metronome <owner/repo>)"
        fi
        ;;
    esac
    shift || true
  done

  # --all is mutually exclusive with --owner: --all means "iterate the
  # channel roster"; --owner means "always this one agent". Mixing them
  # is ambiguous and likely an operator mistake (e.g. forgot to clear an
  # old --owner). Refuse early so the config file never lands in a
  # contradictory state the monitor would silently resolve one way.
  if [ "$all_roster" -eq 1 ] && [ -n "$owner" ]; then
    die "queue metronome: --all is mutually exclusive with --owner (got both: --owner $owner, --all)"
  fi
  case "$roster_window" in
    ''|*[!0-9]*) die "queue metronome: --roster-window must be a positive integer seconds value (got: $roster_window)" ;;
  esac
  if [ "$roster_window" -lt 60 ]; then
    die "queue metronome: --roster-window must be >= 60 seconds (got: $roster_window)"
  fi

  local config_file="$AIRC_WRITE_DIR/queue_metronome"

  if [ "$off" -eq 1 ]; then
    rm -f "$config_file" "$AIRC_WRITE_DIR/queue_metronome_last"
    rm -rf "$AIRC_WRITE_DIR/queue_metronome_recent"
    echo "  Queue metronome off."
    return 0
  fi

  if [ "$status" -eq 1 ] || { [ -z "$target_repo" ] && [ -f "$config_file" ]; }; then
    if [ -f "$config_file" ]; then
      echo "  Queue metronome on:"
      sed 's/^/    /' "$config_file"
    else
      echo "  Queue metronome off."
    fi
    return 0
  fi

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue metronome: pass <owner/repo> or run inside a GitHub-backed checkout"
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue metronome: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$interval" in
    ''|*[!0-9]*) die "queue metronome: --interval must be a positive integer seconds value (got: $interval)" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue metronome: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$interval" -lt 30 ]; then
    die "queue metronome: --interval must be >= 30 seconds to avoid spam (got: $interval)"
  fi
  if [ "$limit" -lt 1 ]; then
    die "queue metronome: --limit must be >= 1 (got: $limit)"
  fi
  # airc#607: the sentinel owner=*roster* tells the monitor consumer to
  # enumerate recent senders from messages.jsonl rather than dispatch to
  # one fixed agent. Operators opt in via --all; default stays single-
  # agent to preserve existing behaviour for anyone scripting against it.
  if [ "$all_roster" -eq 1 ]; then
    owner='*roster*'
  elif [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  mkdir -p "$AIRC_WRITE_DIR"
  {
    printf 'repo=%s\n' "$target_repo"
    printf 'interval=%s\n' "$interval"
    printf 'owner=%s\n' "$owner"
    printf 'limit=%s\n' "$limit"
    printf 'repo_root=%s\n' "$repo_root"
    printf 'roster_window=%s\n' "$roster_window"
  } > "$config_file"
  rm -f "$AIRC_WRITE_DIR/queue_metronome_last"
  # Per-recipient dedup state lives under queue_metronome_recent/<name>.
  # Wipe on reconfigure so the next pulse starts clean rather than
  # silently suppressing recipients whose last-ping pre-dates the new
  # cadence.
  rm -rf "$AIRC_WRITE_DIR/queue_metronome_recent"

  if [ "$all_roster" -eq 1 ]; then
    echo "  Queue metronome every ${interval}s for ${target_repo}, fan-out across active roster (window ${roster_window}s)."
    echo "  Monitor will run: airc queue dispatch <each-recent-sender> ${target_repo}"
  else
    echo "  Queue metronome every ${interval}s for ${target_repo} as ${owner}."
    echo "  Monitor will run: airc queue dispatch ${owner} ${target_repo}"
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
  # Tracks whether --owner was explicitly passed (vs defaulting later to
  # the current scope's resolve_name). Needed for airc#613: when an
  # operator explicitly says `--owner unclaimed` for bulk adoption, we
  # should NOT silently auto-fill the owner with the running agent's
  # name on top of the unclaimed normalization. Without this flag we
  # couldn't distinguish "user said no owner" from "user said nothing".
  local owner_explicit=0
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
      --owner)          shift; card_owner="${1:-}"; owner_explicit=1 ;;
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

  # airc#613 — normalize the "unclaimed" sentinel to no-owner. Operators
  # use `--owner unclaimed` for bulk-adoption ("this card is available;
  # someone else will claim it later"), but writing the literal string
  # into the envelope made the subsequent `airc queue claim` fail
  # collision protection (airc#612) because owner=unclaimed reads as an
  # active owner. Treat the sentinel as the absence-of-owner intent the
  # operator meant. The card body builder skips the owner field
  # entirely when card_owner is empty, which is the right shape for
  # "no active owner / available for claim".
  if [ "$card_owner" = "unclaimed" ]; then
    card_owner=""
  fi

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
  # Auto-fill default owner ONLY if --owner wasn't explicitly given.
  # An explicit --owner "" or --owner unclaimed (normalized above to "")
  # signals "no owner / available" and must NOT be silently overwritten
  # with the running agent's resolve_name. airc#613.
  if [ -z "$card_owner" ] && [ "$owner_explicit" -eq 0 ]; then
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
  local adopt_args=(queue-card adopt-body --issue-json-file "$issue_json_file" --queue-body-file "$body_file")
  [ "$force" = "1" ] && adopt_args+=(--force)
  adopted_body=$("$(airc_rs_bin)" "${adopt_args[@]}")
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
  if ! summary=$("$(airc_rs_bin)" queue-card nudge-summary --raw-json-file "$raw_json_file"); then
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
  if ! card_meta=$("$(airc_rs_bin)" queue-card nudge-card-meta --issue-file "$issue_file"); then
    rm -f "$issue_file"
    die "queue nudge: $repo#$issue_num is not a valid airc-queue-card-v1 envelope"
  fi
  rm -f "$issue_file"

  local card_title card_status card_owner
  card_title=$(printf '%s\n' "$card_meta" | sed -n '1p')
  card_status=$(printf '%s\n' "$card_meta" | sed -n '2p')
  card_owner=$(printf '%s\n' "$card_meta" | sed -n '3p')

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
    if owner == "unclaimed":
        owner = ""
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
    if owner == "unclaimed":
        owner = ""
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

_cmd_queue_staleness() {
  # Detect stale PR branches that would erase already-merged base work.
  #
  # This is intentionally git-side and read-only. GitHub's mergeability and
  # CI can both be green while the PR head lacks recent base commits; this
  # command answers the reviewer question: "what current-base lines in files
  # touched by this PR are absent from the PR head?"
  local pr_url=""
  local repo_root=""
  local base_ref=""
  local head_ref=""
  local pr_repo=""
  local pr_num=""
  local output_json=0
  local no_fetch=0
  local limit_lines=40

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_staleness_help
        return 0
        ;;
      --repo-root)   shift; repo_root="${1:-}" ;;
      --base)        shift; base_ref="${1:-}" ;;
      --head)        shift; head_ref="${1:-}" ;;
      --repo)        shift; pr_repo="${1:-}" ;;
      --pr)          shift; pr_num="${1:-}" ;;
      --limit-lines) shift; limit_lines="${1:-40}" ;;
      --no-fetch)    no_fetch=1 ;;
      --json)        output_json=1 ;;
      -*) die "queue staleness: unknown flag: $1" ;;
      *)
        if [ -z "$pr_url" ]; then
          pr_url="$1"
        else
          die "queue staleness: too many positional args (use: queue staleness <pr-url|owner/repo#PR>)"
        fi
        ;;
    esac
    shift || true
  done

  case "$limit_lines" in
    ''|*[!0-9]*) die "queue staleness: --limit-lines must be a positive integer (got: $limit_lines)" ;;
  esac
  if [ "$limit_lines" -lt 1 ]; then
    die "queue staleness: --limit-lines must be >= 1 (got: $limit_lines)"
  fi

  if [ -n "$pr_url" ]; then
    if [[ "$pr_url" =~ ^https://github\.com/([^/]+/[^/]+)/(pull|pulls|issues)/([0-9]+) ]]; then
      pr_repo="${BASH_REMATCH[1]}"
      pr_num="${BASH_REMATCH[3]}"
    elif [[ "$pr_url" =~ ^([^/]+/[^/]+)#([0-9]+)$ ]]; then
      pr_repo="${BASH_REMATCH[1]}"
      pr_num="${BASH_REMATCH[2]}"
    else
      die "queue staleness: <pr-url> must be a GitHub PR URL or owner/repo#N (got: $pr_url)"
    fi
  fi

  if [ -z "$repo_root" ]; then
    repo_root="."
  fi
  if ! git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1; then
    die "queue staleness: --repo-root must point at a git checkout (got: $repo_root)"
  fi

  local pr_canonical_url=""
  if [ -n "$pr_repo" ] && [ -n "$pr_num" ] && { [ -z "$base_ref" ] || [ -z "$head_ref" ]; }; then
    if ! command -v gh >/dev/null 2>&1; then
      die "queue staleness: 'gh' CLI is required when --base/--head are not supplied."
    fi
    local pr_blob
    if ! pr_blob=$(gh pr view "$pr_num" --repo "$pr_repo" --json baseRefName,headRefName,url,title 2>&1); then
      die "queue staleness: gh pr view failed for $pr_repo#$pr_num: $pr_blob"
    fi
    local pr_file pr_meta
    pr_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-staleness-pr.XXXXXX") || die "queue staleness: mktemp failed"
    printf '%s' "$pr_blob" >"$pr_file"
    if ! pr_meta=$("$AIRC_PYTHON" - "$pr_file" <<'PYEOF'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    pr = json.load(f)
print(f"{pr.get('baseRefName') or ''}\t{pr.get('headRefName') or ''}\t{pr.get('url') or ''}")
PYEOF
    ); then
      rm -f "$pr_file"
      die "queue staleness: PR JSON parse failed"
    fi
    rm -f "$pr_file"
    [ -z "$base_ref" ] && base_ref=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $1}')
    [ -z "$head_ref" ] && head_ref=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $2}')
    pr_canonical_url=$(printf '%s' "$pr_meta" | awk -F'\t' '{print $3}')
  fi

  if [ -z "$base_ref" ] || [ -z "$head_ref" ]; then
    die "queue staleness: need a PR ref or explicit --base <ref> --head <ref>"
  fi

  local base_git_ref="$base_ref"
  local head_git_ref="$head_ref"
  local temp_head_ref=""
  if [ "$no_fetch" -eq 0 ]; then
    if [ -n "$pr_repo" ] && [ -n "$pr_num" ]; then
      temp_head_ref="refs/remotes/origin/airc-staleness-pr-$pr_num"
      git -C "$repo_root" fetch --quiet origin "$base_ref" "pull/$pr_num/head:$temp_head_ref" \
        || die "queue staleness: git fetch failed for origin $base_ref and pull/$pr_num/head"
      base_git_ref="origin/$base_ref"
      head_git_ref="$temp_head_ref"
    else
      git -C "$repo_root" fetch --quiet origin "$base_ref" "$head_ref" \
        || die "queue staleness: git fetch failed for origin $base_ref $head_ref"
      base_git_ref="origin/$base_ref"
      head_git_ref="origin/$head_ref"
    fi
  fi

  local merge_base
  if ! merge_base=$(git -C "$repo_root" merge-base "$base_git_ref" "$head_git_ref" 2>/dev/null); then
    die "queue staleness: could not compute merge-base for $base_git_ref and $head_git_ref"
  fi

  local files_file diff_file base_new_file
  files_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-staleness-files.XXXXXX") || die "queue staleness: mktemp failed"
  diff_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-staleness-diff.XXXXXX") || die "queue staleness: mktemp failed"
  base_new_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-staleness-base.XXXXXX") || die "queue staleness: mktemp failed"

  git -C "$repo_root" diff --name-only "$merge_base..$head_git_ref" >"$files_file" \
    || { rm -f "$files_file" "$diff_file" "$base_new_file"; die "queue staleness: git diff --name-only failed"; }

  if [ ! -s "$files_file" ]; then
    if [ "$output_json" -eq 1 ]; then
      printf '{"status":"ok","warnings":[],"message":"OK: PR has no changed files relative to merge-base"}\n'
    else
      printf 'OK: PR has no changed files relative to merge-base.\n'
    fi
    rm -f "$files_file" "$diff_file" "$base_new_file"
    return 0
  fi

  git -C "$repo_root" diff --unified=0 "$merge_base..$base_git_ref" -- $(cat "$files_file") >"$base_new_file" \
    || { rm -f "$files_file" "$diff_file" "$base_new_file"; die "queue staleness: git diff merge-base..base failed"; }

  git -C "$repo_root" diff --unified=0 "$head_git_ref..$base_git_ref" -- $(cat "$files_file") >"$diff_file" \
    || { rm -f "$files_file" "$diff_file" "$base_new_file"; die "queue staleness: git diff head..base failed"; }

  AIRC_QUEUE_STALENESS_LIMIT="$limit_lines" "$AIRC_PYTHON" - \
      "$repo_root" "$pr_repo" "$pr_num" "$base_ref" "$head_ref" "$base_git_ref" "$head_git_ref" \
      "$merge_base" "$pr_canonical_url" "$output_json" "$files_file" "$diff_file" "$base_new_file" <<'PYEOF'
import json, os, re, subprocess, sys

(
    repo_root,
    pr_repo,
    pr_num,
    base_ref,
    head_ref,
    base_git_ref,
    head_git_ref,
    merge_base,
    pr_url,
    output_json_raw,
    files_path,
    diff_path,
    base_new_path,
) = sys.argv[1:14]
output_json = output_json_raw == "1"
limit = int(os.environ.get("AIRC_QUEUE_STALENESS_LIMIT", "40"))

with open(files_path, "r", encoding="utf-8") as f:
    touched_files = [line.strip() for line in f if line.strip()]
with open(diff_path, "r", encoding="utf-8", errors="replace") as f:
    diff_lines = f.read().splitlines()
with open(base_new_path, "r", encoding="utf-8", errors="replace") as f:
    base_new_lines = f.read().splitlines()

def plus_lines_by_file(lines):
    out = {}
    current = ""
    for raw in lines:
        if raw.startswith("+++ b/"):
            current = raw[6:]
            continue
        if not raw.startswith("+") or raw.startswith("+++"):
            continue
        content = raw[1:]
        if not content.strip():
            continue
        out.setdefault(current, set()).add(content)
    return out

base_added = plus_lines_by_file(base_new_lines)

warnings = []
current_file = ""
for line in diff_lines:
    if line.startswith("+++ b/"):
        current_file = line[6:]
        continue
    if not line.startswith("+") or line.startswith("+++"):
        continue
    content = line[1:]
    if not content.strip():
        continue
    if content not in base_added.get(current_file, set()):
        continue
    if len(content) > 240:
        content = content[:240] + "..."
    origin = ""
    try:
        proc = subprocess.run(
            ["git", "-C", repo_root, "log", "--format=%h %s", "-n", "1", "-S", content, base_git_ref, "--", current_file],
            capture_output=True,
            text=True,
            timeout=5,
        )
        origin = proc.stdout.strip().splitlines()[0] if proc.stdout.strip() else ""
    except Exception:
        origin = ""
    warnings.append({"file": current_file, "line": content, "origin": origin})
    if len(warnings) >= limit:
        break

payload = {
    "repo": pr_repo,
    "pr": pr_num,
    "url": pr_url,
    "base": base_ref,
    "head": head_ref,
    "base_git_ref": base_git_ref,
    "head_git_ref": head_git_ref,
    "merge_base": merge_base,
    "touched_files": touched_files,
    "warning_count": len(warnings),
    "warnings": warnings,
}

if output_json:
    print(json.dumps(payload, indent=2))
elif not warnings:
    print(f"OK: no stale conflicts detected for {pr_repo + '#' + pr_num if pr_repo and pr_num else head_ref}.")
    print(f"base={base_ref} head={head_ref} files_touched={len(touched_files)}")
else:
    label = f"{pr_repo}#{pr_num}" if pr_repo and pr_num else head_ref
    print(f"WARN: {label} branch may erase current-base work.")
    print(f"base={base_ref} head={head_ref} files_touched={len(touched_files)} missing_base_lines_sample={len(warnings)}")
    print("Rebase the PR branch onto the current base before merge, then rerun this command.")
    for item in warnings:
        origin = f" ({item['origin']})" if item["origin"] else ""
        print(f"  - {item['file']}: {item['line']}{origin}")
PYEOF
  local py_status=$?
  rm -f "$files_file" "$diff_file" "$base_new_file"
  return "$py_status"
}
