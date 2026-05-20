# shellcheck shell=bash
# Sourced by cmd_queue.sh. Read-only queue steward / PM digestion view.

_cmd_queue_steward() {
  local target_repo=""
  local limit=100
  local output_json=0
  local stale_after="30m"
  local owner=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_queue_steward_help
        return 0
        ;;
      --repo)        shift; target_repo="${1:-}" ;;
      --limit)       shift; limit="${1:-100}" ;;
      --stale-after) shift; stale_after="${1:-30m}" ;;
      --owner)       shift; owner="${1:-}" ;;
      --json)        output_json=1 ;;
      -*) die "queue steward: unknown flag: $1" ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          die "queue steward: too many positional args (use: queue steward [owner/repo])"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    target_repo=$(_airc_queue_detect_repo_from_cwd || true)
  fi
  if [ -z "$target_repo" ]; then
    die "queue steward: no <owner/repo> given and could not detect one from \$PWD's git remote. Run inside a GitHub checkout or pass --repo owner/repo."
  fi
  case "$target_repo" in
    */*) : ;;
    *) die "queue steward: target must be owner/repo, got: $target_repo" ;;
  esac
  case "$limit" in
    ''|*[!0-9]*) die "queue steward: --limit must be a positive integer (got: $limit)" ;;
  esac
  if [ "$limit" -lt 1 ]; then
    die "queue steward: --limit must be >= 1 (got: $limit)"
  fi
  if [ -z "$owner" ]; then
    owner=$(_airc_queue_resolve_name)
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "queue steward: 'gh' CLI is required."
  fi

  local raw_json
  if ! raw_json=$(gh issue list \
    --repo "$target_repo" \
    --label "airc-queue" \
    --state open \
    --limit "$limit" \
    --json number,title,url,body,updatedAt,createdAt 2>&1); then
    die "queue steward: gh issue list failed for $target_repo: $raw_json"
  fi

  local raw_json_file
  raw_json_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-steward.XXXXXX") || die "queue steward: mktemp failed"
  printf '%s' "$raw_json" >"$raw_json_file"

  local steward_args=(
    queue-card steward
    --repo "$target_repo"
    --owner "$owner"
    --stale-after "$stale_after"
    --raw-json-file "$raw_json_file"
  )
  if [ "$output_json" -eq 1 ]; then
    steward_args+=(--json)
  fi

  "$(airc_rs_bin)" "${steward_args[@]}"
  local steward_status=$?
  rm -f "$raw_json_file"
  return "$steward_status"
}
