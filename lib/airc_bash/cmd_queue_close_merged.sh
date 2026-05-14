# Sourced by cmd_queue.sh. queue close-merged workflow.

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
