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

for path in install.sh uninstall.sh skills integrations README.md .github; do
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

# Post-demolition contract (PR D): the public `airc` is the Rust
# binary at $BIN_DIR/airc, installed by install.sh from
# target/release/airc. No bash wrapper, no .shim/.cmd/.ps1
# trampolines, no airc-core suffix on the binary. Guards below
# enforce that the install.sh shape stays consistent with that.

if [ -e airc ] || [ -e airc.shim ] || [ -e airc.cmd ] || [ -e airc.ps1 ]; then
  fail "legacy bash wrapper/trampoline files must not exist in the repo root"
fi

if [ -e lib/airc_bash ]; then
  fail "legacy bash command library lib/airc_bash must not exist"
fi

if ! git grep -q 'Installed airc: [$]BIN_DIR/airc' -- install.sh; then
  fail "install.sh must install the Rust binary at \$BIN_DIR/airc (or .exe on Windows)"
fi

if ! git grep -q '_default_clone_dir' -- install.sh; then
  fail "install.sh must resolve a checked-out source tree before defaulting to ~/.airc/src"
fi

if git grep -nE 'Installed command shim:|cat > "[$]BIN_DIR/airc" <<' -- install.sh; then
  fail "install.sh must not install a bash wrapper shim — the Rust binary is the user surface"
fi

if git grep -nE 'BIN_DIR.*/relay|for f in airc relay|[,{]airc,relay|relay-[*]' -- install.sh uninstall.sh skills; then
  fail "relay command alias install/uninstall surface remains"
fi
