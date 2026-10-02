#!/usr/bin/env bash
# What this catches: the macOS/Linux login supervisor must run `airc join` in
# supervisor mode from the account home (never the setup cwd's project scope),
# restart it when it exits, and survive paths that need escaping. Renderers only;
# no launchctl/systemctl calls.
set -Eeuo pipefail
trap 'printf "FAIL: unix autostart fixture line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2' ERR
repo="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=../unix/register-autostart.sh
source "$repo/unix/register-autostart.sh"
fail() { echo "FAIL: $*" >&2; exit 1; }

home='/Users/a & b'
plist="$(autostart_plist '/opt/airc <x>/airc' "$home" '/usr/bin:/bin')"
grep -q '<string>/opt/airc &lt;x&gt;/airc</string><string>join</string>' <<<"$plist" || fail "plist does not run the escaped binary with join"
grep -q '<key>AIRC_SUPERVISOR</key><string>1</string>' <<<"$plist" || fail "plist does not mark the supervisor runtime"
grep -q '<key>WorkingDirectory</key><string>/Users/a &amp; b</string>' <<<"$plist" || fail "plist does not run from the account home"
grep -q '<key>KeepAlive</key><true/>' <<<"$plist" || fail "plist does not restart join when it exits"
grep -q '<key>RunAtLoad</key><true/>' <<<"$plist" || fail "plist does not start at login"
if command -v plutil >/dev/null 2>&1; then
  plutil -lint - <<<"$plist" >/dev/null || fail "plist does not lint"
fi

unit="$(autostart_unit '/home/u/.local/bin/airc' '/usr/bin:/bin')"
grep -qx 'ExecStart="/home/u/.local/bin/airc" join' <<<"$unit" || fail "unit does not run join"
grep -qx 'Environment=AIRC_SUPERVISOR=1' <<<"$unit" || fail "unit does not mark the supervisor runtime"
grep -qx 'WorkingDirectory=%h' <<<"$unit" || fail "unit does not run from the account home"
grep -qx 'Restart=always' <<<"$unit" || fail "unit does not restart join when it exits"
grep -qx 'WantedBy=default.target' <<<"$unit" || fail "unit is not started with the user session"

echo "✓ unix autostart renders a home-scoped, self-restarting airc join supervisor"
