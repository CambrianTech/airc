# Sourced by airc. cmd_approve + cmd_decrypt_approval — knock follow-up
# verbs (airc#559 PR-2).
#
# Functions exported back to airc's dispatch:
#   cmd_approve            — approver-side: read knocker_pub from a knock
#                            issue, encrypt the private-room invite to
#                            that pubkey via per-approval ephemeral ECDH,
#                            post the ciphertext as a comment.
#   cmd_decrypt_approval   — knocker-side: fetch the approval comment,
#                            decrypt with the saved ephemeral priv key,
#                            print the join string. Manual handoff for
#                            now; PR-2c automates this via state.
#
# External cross-references (resolved at call time): die, AIRC_PYTHON,
# `gh` CLI; airc_core.knock_crypto module.
#
# Crypto contract: per-knock + per-approval X25519 ephemerals → ECDH +
# HKDF-SHA256 (info=b"airc-knock-approve-v1") → ChaCha20-Poly1305 AEAD.
# Forward-secret: even if a long-term identity key leaks YEARS later,
# every prior approval's join string is unrecoverable because the
# ephemerals were never written to disk past one-shot use.

cmd_approve() {
  # Approver-side. Read the knock issue, encrypt the join string to the
  # knocker's ephemeral pubkey, post as a comment.
  #
  #   airc approve <issue-url> [--invite <string>] [--dry-run]
  #
  # When --invite is omitted, the join string defaults to your CURRENT
  # SCOPE'S invite (via `airc invite`) — convenient for "add this knocker
  # to the room I'm hosting." When you want to invite to a DIFFERENT
  # room, pass the invite string explicitly.

  local issue_url=""
  local invite_string=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_approve_help
        return 0
        ;;
      --invite)
        shift
        invite_string="${1:-}"
        ;;
      --dry-run)
        dry_run=1
        ;;
      -*)
        die "approve: unknown flag: $1"
        ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "approve: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ]; then
    _airc_approve_help >&2
    return 1
  fi

  # Validate URL shape — github.com/owner/repo/issues/N. Accept the
  # short form `owner/repo#N` too because it's natural to type after
  # reading `gh issue list` output.
  local repo issue_num
  if [[ "$issue_url" =~ ^https://github\.com/([^/]+/[^/]+)/issues/([0-9]+) ]]; then
    repo="${BASH_REMATCH[1]}"
    issue_num="${BASH_REMATCH[2]}"
  elif [[ "$issue_url" =~ ^([^/]+/[^/]+)#([0-9]+)$ ]]; then
    repo="${BASH_REMATCH[1]}"
    issue_num="${BASH_REMATCH[2]}"
  else
    die "approve: <issue-url> must be a GitHub issue URL or owner/repo#N (got: $issue_url)"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "approve: 'gh' CLI is required. Install: https://cli.github.com/  Then: gh auth login"
  fi

  # Fetch the issue body via gh — gives us the airc-knock envelope text.
  local issue_body
  if ! issue_body=$(gh issue view "$issue_num" --repo "$repo" --json body --jq .body 2>&1); then
    die "approve: gh issue view failed for $repo#$issue_num: $issue_body"
  fi

  local knocker_pub
  knocker_pub=$(_airc_approve_extract_knocker_pub "$issue_body")
  if [ -z "$knocker_pub" ]; then
    die "approve: no knocker pubkey found in $repo#$issue_num — is this an airc-knock issue with the Approval crypto block?"
  fi

  # Default invite: the current scope's invite string (the room the
  # approver is hosting / joined to). Operators override with --invite
  # for cross-room invites.
  if [ -z "$invite_string" ]; then
    if ! invite_string=$(_airc_approve_default_invite); then
      die "approve: no --invite given and could not derive one from current scope. Pass --invite \"<string>\" explicitly."
    fi
  fi

  if [ -z "$invite_string" ]; then
    die "approve: invite string is empty (refusing to send empty approval)."
  fi

  # Encrypt the invite to the knocker's pubkey via the python helper.
  # The helper generates a per-approval ephemeral keypair internally,
  # runs ECDH, and emits {ver, approver_pub, nonce, ciphertext} JSON.
  local approval_json
  if ! approval_json=$("$AIRC_PYTHON" -m airc_core.knock_crypto encrypt-for-knocker \
        --knocker-pub "$knocker_pub" \
        --plaintext "$invite_string" 2>&1); then
    die "approve: encryption failed: $approval_json"
  fi

  # Build the comment body. JSON envelope inside a fenced block so
  # cmd_decrypt_approval can parse it; markdown around it explains
  # what humans are looking at.
  #
  # Avoiding heredoc here: the original heredoc-inside-$() form tripped
  # bash's parser when the body contained an apostrophe (knocker's).
  # printf %s with explicit \n is robust + reads cleanly.
  local approver_name
  approver_name=$(_airc_approve_resolve_name)
  local comment_body
  comment_body=$(printf '**airc-approve from `%s`**\n\n%s\n\n```json\n%s\n```\n\n%s\n\n```\nairc decrypt-approval %s --knocker-priv <your-saved-priv-hex>\n```\n' \
    "$approver_name" \
    'The private-room invite is encrypted to the knocker per-knock pubkey (forward-secret per-message ECDH; see [airc#559](https://github.com/CambrianTech/airc/issues/559)). The knocker decrypts with their saved ephemeral private key.' \
    "$approval_json" \
    'To decrypt:' \
    "$issue_url")

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would post approval comment:\n'
    printf '  repo:        %s\n' "$repo"
    printf '  issue:       #%s\n' "$issue_num"
    printf '  knocker_pub: %s...\n' "${knocker_pub:0:24}"
    printf '  invite:      %s\n' "$(_airc_approve_redact_invite "$invite_string")"
    printf '  body preview:\n'
    printf '%s\n' "$comment_body" | sed 's/^/    /'
    return 0
  fi

  local comment_url
  if ! comment_url=$(gh issue comment "$issue_num" --repo "$repo" --body "$comment_body" 2>&1); then
    die "approve: gh issue comment failed: $comment_url"
  fi

  printf 'Approval posted: %s\n' "$comment_url"
  printf 'Knocker decrypts with: airc decrypt-approval %s --knocker-priv <hex>\n' "$issue_url"
}

cmd_decrypt_approval() {
  # Knocker-side. Fetch the approval comment from the issue, decrypt
  # with the knocker's saved ephemeral priv key, print the join string.
  #
  #   airc decrypt-approval <issue-url> --knocker-priv <hex>
  #
  # PR-2 keeps state minimal — the knocker manually saves the priv key
  # output by `airc knock` and passes it back here. PR-2c will manage
  # state automatically in $AIRC_WRITE_DIR/knock-state/.

  local issue_url=""
  local knocker_priv=""

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_decrypt_approval_help
        return 0
        ;;
      --knocker-priv)
        shift
        knocker_priv="${1:-}"
        ;;
      -*)
        die "decrypt-approval: unknown flag: $1"
        ;;
      *)
        if [ -z "$issue_url" ]; then
          issue_url="$1"
        else
          die "decrypt-approval: too many positional args. Got extra: $1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$issue_url" ] || [ -z "$knocker_priv" ]; then
    _airc_decrypt_approval_help >&2
    return 1
  fi

  local repo issue_num
  if [[ "$issue_url" =~ ^https://github\.com/([^/]+/[^/]+)/issues/([0-9]+) ]]; then
    repo="${BASH_REMATCH[1]}"
    issue_num="${BASH_REMATCH[2]}"
  elif [[ "$issue_url" =~ ^([^/]+/[^/]+)#([0-9]+)$ ]]; then
    repo="${BASH_REMATCH[1]}"
    issue_num="${BASH_REMATCH[2]}"
  else
    die "decrypt-approval: <issue-url> must be a GitHub issue URL or owner/repo#N"
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "decrypt-approval: 'gh' CLI is required."
  fi

  # Fetch comments. The approval is the first (or only) comment with an
  # airc-approve envelope. If multiple approvals are posted, decrypt the
  # most recent one — operators can override later via --comment-index
  # if needed (PR-2c).
  local comments_json
  if ! comments_json=$(gh issue view "$issue_num" --repo "$repo" --json comments 2>&1); then
    die "decrypt-approval: gh issue view failed: $comments_json"
  fi

  local approval_json
  approval_json=$(_airc_decrypt_extract_approval "$comments_json")
  if [ -z "$approval_json" ]; then
    die "decrypt-approval: no airc-approve envelope found in $repo#$issue_num comments. Approval may not have been posted yet."
  fi

  local approver_pub nonce ciphertext
  approver_pub=$("$AIRC_PYTHON" -c 'import json,sys; print(json.loads(sys.stdin.read()).get("approver_pub",""))' <<< "$approval_json")
  nonce=$("$AIRC_PYTHON" -c 'import json,sys; print(json.loads(sys.stdin.read()).get("nonce",""))' <<< "$approval_json")
  ciphertext=$("$AIRC_PYTHON" -c 'import json,sys; print(json.loads(sys.stdin.read()).get("ciphertext",""))' <<< "$approval_json")

  if [ -z "$approver_pub" ] || [ -z "$nonce" ] || [ -z "$ciphertext" ]; then
    die "decrypt-approval: malformed approval envelope (missing approver_pub/nonce/ciphertext)"
  fi

  if ! "$AIRC_PYTHON" -m airc_core.knock_crypto decrypt-from-approver \
        --knocker-priv "$knocker_priv" \
        --approver-pub "$approver_pub" \
        --nonce "$nonce" \
        --ciphertext "$ciphertext"; then
    die "decrypt-approval: decryption failed (see error above)"
  fi
}

_airc_approve_help() {
  cat <<'EOF'
airc approve — encrypt a private-room invite to a knocker (airc#559 PR-2)

USAGE
  airc approve <issue-url> [--invite "<string>"] [--dry-run]
  airc approve owner/repo#N  [--invite "<string>"] [--dry-run]

DESCRIPTION
  Reads the knocker's per-knock ephemeral X25519 pubkey from the
  airc-knock issue body. Generates a per-approval ephemeral keypair,
  derives a forward-secret shared key via ECDH+HKDF, AEAD-encrypts the
  invite, and posts the {approver_pub, nonce, ciphertext} envelope as
  a comment on the issue.

OPTIONS
  --invite "<string>"   The private-room join string. If omitted, defaults
                        to the current scope's invite (the room you're
                        hosting / joined to).
  --dry-run             Print what would be posted; don't comment on gh.
  -h, --help            This help.

NOTES
  - Both ephemerals are per-message — long-term key compromise YEARS
    later cannot recover any prior approval's join string.
  - Public GitHub comments are durable; the join string MUST stay valid
    only briefly. Per airc#559, room-rotation hooks (PR-3) make leaked
    old ciphertext unable to reopen access later.
  - 'gh' CLI must be authenticated. Run 'gh auth login' first.
EOF
}

_airc_decrypt_approval_help() {
  cat <<'EOF'
airc decrypt-approval — decrypt an approval comment (airc#559 PR-2)

USAGE
  airc decrypt-approval <issue-url> --knocker-priv <hex>
  airc decrypt-approval owner/repo#N --knocker-priv <hex>

DESCRIPTION
  Fetches the airc-approve comment from the knock issue, derives the
  shared ECDH key with the knocker's saved ephemeral private key, and
  prints the decrypted invite string to stdout.

OPTIONS
  --knocker-priv <hex>   Per-knock ephemeral X25519 priv key (hex).
                         You saved this when running `airc knock`.
  -h, --help             This help.

NOTES
  - PR-2 minimal: knocker manually passes the priv key from `airc knock`
    output. PR-2c will manage state in $AIRC_WRITE_DIR/knock-state/.
  - 'gh' CLI must be authenticated.
EOF
}

_airc_approve_extract_knocker_pub() {
  # Parse the JSON block under "### Approval crypto" from the issue body.
  # Body comes in via $1 as raw markdown; we look for the first JSON
  # block with a "knocker_pub" field.
  local body="$1"
  printf '%s' "$body" | "$AIRC_PYTHON" - <<'PYEOF' 2>/dev/null
import json, re, sys
body = sys.stdin.read()
# Match ```json ... ``` blocks; check each for knocker_pub.
for match in re.finditer(r'```json\s*\n(.*?)\n```', body, re.DOTALL):
    blob = match.group(1).strip()
    try:
        parsed = json.loads(blob)
    except Exception:
        continue
    if isinstance(parsed, dict) and "knocker_pub" in parsed:
        pub = parsed.get("knocker_pub", "")
        if pub:
            print(pub)
            sys.exit(0)
sys.exit(1)
PYEOF
}

_airc_decrypt_extract_approval() {
  # Find the airc-approve envelope in the issue's comments JSON.
  # Returns the most recent envelope (last comment with one), so a
  # repost or correction wins over the original.
  local comments_json="$1"
  printf '%s' "$comments_json" | "$AIRC_PYTHON" - <<'PYEOF' 2>/dev/null
import json, re, sys
data = json.loads(sys.stdin.read())
comments = data.get("comments", [])
found = None
for comment in comments:
    body = comment.get("body", "")
    for match in re.finditer(r'```json\s*\n(.*?)\n```', body, re.DOTALL):
        try:
            parsed = json.loads(match.group(1).strip())
        except Exception:
            continue
        if (isinstance(parsed, dict)
                and parsed.get("ver") == "v1"
                and "approver_pub" in parsed
                and "nonce" in parsed
                and "ciphertext" in parsed):
            found = match.group(1).strip()  # keep last; iteration is chronological
if found:
    print(found)
PYEOF
}

_airc_approve_default_invite() {
  # Try to derive the current scope's invite string. This is best-effort —
  # `airc invite` exists for the current scope but the approver may want
  # to invite to a different room. Operator-override via --invite.
  if command -v airc >/dev/null 2>&1; then
    airc invite 2>/dev/null | tail -1 | tr -d '\n' && return 0
  fi
  return 1
}

_airc_approve_resolve_name() {
  # Best-effort approver name. Falls back to "approver" if no scope.
  if declare -F resolve_name >/dev/null 2>&1; then
    resolve_name
  else
    echo "approver"
  fi
}

_airc_approve_redact_invite() {
  # Show only the first 24 chars of the invite for dry-run output.
  # Avoid leaking the full credential into terminal scrollback.
  local invite="$1"
  if [ "${#invite}" -le 24 ]; then
    printf '%s' "$invite"
  else
    printf '%s...' "${invite:0:24}"
  fi
}
