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
