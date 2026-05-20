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
