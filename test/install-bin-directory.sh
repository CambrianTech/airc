#!/usr/bin/env bash
# Pure destination selection plus scratch shell PATH reconciliation. No install.
set -Eeuo pipefail
trap 'printf "FAIL: install destination fixture line %s: %s\n" "$LINENO" "$BASH_COMMAND" >&2' ERR
repo="$(cd "$(dirname "$0")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
for function in _to_bash_path _select_bin_dir _add_path_entry; do
  eval "$(sed -n "/^${function}()/,/^}/p" "$repo/install.sh")"
done
fail() { echo "$*" >&2; exit 1; }
ok() { :; }
info() { :; }
fixture_platform="$(uname -s)"
executable_name=airc
native_windows_image=""
case "$fixture_platform" in MINGW*|MSYS*|CYGWIN*)
  executable_name=airc.exe
  native_windows_image="$(_to_bash_path "${WINDIR:-${SystemRoot:?}}/System32/where.exe")" ;;
esac
uname() { printf '%s\n' "$fixture_platform"; }
original_path="$PATH"
export HOME="$fixture/home" LOCALAPPDATA="$fixture/local"
CLONE_DIR="$fixture/source"
mkdir -p "$HOME" "$fixture/first bin" "$fixture/second bin" "$CLONE_DIR/target/release" "$fixture/custom-target/debug"
native() {
  case "$fixture_platform" in
    MINGW*|MSYS*|CYGWIN*)
      # MSYS executable discovery must see a real PE image, not truncated magic.
      # This image is only inspected; the fixture never executes it.
      if [ -n "$native_windows_image" ]; then cp "$native_windows_image" "$1"
      else printf MZfixture > "$1"; fi ;;
    Darwin) printf '\317\372\355\376fixture' > "$1" ;;
    *) printf '\177ELFfixture' > "$1" ;;
  esac
  chmod +x "$1"
}
native "$fixture/first bin/$executable_name"
native "$fixture/second bin/$executable_name"
native "$CLONE_DIR/target/release/$executable_name"
native "$fixture/custom-target/debug/$executable_name"
unset BIN_DIR BIN_TARGET CARGO_TARGET_DIR
PATH="$fixture/first bin:$fixture/second bin:$original_path"
[ "$(_select_bin_dir)" = "$fixture/first bin" ]
PATH="$fixture/second bin:$fixture/first bin:$original_path"
[ "$(_select_bin_dir)" = "$fixture/second bin" ]
# An alias/function or source build is not an installed executable.
airc() { fail 'Selector executed a function'; }
alias airc='echo must-not-run'
PATH="$CLONE_DIR/target/release:$fixture/first bin:$original_path"
[ "$(_select_bin_dir)" = "$fixture/first bin" ]
CARGO_TARGET_DIR="$fixture/custom-target"
PATH="$CARGO_TARGET_DIR/debug:$fixture/first bin:$original_path"
[ "$(_select_bin_dir)" = "$fixture/first bin" ]
CARGO_TARGET_DIR='relative-target'
mkdir -p "$CLONE_DIR/$CARGO_TARGET_DIR/debug"
native "$CLONE_DIR/$CARGO_TARGET_DIR/debug/$executable_name"
PATH="$CLONE_DIR/$CARGO_TARGET_DIR/debug:$fixture/first bin:$original_path"
[ "$(_select_bin_dir)" = "$fixture/first bin" ]
BIN_DIR="$fixture/explicit directory"
[ "$(_select_bin_dir)" = "$BIN_DIR" ]
BIN_TARGET="$fixture/explicit target"
[ "$(_select_bin_dir)" = "$BIN_TARGET" ]
unset BIN_DIR BIN_TARGET CARGO_TARGET_DIR
# A shell shim is not a native binary. A PATH containing only this shim must
# fall through to the platform default, never execute it to discover identity.
printf '#!/bin/sh\nexit 91\n' > "$fixture/first bin/$executable_name"
PATH="$fixture/first bin:/usr/bin:/bin"
fixture_platform=Linux
[ "$(_select_bin_dir)" = "$HOME/.local/bin" ]
fixture_platform=MINGW64_NT-10.0
[ "$(_select_bin_dir)" = "$LOCALAPPDATA/Programs/airc" ]
native "$fixture/second bin/$executable_name"
PATH="$fixture/first bin:$fixture/second bin:/usr/bin:/bin"
[ "$(_select_bin_dir)" = "$fixture/second bin" ]
# Reconcile an existing later PATH entry and retain every unrelated tool.
PATH="$fixture/first bin:$fixture/second bin:$fixture/second bin:/usr/bin:/bin"
_add_path_entry "$fixture/second bin"
[ "$PATH" = "$fixture/second bin:$fixture/first bin:/usr/bin:/bin" ]
before="$PATH"
_add_path_entry "$fixture/second bin"
[ "$PATH" = "$before" ]
[ "$(grep -c '# airc$' "$HOME/.bashrc")" = 1 ]
echo 'PASS: shared install destination and PATH convergence'
