# Sourced by cmd_queue.sh. Shared queue-card parsing and mutation primitives.

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

  # Hand to Rust: parse envelope, apply mutations, rewrite body with
  # status-log entry. Body + mutations pass via temp files so shell only
  # owns process orchestration.
  local body_file mut_file
  body_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-body.XXXXXX") || die "queue: mktemp failed"
  mut_file=$(mktemp "${TMPDIR:-/tmp}/airc-queue-muts.XXXXXX") || die "queue: mktemp failed"
  printf '%s' "$current_body" >"$body_file"
  printf '%s' "$mutations" >"$mut_file"

  local timestamp
  timestamp=$(date -u +"%Y-%m-%dT%H:%MZ")

  local new_body
  if ! new_body=$("$(airc_core_bin)" queue-card mutate-body \
    --body-file "$body_file" \
    --mutations-file "$mut_file" \
    --log-msg "$log_msg" \
    --timestamp "$timestamp"); then
    rm -f "$body_file" "$mut_file"
    die "queue mutate: Rust helper failed: $new_body"
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
