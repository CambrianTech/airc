#!/usr/bin/env bash
# Run the actual shared installer through its native environment-return boundary.
# The package adapter is a fixture; no actual packages or user state are changed.
set -euo pipefail
case "$(uname -s)" in MINGW*|MSYS*) ;; *) echo 'SKIP: Windows process boundary'; exit 0 ;; esac
repo="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
source_dir="$fixture/source with spaces"
mkdir -p "$source_dir/windows" "$source_dir/setup" "$source_dir/.git" "$fixture/home" "$fixture/wsl-first"
cp "$repo/install.sh" "$source_dir/install.sh"
cp "$repo/windows/run-powershell.sh" "$source_dir/windows/run-powershell.sh"
cp "$repo/setup/github-auth.sh" "$source_dir/setup/github-auth.sh"
printf 'fixture\n' > "$source_dir/Cargo.toml"
cat > "$fixture/wsl-first/bash" <<'BAD'
#!/bin/sh
echo 'FAIL: imported Windows PATH selected the wrong Bash runtime' >&2
exit 97
BAD
chmod +x "$fixture/wsl-first/bash"
cat > "$source_dir/windows/install-prereqs.ps1" <<'PS'
param([string]$SourceDirectory,[string]$EnvironmentFile)
$value = $env:AIRC_FIXTURE_BAD_BIN + ';' + $env:PATH
[IO.File]::WriteAllText($EnvironmentFile, ('PATH' + [char]0 + $value + [char]0), (New-Object Text.UTF8Encoding($false)))
PS
export AIRC_FIXTURE_BAD_BIN="$(cygpath -w "$fixture/wsl-first")"
export HOME="$fixture/home" AIRC_DIR="$source_dir" BIN_DIR="$fixture/bin"
export SKILLS_TARGET="$fixture/skills" AIRC_INSTALL_NO_PULL=1 AIRC_SKIP_AUTH=1 AIRC_SKIP_RUST_BUILD=1
export AIRC_SKIP_CODEX_CONFIG=1 AIRC_SKIP_CODEX_INSTRUCTIONS=1 AIRC_SKIP_CODEX_HOOKS=1
export AIRC_SKIP_CODEX_TOKEN=1 AIRC_SKIP_CODEX_RULES=1 AIRC_SKIP_GIT_HOOKS=1
unset AIRC_SKIP_PREREQS
"$BASH" "$source_dir/install.sh" > "$fixture/output" 2>&1 || { cat "$fixture/output"; exit 1; }
grep -q 'GitHub authorization explicitly skipped' "$fixture/output"
grep -q 'Installed.' "$fixture/output"
! grep -q 'wrong Bash runtime' "$fixture/output"
# Negative control: the fixture must catch the exact pre-fix regression.
content="$(cat "$source_dir/install.sh")"
printf '%s\n' "${content/'$(dirname "$BASH"):'/}" > "$source_dir/install.sh"
if "$BASH" "$source_dir/install.sh" > "$fixture/broken-output" 2>&1; then
  echo 'FAIL: regression control unexpectedly succeeded' >&2
  exit 1
fi
grep -q 'wrong Bash runtime' "$fixture/broken-output"
echo 'PASS: native PATH refresh preserves Git Bash through the actual shared install entry'
