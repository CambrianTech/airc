#!/usr/bin/env bash
# Exercise the public shared stage with an isolated HOME and fake GitHub.
# No credentials/network, keyring mutation, or package installs in these tests.
set -euo pipefail
case "${1:-all}" in
  all) adapter_only=0 ;;
  --native-adapter)
    case "$(uname -s)" in Darwin|MINGW*|MSYS*|CYGWIN*) ;; *) echo 'Native auth adapter requires macOS or Git Bash' >&2; exit 2 ;; esac
    adapter_only=1 ;;
  *) echo 'Usage: setup-github-auth.sh [--native-adapter]' >&2; exit 2 ;;
esac
repo="$(cd "$(dirname "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin" "$tmp/home"
export HOME="$tmp/home" TEST_STATE="$tmp" PATH="$tmp/bin:$PATH"
unset AIRC_SKIP_AUTH AIRC_GITHUB_USER GH_TOKEN GITHUB_TOKEN
cat > "$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$TEST_STATE/calls"
case "$1 $2" in
  'auth status') test -f "$TEST_STATE/approved" ;;
  'auth login'|'auth refresh')
    printf 'DEVICE-CODE-FIXTURE: waiting for browser approval\n' >&2
    [ ! -f "$TEST_STATE/reject" ] || exit 1
    if [ -f "$TEST_STATE/wait" ]; then
      printf 'ready' > "$TEST_STATE/ready"
      while [ ! -f "$TEST_STATE/consent" ]; do sleep 0.1; done
    fi
    touch "$TEST_STATE/approved" "$TEST_STATE/gist"
    ;;
  'auth setup-git') test -f "$TEST_STATE/approved" ;;
  'api --hostname')
    if [[ "$*" == *--include* ]]; then
      if [ -f "$TEST_STATE/gist" ]; then printf 'X-Oauth-Scopes: repo, gist\r\n'
      else printf 'X-Oauth-Scopes: repo\r\n'; fi
    elif [[ "$*" == *'.name // .login'* ]]; then printf 'Test User\n'
    elif [[ "$*" == *'.id'* ]]; then printf '123\n'
    else printf 'test-account\n'; fi
    ;;
  *) exit 91 ;;
esac
GH
cat > "$tmp/bin/git" <<'GIT'
#!/usr/bin/env bash
set -eu
printf 'git %s\n' "$*" >> "$TEST_STATE/calls"
if [ "$#" = 3 ]; then
  cat "$TEST_STATE/$3" 2>/dev/null || exit 1
else
  printf '%s' "$4" > "$TEST_STATE/$3"
fi
GIT
# Generic policy can run as root; native adapter coverage keeps the real OS.
if [ "$adapter_only" = 0 ]; then
  printf '#!/usr/bin/env bash\nprintf "MINGW64_NT-fixture\\n"\n' > "$tmp/bin/uname"
  chmod +x "$tmp/bin/uname"
fi
chmod +x "$tmp/bin/gh" "$tmp/bin/git"
stage="$repo/setup/github-auth.sh"

# Fresh login must expose the code and await the actual approval operation.
touch "$tmp/wait"
bash "$stage" > "$tmp/output" 2>&1 &
stage_pid=$!
for ((i=0; i<100; i++)); do
  [ ! -f "$tmp/ready" ] || break
  kill -0 "$stage_pid" || { cat "$tmp/output"; exit 1; }
  sleep 0.1
done
test -f "$tmp/ready"
# Deliberately longer than the rushed five-second login window from the report.
[ "$adapter_only" = 1 ] || sleep 6
kill -0 "$stage_pid"
! grep -q 'auth setup-git' "$tmp/calls"
grep -q 'DEVICE-CODE-FIXTURE' "$tmp/output"
touch "$tmp/consent"
wait "$stage_pid"
rm "$tmp/wait"
grep -q 'DEVICE-CODE-FIXTURE' "$tmp/output"
grep -q 'auth login .*--web --scopes gist' "$tmp/calls"
grep -q 'GitHub account verified: test-account' "$tmp/output"
grep -q '123+test-account@users.noreply.github.com' "$tmp/user.email"

# Rerun reuses approval and preserves existing identity.
: > "$tmp/calls"
printf 'Keep My Name' > "$tmp/user.name"
bash "$stage" > "$tmp/output" 2>&1
! grep -qE 'auth (login|refresh)' "$tmp/calls"
test "$(cat "$tmp/user.name")" = 'Keep My Name'

# Linux owns the full policy matrix below. The native adapter has exercised
# fresh consent output/waiting, CRLF scope parsing, identity and approval reuse.
if [ "$adapter_only" = 1 ]; then
  echo 'PASS: native shell auth consent and approval reuse'
  exit 0
fi

# Insufficient scopes use the same device approval, not replacement credentials.
rm "$tmp/gist"
: > "$tmp/calls"
bash "$stage" > "$tmp/output" 2>&1
grep -q 'auth refresh .*--scopes gist' "$tmp/calls"
! grep -q 'auth login' "$tmp/calls"

# Account mismatch never proceeds to git configuration or join.
: > "$tmp/calls"
if AIRC_GITHUB_USER=someone-else bash "$stage" > "$tmp/output" 2>&1; then exit 1; fi
grep -q 'expected someone-else' "$tmp/output"
! grep -q 'auth setup-git' "$tmp/calls"

# Denied/expired authorization is failure, never installation success.
rm "$tmp/approved"
touch "$tmp/reject"
: > "$tmp/calls"
if bash "$stage" > "$tmp/output" 2>&1; then exit 1; fi
grep -q 'Rerun the same AIRC setup' "$tmp/output"
! grep -q 'auth setup-git' "$tmp/calls"

printf 'PASS: fresh consent, approval reuse, missing scope, wrong account, cancelled approval\n'
