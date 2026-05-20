# Sourced by airc. cmd_knock — public collaboration knock for a project repo.
#
# Function exported back to airc's dispatch:
#   cmd_knock — open a GitHub issue on a target project's repo with a
#               structured "airc-knock" envelope so the project's owners
#               can see the request, moderate it (GitHub's native labels +
#               close + spam tools), and approve (Phase 2) to receive the
#               private room invite.
#
# Why GitHub issues (not a new gist namespace) for PR-1:
#   1. Existing GitHub moderation tools (block users, mark spam, close
#      issues) work out of the box. No new abuse surface.
#   2. Repo owners ALREADY watch their issues; no new tool to learn.
#   3. Knock body is a structured envelope so automated tooling (a future
#      `airc approve` flow + the kanban work in #559) can read it.
#   4. `gh` is already a hard dependency for the rest of airc.
#
# What this PR does NOT do (later PR-3 under airc#559):
#   - Private-room rotation when a peer becomes abusive
#   - Shared sprint/kanban queue primitives
#   - Repo-local `.airc/` discovery manifest (continuum#1109 pilots that)
#
# External cross-references (resolved at call time):
#   die, ensure_init, resolve_name, get_config_val, AIRC_HOME, AIRC_PYTHON,
#   _airc_lib_dir; `gh` CLI; `airc_core.identity`.

cmd_knock() {
  # Public-facing entrypoint. Args:
  #   airc knock <owner/repo> <message>           Open a knock issue on owner/repo.
  #   airc knock <owner/repo> --message "..."     Same, explicit flag form.
  #   airc knock <owner/repo>                     Prompts for the message on stdin (rare; tests pass explicitly).
  #
  # The target MUST be a full `owner/repo` slug. Resolving from a bare
  # project name would require a discovery substrate (continuum#1109's
  # `.airc/` manifest pilot lays the ground for that — once it ships,
  # cmd_knock will accept bare names that resolve via the manifest).
  # For now: explicit, no surprise.
  #
  # No ensure_init here: knock is intentionally usable BEFORE the knocker
  # has a paired airc scope. The whole point is "outsider asks to join."
  # `resolve_name` falls back to derive_name → hostname when no scope
  # exists, so identity is still well-defined.

  local target_repo=""
  local message=""
  local dry_run=0

  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)
        _airc_knock_help
        return 0
        ;;
      --message|-m)
        shift
        message="${1:-}"
        ;;
      --dry-run)
        dry_run=1
        ;;
      --)
        shift
        # Remaining args are message words.
        message="${message:+$message }$*"
        break
        ;;
      -*)
        die "knock: unknown flag: $1"
        ;;
      *)
        if [ -z "$target_repo" ]; then
          target_repo="$1"
        else
          # Subsequent positional args are message words. Append with
          # space separation so `airc knock owner/repo hello there` works.
          message="${message:+$message }$1"
        fi
        ;;
    esac
    shift || true
  done

  if [ -z "$target_repo" ]; then
    _airc_knock_help >&2
    return 1
  fi

  case "$target_repo" in
    */*) : ;;
    *)
      die "knock: target must be owner/repo (e.g. CambrianTech/continuum), got: $target_repo"
      ;;
  esac

  if [ -z "$message" ]; then
    die "knock: message is required. Usage: airc knock $target_repo \"<message>\""
  fi

  # Length cap: GitHub issue titles cap at 256 chars; we encode the user
  # message into the title prefix so spam moderation is title-readable.
  # Cap our slice well below the limit to leave room for the "airc-knock:"
  # prefix + ellipsis. 180 chars is plenty for a single sentence.
  local title_slice="$message"
  if [ "${#title_slice}" -gt 180 ]; then
    title_slice="${title_slice:0:177}..."
  fi

  local knocker_name
  knocker_name=$(resolve_name)
  local knocker_identity_json
  knocker_identity_json=$(_airc_knock_identity_json "$knocker_name")

  # Generate a per-knock ephemeral X25519 keypair for the approve flow
  # (airc#559 PR-2). The approver derives a forward-secret shared key
  # via ECDH(approver_ephemeral, knocker_ephemeral) and posts the
  # encrypted private-room invite as a comment. Both ephemerals are
  # per-message — even if either party's long-term key leaks years
  # later, every prior approval's join string is unrecoverable.
  #
  # The PRIVATE half is printed at the end of the success message so the
  # operator can save it. PR-2c will manage state automatically; for now
  # state surface stays minimal.
  local knock_keys_json knocker_pub knocker_priv
  knock_keys_json=$(_airc_knock_gen_keys 2>/dev/null || echo "")
  if [ -n "$knock_keys_json" ]; then
    knocker_pub=$("$(airc_rs_bin)" knock approval-field --field pub <<< "$knock_keys_json" 2>/dev/null || echo "")
    knocker_priv=$("$(airc_rs_bin)" knock approval-field --field priv <<< "$knock_keys_json" 2>/dev/null || echo "")
  else
    # Crypto path unavailable. Knock
    # still posts but without an approval pubkey — cmd_approve will
    # hard-fail with a clear "no knocker pubkey, can't encrypt" error
    # rather than silently shipping plaintext.
    knocker_pub=""
    knocker_priv=""
  fi

  local issue_title="airc-knock: $title_slice"
  local issue_body
  issue_body=$(_airc_knock_issue_body "$knocker_name" "$knocker_identity_json" "$message" "$knocker_pub")

  if [ "$dry_run" -eq 1 ]; then
    printf 'DRY RUN — would post knock issue:\n'
    printf '  repo:   %s\n' "$target_repo"
    printf '  title:  %s\n' "$issue_title"
    printf '  body:\n%s\n' "$issue_body" | sed 's/^/    /'
    if [ -n "$knocker_priv" ]; then
      printf '\n  knocker priv key (SAVE THIS — needed to decrypt approval):\n    %s\n' "$knocker_priv"
    fi
    return 0
  fi

  if ! command -v gh >/dev/null 2>&1; then
    die "knock: 'gh' CLI is required. Install: https://cli.github.com/  Then: gh auth login"
  fi

  # Use gh to open the issue with the airc-knock label. Repo owners
  # filter on label=airc-knock to watch incoming knocks. The label is
  # created on first use; if `gh issue create` fails because the label
  # doesn't exist, fall back to no-label (still a valid issue, just
  # less filterable).
  local issue_url
  # Body goes via --body-file (lib_gh.sh) so embedded backticks / fenced
  # code blocks / $-vars in the knock message can't trigger shell
  # command substitution and can't blow argv length limits (airc#571).
  if issue_url=$(_airc_gh_safe_body "$issue_body" issue create \
    --repo "$target_repo" \
    --title "$issue_title" \
    --label "airc-knock"); then
    :
  elif issue_url=$(_airc_gh_safe_body "$issue_body" issue create \
    --repo "$target_repo" \
    --title "$issue_title"); then
    # Owner repo doesn't have the airc-knock label yet — issue still
    # posts. Surface a note so the operator knows to add the label.
    printf 'note: %s does not have an "airc-knock" label yet. The issue posted without one — repo owner may want to add the label for filtering.\n' "$target_repo" >&2
  else
    die "knock: gh issue create failed: $issue_url"
  fi

  printf 'Knock sent: %s\n' "$issue_url"
  if [ -n "$knocker_priv" ]; then
    printf '\nSAVE THIS PRIVATE KEY — needed to decrypt the approval comment:\n  %s\n\n' "$knocker_priv"
    printf 'When approved, run:\n  airc decrypt-approval %s --knocker-priv <the key above>\n' "$issue_url"
  else
    printf 'WARNING: no crypto pubkey embedded — cmd_approve cannot encrypt for this knock.\n' >&2
    printf '         Approval will require an out-of-band channel (DM, email).\n' >&2
  fi
}

_airc_knock_help() {
  cat <<'EOF'
airc knock — request collaboration access to a project repo

USAGE
  airc knock <owner/repo> <message>
  airc knock <owner/repo> --message "..."
  airc knock <owner/repo> --dry-run [--message "..."]

DESCRIPTION
  Opens a GitHub issue on the target repo with title "airc-knock: <msg>"
  and a structured envelope body containing your airc identity (name,
  role, bio) + the message. Repo owners use GitHub's native moderation
  tools (labels, close, spam, block) and `airc approve` to send approved
  peers the private room invite.

OPTIONS
  -m, --message <text>   Provide the message via flag (vs trailing args).
      --dry-run          Print the envelope that WOULD be posted; don't post.
  -h, --help             This help.

EXAMPLES
  airc knock CambrianTech/continuum "I want to help with Carl install"
  airc knock CambrianTech/airc --message "Working on a Vulkan ICD probe"
  airc knock CambrianTech/continuum --dry-run -m "test envelope"

NOTES
  - 'gh' CLI must be authenticated. Run 'gh auth login' first.
  - The 'airc-knock' label is auto-applied if it exists on the target
    repo; otherwise the issue posts without a label and a hint suggests
    creating it.
  - Approval handoff uses per-knock crypto. Private-room rotation and
    sprint/kanban queue primitives come in later airc#559 slices.
EOF
}

_airc_knock_identity_json() {
  # Read identity fields if airc_core.identity is reachable. Falls back
  # to a minimal name-only envelope if it isn't — knock should work
  # even on a fresh install where identity wasn't populated yet.
  local name="$1"
  if [ -n "${AIRC_PYTHON:-}" ] && [ -d "${_airc_lib_dir:-}" ]; then
    "$AIRC_PYTHON" - "$name" "$AIRC_WRITE_DIR" <<'PYEOF' 2>/dev/null || _airc_knock_identity_fallback "$name"
import json, os, sys
name = sys.argv[1]
home = sys.argv[2]
pronouns = ""
role = ""
bio = ""
gh_login = ""
identity_path = os.path.join(home, "identity.json")
if os.path.exists(identity_path):
    try:
        with open(identity_path) as f:
            data = json.load(f) or {}
        pronouns = data.get("pronouns", "")
        role = data.get("role", "")
        bio = data.get("bio", "")
        gh_login = data.get("gh_login", "")
    except Exception:
        pass
# Fall back to gh CLI if gh_login isn't in identity.json.
if not gh_login:
    try:
        import subprocess
        gh_login = subprocess.run(
            ["gh", "api", "user", "--jq", ".login"],
            capture_output=True, text=True, timeout=5
        ).stdout.strip() or ""
    except Exception:
        gh_login = ""
print(json.dumps({
    "name": name,
    "pronouns": pronouns,
    "role": role,
    "bio": bio,
    "gh_login": gh_login,
}))
PYEOF
  else
    _airc_knock_identity_fallback "$name"
  fi
}

_airc_knock_identity_fallback() {
  local name="$1"
  printf '{"name":%s,"pronouns":"","role":"","bio":"","gh_login":""}' \
    "$(_airc_knock_json_str "$name")"
}

_airc_knock_json_str() {
  # Minimal JSON string escaper for the bash fallback path. Real callers
  # go through the python heredoc which handles edge cases properly.
  # Escapes backslash, double-quote, newline, tab — sufficient for typical
  # name/role/bio content. Anything weirder uses the python path.
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  s="${s//$'\n'/\\n}"
  s="${s//$'\t'/\\t}"
  printf '"%s"' "$s"
}

_airc_knock_gen_keys() {
  # Generate per-knock ephemeral X25519 keypair via the Rust CLI.
  # Returns JSON {"priv": "<hex>", "pub": "<hex>"} on stdout.
  # Returns non-zero (caller falls back to no-pubkey envelope) when the
  # Rust binary isn't available.
  "$(airc_rs_bin)" knock gen-keys
}

_airc_knock_issue_body() {
  local knocker_name="$1"
  local identity_json="$2"
  local message="$3"
  local knocker_pub="${4:-}"

  # The body is human-readable markdown PLUS machine-readable JSON
  # blocks. Future tooling (`airc approve`, sprint/kanban) parses the
  # JSON; humans read the markdown.
  #
  # The Approval crypto block (PR-2) carries the per-knock ephemeral
  # X25519 pubkey. cmd_approve parses this, generates its own ephemeral,
  # ECDH-derives a shared key, and posts an encrypted comment. Both
  # ephemerals are per-message — even a long-term-key leak years later
  # cannot recover any prior approval's join string.
  cat <<EOF
**airc knock from \`$knocker_name\`**

$message

---

### Identity

\`\`\`json
$identity_json
\`\`\`

### Approval crypto

\`\`\`json
{"ver":"v1","knocker_pub":"$knocker_pub"}
\`\`\`

### Next step

Repo owners can:
- Approve by running \`airc approve <this issue URL>\` (Phase 2 of [airc#559](https://github.com/CambrianTech/airc/issues/559)). The approval encrypts the private-room invite to the \`knocker_pub\` above and posts it as a comment on this issue.
- Reject by closing this issue, optionally with a comment.
- Mark as spam via GitHub's spam tools.

This issue was opened by \`airc knock\`. The structured JSON envelopes above are parsed by future approval tooling.
EOF
}
