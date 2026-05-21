#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

fail() {
  echo "rust quality guard failed: $*" >&2
  exit 1
}

grep_forbid() {
  local reason="$1"
  local pattern="$2"
  shift 2
  if git grep -nE "$pattern" -- "$@" ':(exclude)test/rust_quality_guard.sh'; then
    fail "$reason"
  fi
}

grep_forbid \
  "public fallback command names must not return" \
  'message build-legacy|whois-fallback|peers-fallback|RoutePurpose::Migration' \
  crates test

grep_forbid \
  "envelope encryption failure must not emit the original plaintext payload" \
  "envelope wrap.*\\|\\|[[:space:]]*printf '%s'.*full_msg|\\|\\|[[:space:]]*printf '%s'.*full_msg" \
  crates/airc-cli/src

grep_forbid \
  "gh-gist is bootstrap/rendezvous only; do not reintroduce migration route policy" \
  'RoutePurpose::Migration|Migration => "migration"|migration -> gh-gist|bootstrap/migration' \
  crates docs/architecture

grep_forbid \
  "workspace crates must opt into workspace lint policy" \
  '^unsafe_code[[:space:]]*=' \
  crates

missing_lints=()
for manifest in crates/*/Cargo.toml; do
  if ! awk '
    /^\[lints\]$/ { in_lints = 1; next }
    /^\[/ { in_lints = 0 }
    in_lints && /^workspace[[:space:]]*=[[:space:]]*true$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$manifest"; then
    missing_lints+=("$manifest")
  fi
done

if [ "${#missing_lints[@]}" -gt 0 ]; then
  printf '%s\n' "${missing_lints[@]}" >&2
  fail "crate manifests must inherit [workspace.lints]"
fi
