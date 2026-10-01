#!/usr/bin/env bash
# Downloaded-entry/older-source regression, with local Git acquisition fixtures.
set -euo pipefail
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
GIT
printf '#!/bin/sh\necho Linux\n' > "$fixture/tools/uname"
chmod +x "$fixture/tools/git" "$fixture/tools/uname"
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
echo 'PASS: downloaded entry acquires compatible source and preserves older/developer checkouts'
