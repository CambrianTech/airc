#!/usr/bin/env bash
# Shared installer stage: GitHub consent, account verification and git identity.
# Both native Windows and POSIX entry points invoke this file. Never capture
# login output: gh must show its device code and browser instructions directly.
set -euo pipefail

if [ "${AIRC_SKIP_AUTH:-0}" = 1 ]; then
  printf 'GitHub authorization explicitly skipped (AIRC_SKIP_AUTH=1).\n'
  exit 0
fi

die() { printf 'AIRC authorization: %s\n' "$*" >&2; exit 1; }
command -v gh >/dev/null || die 'GitHub CLI is unavailable. Rerun AIRC setup to repair prerequisites.'

if ! gh auth status --hostname github.com >/dev/null 2>&1; then
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *)
      if [ "${EUID:-$(id -u)}" = 0 ] || [ -n "${SUDO_USER:-}" ]; then
        die 'Run setup as your normal user so authorization belongs to the account that runs AIRC.'
      fi ;;
  esac
  printf '\nGitHub authorization is required for AIRC peer discovery.\n'
  printf 'Use the code shown below at https://github.com/login/device.\n'
  printf 'Complete browser approval at your own pace; setup waits and resumes here.\n'
  gh auth login --hostname github.com --git-protocol https --web --scopes gist --skip-ssh-key ||
    die 'Authorization did not complete. Rerun the same AIRC setup to resume.'
fi

account="$(gh api --hostname github.com user --jq '.login')" || die 'Unable to verify the GitHub account.'
[ -n "$account" ] || die 'GitHub returned an empty account name.'
if [ -n "${AIRC_GITHUB_USER:-}" ] &&
   [ "$(printf '%s' "$account" | tr '[:upper:]' '[:lower:]')" != "$(printf '%s' "$AIRC_GITHUB_USER" | tr '[:upper:]' '[:lower:]')" ]; then
  die "Signed in as $account; expected $AIRC_GITHUB_USER. No mesh join was attempted."
fi

# OAuth/classic tokens report scopes. Preserve an existing usable approval;
# request an extension only when GitHub explicitly reports a missing gist scope.
# Fine-grained/environment credentials may omit this header; don't replace them.
headers="$(gh api --hostname github.com --include --silent user)" || die 'Unable to check GitHub permissions.'
scope_line="$(printf '%s\n' "$headers" | tr -d '\r' | sed -n 's/^[Xx]-[Oo][Aa][Uu][Tt][Hh]-[Ss][Cc][Oo][Pp][Ee][Ss]:[[:space:]]*//p')"
if printf '%s\n' "$headers" | grep -qi '^x-oauth-scopes:' &&
   ! printf '%s\n' "$scope_line" | tr ',' '\n' | grep -qE '^[[:space:]]*gist[[:space:]]*$'; then
  printf '\nGitHub approval for %s needs gist access for AIRC peer discovery.\n' "$account"
  printf 'Use the device code below. Setup will wait for browser approval.\n'
  gh auth refresh --hostname github.com --scopes gist ||
    die 'Gist approval did not complete. Rerun the same AIRC setup to resume.'
fi

gh auth setup-git --hostname github.com || die 'Unable to configure GitHub git authentication.'
printf 'GitHub account verified: %s. Existing authorization will be reused on reruns.\n' "$account"

# Preserve user-selected commit identity. Derive only missing values.
if [ -z "$(git config --global user.name || true)" ]; then
  name="$(gh api --hostname github.com user --jq '.name // .login')" || die 'Cannot read GitHub display name.'
  git config --global user.name "$name"
fi
if [ -z "$(git config --global user.email || true)" ]; then
  id="$(gh api --hostname github.com user --jq '.id')" || die 'Cannot read GitHub account id.'
  git config --global user.email "${id}+${account}@users.noreply.github.com"
fi
