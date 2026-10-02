#!/usr/bin/env bash
# Downloaded-entry/older-source regression, with local Git acquisition fixtures.
set -euo pipefail
case "${1:-all}" in
  all) adapter_only=0 ;;
  --posix-adapter)
    case "$(uname -s)" in Linux|Darwin) ;; *) echo 'POSIX source adapter requires Linux or macOS' >&2; exit 2 ;; esac
    adapter_only=1 ;;
  *) echo 'Usage: setup-source-acquisition.sh [--posix-adapter]' >&2; exit 2 ;;
esac
repo="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/entry" "$fixture/home/.airc/src" "$fixture/tools"
cp "$repo/install.sh" "$fixture/entry/install.sh"
printf 'old source preserved\n' > "$fixture/home/.airc/src/Cargo.toml"
cat > "$fixture/tools/git" <<'GIT'
#!/usr/bin/env bash
set -eu
printf '%s\n' "$*" >> "$AIRC_FIXTURE_CALLS"
[ "$1" = clone ] || exit 92
destination="${!#}"
mkdir -p "$destination/setup" "$destination/.git"
printf 'new fixture source\n' > "$destination/Cargo.toml"
printf '#!/usr/bin/env bash\necho compatible-auth-stage\n' > "$destination/setup/github-auth.sh"
if [ "${AIRC_FIXTURE_WINDOWS:-0}" = 1 ]; then
  mkdir -p "$destination/windows"
  for relative in install-prereqs.ps1 register-bin-path.ps1 configure-firewall.ps1 shared-setup.ps1 setup-artifacts.lock.json install-session.ps1 adopt-installed.ps1 sync-bootstrap.ps1 setup-entrypoint.ps1; do
    printf 'fixture\n' > "$destination/windows/$relative"
  done
  printf '#!/usr/bin/env bash\necho compatible-windows-session\n' > "$destination/windows/run-powershell.sh"
fi
GIT
if [ "$adapter_only" = 0 ]; then
  printf '#!/bin/sh\necho Linux\n' > "$fixture/tools/uname"
  chmod +x "$fixture/tools/uname"
fi
chmod +x "$fixture/tools/git"
export HOME="$fixture/home" PATH="$fixture/tools:$PATH" AIRC_FIXTURE_CALLS="$fixture/calls"
export BIN_DIR="$fixture/bin" SKILLS_TARGET="$fixture/skills"
export AIRC_SKIP_PREREQS=1 AIRC_SKIP_AUTH=1 AIRC_SKIP_RUST_BUILD=1 AIRC_INSTALL_NO_PULL=1
export AIRC_SKIP_CODEX_CONFIG=1 AIRC_SKIP_CODEX_INSTRUCTIONS=1 AIRC_SKIP_CODEX_HOOKS=1
export AIRC_SKIP_CODEX_TOKEN=1 AIRC_SKIP_CODEX_RULES=1 AIRC_SKIP_GIT_HOOKS=1
unset AIRC_DIR AIRC_CHANNEL
bash "$fixture/entry/install.sh" > "$fixture/output" 2>&1 || { cat "$fixture/output"; exit 1; }
grep -q 'clone --quiet --branch canary' "$fixture/calls"
grep -q 'setup-source-' "$fixture/calls"
grep -q 'compatible-auth-stage' "$fixture/output"
grep -q 'old source preserved' "$HOME/.airc/src/Cargo.toml"
: > "$fixture/calls"
if AIRC_DIR="$HOME/.airc/src" bash "$fixture/entry/install.sh" > "$fixture/output" 2>&1; then
  echo 'FAIL: incompatible explicit source was accepted'; exit 1
fi
grep -q 'developer work was preserved' "$fixture/output"
[ ! -s "$fixture/calls" ]

# The native macOS shell has exercised acquisition and developer preservation.
# Simulated Windows layout policy stays in the full scenario. Git Bash retains
# that direct-entry proof separately from windows-setup-bridge.ps1's native entry.
if [ "$adapter_only" = 1 ]; then
  echo 'PASS: native POSIX source acquisition and developer preservation'
  exit 0
fi

# The immediately preceding Windows release has auth but no session adapter.
# Its existing managed fallback must also survive while setup acquires a new one.
mkdir -p "$HOME/.airc/src/setup"
printf 'old auth\n' > "$HOME/.airc/src/setup/github-auth.sh"
old_fallback="$(find "$HOME/.airc" -maxdepth 1 -name 'setup-source-*' -type d | head -1)"
printf 'local work\n' > "$old_fallback/local-work.txt"
printf '#!/bin/sh\necho MINGW64_NT-10.0\n' > "$fixture/tools/uname"
# Only the platform/source-selection unit fixture simulates this boundary.
printf '#!/bin/sh\nexit 0\n' > "$fixture/tools/powershell.exe"
chmod +x "$fixture/tools/powershell.exe"
export AIRC_FIXTURE_WINDOWS=1
unset CAMBRIAN_INSTALL_ELEVATION
bash "$fixture/entry/install.sh" > "$fixture/output" 2>&1 || { cat "$fixture/output"; exit 1; }
grep -q 'setup-source-.*-1' "$fixture/calls"
grep -q 'compatible-windows-session' "$fixture/output"
grep -q 'local work' "$old_fallback/local-work.txt"
grep -q 'old auth' "$HOME/.airc/src/setup/github-auth.sh"
: > "$fixture/calls"
if AIRC_DIR="$HOME/.airc/src" bash "$fixture/entry/install.sh" > "$fixture/output" 2>&1; then
  echo 'FAIL: preceding Windows release was accepted as a compatible developer source'; exit 1
fi
[ ! -s "$fixture/calls" ]
echo 'PASS: downloaded entry acquires compatible source and preserves older/developer checkouts'
