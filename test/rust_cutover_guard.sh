#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail() {
  echo "rust cutover guard failed: $*" >&2
  exit 1
}

if [ -d lib/airc_core ]; then
  fail "lib/airc_core still exists"
fi

tracked_py="$(git ls-files '*.py')"
if [ -n "$tracked_py" ]; then
  printf '%s\n' "$tracked_py" >&2
  fail "tracked Python files remain"
fi

for path in install.sh uninstall.sh airc lib/airc_bash skills integrations README.md .github test/integration.sh; do
  [ -e "$path" ] || continue
  if git grep -nE 'AIRC_PYTHON|PYTHONPATH|PYTHONIOENCODING|python3|airc_core\.' -- "$path"; then
    fail "live path still references Python runtime: $path"
  fi
done

if git grep -nE 'airc-rs|airc_rs|AIRC_RS' -- . ':!test/rust_cutover_guard.sh'; then
  fail "old Rust-language command suffix remains"
fi

if git grep -nE 'airc-src|[.]airc-src' -- . ':!test/rust_cutover_guard.sh'; then
  fail "old split install clone path remains"
fi

if git grep -nE 'ln -s[f]?[[:space:]].*BIN_DIR.*/airc|ln -s[f]?[[:space:]].*CLONE_DIR.*/airc' -- install.sh; then
  fail "install.sh must install the public airc command as a forwarder file, not a symlink"
fi

if git grep -nE 'BIN_DIR.*/relay|for f in airc relay|[,{]airc,relay|relay-[*]' -- install.sh uninstall.sh skills; then
  fail "relay command alias install/uninstall surface remains"
fi
