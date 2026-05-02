#!/usr/bin/env bash
#
# AIRC installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
#
# Clones the repo, puts `airc` on PATH, symlinks skills into ~/.claude/skills/

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/airc.git"
CLONE_DIR="${AIRC_DIR:-$HOME/.airc-src}"
# BIN_DIR + SKILLS_TARGET respect env-var overrides so test harnesses
# (and packagers, distros, etc.) can point install.sh at a sandbox
# instead of stomping ~/.local/bin and ~/.claude/skills. Pre-fix, a
# test passing BIN_DIR=/tmp/foo would be silently ignored and the
# real ~/.local/bin/airc symlink would get rewritten to point at the
# test dir — caught when our own canary test corrupted the real install.
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SKILLS_TARGET="${SKILLS_TARGET:-$HOME/.claude/skills}"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn()  { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }

# MSYS / Git Bash path conversion. Three callsites in this file used the
# same `if command -v cygpath ... else sed ...` block; #205 Target #3
# collapsed them. Mirrors lib/airc_bash/platform_adapters.sh's helpers
# (defined twice on purpose: install.sh runs pre-clone so it can't
# source from $CLONE_DIR, and the helper bodies are tiny).
_to_win_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1" 2>/dev/null
  else
    printf '%s' "$1" | sed 's|^/\([a-z]\)/|\U\1:\\\\|; s|/|\\\\|g'
  fi
}
_to_bash_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1" 2>/dev/null
  else
    printf '%s' "$1" | sed 's|\\|/|g; s|^\([A-Za-z]\):|/\L\1|'
  fi
}

# ── Prereq auto-install ─────────────────────────────────────────────────
# Mirrors the Windows install.ps1 winget path: detect what's missing,
# install via the platform's package manager, then verify. Designed for
# FIRST-TIME users with nothing pre-installed beyond a shell.
#
# Required: git, gh, ssh-keygen, python3 (+ cryptography via venv pip)
# Optional: tailscale (only needed for cross-LAN mesh; LAN works without)
# Deliberately not required: openssl. Issue #341 — identity Ed25519 ops
# moved to the venv cryptography module so we don't depend on system
# openssl flavoring (LibreSSL vs OpenSSL etc).
#
# AIRC_SKIP_PREREQS=1 short-circuits the whole block (CI, dev installs,
# users who manage their own packages).

detect_pkgmgr() {
  case "$(uname -s 2>/dev/null)" in
    Darwin)
      if command -v brew >/dev/null 2>&1; then echo "brew"; return; fi
      echo "brew-missing"; return ;;
    Linux)
      if command -v apt-get >/dev/null 2>&1; then echo "apt";    return; fi
      if command -v dnf     >/dev/null 2>&1; then echo "dnf";    return; fi
      if command -v pacman  >/dev/null 2>&1; then echo "pacman"; return; fi
      if command -v apk     >/dev/null 2>&1; then echo "apk";    return; fi
      ;;
    MINGW*|MSYS*|CYGWIN*)
      # Windows Git Bash / MSYS2 / Cygwin. winget is the standard
      # package manager on modern Windows and what install.ps1 uses;
      # it's reachable from Git Bash as winget.exe via PATH or as
      # `cmd /c winget`. If winget isn't there (older Win10), fall
      # through to the unknown branch which emits the manual prereq
      # list. Issue #83 follow-up: pre-fix, install.sh on Git Bash
      # said "Unknown package manager (uname=MINGW64_NT-10.0-26200)"
      # and skipped prereq install entirely.
      if command -v winget.exe >/dev/null 2>&1 || command -v winget >/dev/null 2>&1; then
        echo "winget"; return
      fi
      ;;
  esac
  echo "unknown"
}

# Map a generic prereq name to the package id for a given pkg manager.
# Most names match across managers; the exceptions are listed inline.
pkgname_for() {
  local mgr="$1" prereq="$2"
  case "$prereq" in
    ssh-keygen|ssh)
      case "$mgr" in
        brew)   echo "openssh" ;;
        apt)    echo "openssh-client" ;;
        dnf)    echo "openssh-clients" ;;
        pacman) echo "openssh" ;;
        apk)    echo "openssh-client" ;;
        winget) echo "" ;;  # OpenSSH ships with modern Windows; nothing to install
      esac ;;
    python3)
      case "$mgr" in
        pacman) echo "python" ;;
        winget) echo "Python.Python.3.12" ;;
        *)      echo "python3" ;;
      esac ;;
    git)
      case "$mgr" in
        winget) echo "Git.Git" ;;
        *)      echo "git" ;;
      esac ;;
    gh)
      case "$mgr" in
        winget) echo "GitHub.cli" ;;
        *)      echo "gh" ;;
      esac ;;
    *) echo "$prereq" ;;
  esac
}

install_with_pkgmgr() {
  local mgr="$1"; shift
  local pkgs=("$@")
  [ ${#pkgs[@]} -eq 0 ] && return 0
  case "$mgr" in
    brew)   brew install "${pkgs[@]}" ;;
    apt)    sudo apt-get update -qq && sudo apt-get install -y "${pkgs[@]}" ;;
    dnf)    sudo dnf install -y "${pkgs[@]}" ;;
    pacman) sudo pacman -S --noconfirm --needed "${pkgs[@]}" ;;
    apk)    sudo apk add --no-cache "${pkgs[@]}" ;;
    winget)
      # winget on Git Bash: install one ID at a time, --accept-* flags so
      # it doesn't prompt during the script. winget.exe is the binary;
      # plain `winget` works if PATHEXT is honored.
      local wbin; wbin=$(command -v winget.exe 2>/dev/null || command -v winget 2>/dev/null || true)
      [ -z "$wbin" ] && return 1
      local pkg
      for pkg in "${pkgs[@]}"; do
        [ -z "$pkg" ] && continue
        "$wbin" install --id "$pkg" --silent --accept-source-agreements --accept-package-agreements 2>&1 \
          || warn "winget install $pkg returned non-zero (may already be installed; continuing)"
      done ;;
    *)      return 1 ;;
  esac
}



ensure_prereqs() {
  [ "${AIRC_SKIP_PREREQS:-0}" = "1" ] && { info "AIRC_SKIP_PREREQS=1 -- skipping prereq install"; return 0; }

  local mgr; mgr=$(detect_pkgmgr)
  if [ "$mgr" = "unknown" ] || [ "$mgr" = "brew-missing" ]; then
    if [ "$mgr" = "brew-missing" ]; then
      # Joel 2026-04-29: 'whatever agent we have ought to talk to the
      # user about prereq if they dont have something, and it should
      # still auto install for the most part'.
      # When stdin is a TTY, just RUN the official Homebrew installer
      # — it asks for the user's password (sudo) and runs cleanly.
      # User clicks through prompts. AI-driven installs see the prompt
      # surface and can guide the user. Non-TTY installs fall through
      # to the manual instruction.
      warn "macOS detected but Homebrew not found."
      if [ -t 0 ] && [ -t 1 ]; then
        info "  Running Homebrew's official installer now (you'll be prompted for sudo)..."
        if /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"; then
          # Refresh PATH so the rest of this script sees brew.
          if [ -x /opt/homebrew/bin/brew ]; then
            eval "$(/opt/homebrew/bin/brew shellenv)"
          elif [ -x /usr/local/bin/brew ]; then
            eval "$(/usr/local/bin/brew shellenv)"
          fi
          if command -v brew >/dev/null 2>&1; then
            ok "Homebrew installed and active in this session"
            mgr="brew"
          else
            warn "Homebrew install ran but 'brew' still not on PATH — open a new shell and re-run install.sh"
            return 0
          fi
        else
          warn "Homebrew install did not complete. Manual:"
          warn "  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
          warn "  Then re-run this installer."
          return 0
        fi
      else
        warn "  Install Homebrew first:  /bin/bash -c \"\$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\""
        warn "  Then re-run this installer."
        warn "  (auto-install requires a TTY; this is a non-interactive run)"
        return 0
      fi
    else
      warn "Unknown package manager (uname=$(uname -s)). Skipping prereq auto-install."
      warn "Required prereqs: git, gh, python3 (cryptography via pip)"
      return 0
    fi
  fi

  local missing=() pkgs=() unmappable=()
  # #188: jq removed — airc's gist envelope parser now uses Python's
  # stdlib JSON (lib/airc_core/gistparse.py). Python was already a hard
  # dep since #152 Phase 0; jq was redundant. Drop the dep + the
  # winget step that would install it.
  # Issue #341 follow-up: openssl removed from the prereq list. airc
  # no longer shells out to it for Ed25519 — identity gen + signing
  # both route through the venv cryptography module (which is already
  # a hard dep, pip-installed below). LibreSSL on macOS used to make
  # this an ordeal; now it's a non-issue at the source.
  for cmd in git gh ssh-keygen python3; do
    # Strict probe: presence on PATH AND a successful --version invocation.
    # Used selectively: python3 needs the strict variant because Windows
    # Store's python3.exe alias is on PATH but exits 49 with a Store-
    # redirect (continuum-b69f, 2026-04-27). git/gh all
    # support --version cleanly. ssh-keygen does NOT have a version
    # flag at all (different from `ssh -V`); calling `ssh-keygen
    # --version` exits non-zero on every install, so the strict probe
    # produces false positives — Joel 2026-04-28 saw "ssh-keygen needs
    # manual install on winget" on a perfectly good Git for Windows
    # install. Skip the strict variant for ssh-keygen; presence-on-PATH
    # is sufficient since Git for Windows bundles a working binary.
    local _missing=0
    if ! command -v "$cmd" >/dev/null 2>&1; then
      _missing=1
    elif [ "$cmd" != "ssh-keygen" ] && ! "$cmd" --version >/dev/null 2>&1; then
      _missing=1
    fi
    if [ "$_missing" = "1" ]; then
      missing+=("$cmd")
      local pkg; pkg=$(pkgname_for "$mgr" "$cmd")
      if [ -z "$pkg" ]; then
        # Manager has no auto-install path for this prereq (e.g., winget
        # treats ssh + openssl as bundled-with-Windows / Git-for-Windows
        # but the user hits this case if those bundles are absent).
        # Surface clearly instead of silently skipping (#92 Copilot).
        unmappable+=("$cmd")
      else
        pkgs+=("$pkg")
      fi
    fi
  done
  if [ ${#missing[@]} -gt 0 ]; then
    if [ ${#pkgs[@]} -gt 0 ]; then
      info "Installing missing prereqs via $mgr: ${pkgs[*]}"
      if install_with_pkgmgr "$mgr" "${pkgs[@]}"; then
        ok "Auto-installable prereqs installed"
      else
        warn "Package install reported failure; airc may not run until you fix: ${missing[*]}"
      fi
    else
      warn "Missing prereqs not auto-installable on $mgr: ${missing[*]}"
    fi
    if [ ${#unmappable[@]} -gt 0 ]; then
      warn "These prereqs need manual install on $mgr: ${unmappable[*]}"
      case "$mgr" in
        winget)
          warn "  ssh / ssh-keygen: Settings -> Apps -> Optional Features -> Add OpenSSH Client" ;;
      esac
    fi
  else
    ok "All required prereqs present"
  fi
  # Issue #341 follow-up: openssl Ed25519-capability probe + brew
  # install dance removed. Identity gen + signing live in the venv
  # cryptography module now; the system openssl version (LibreSSL or
  # otherwise) is irrelevant to airc.

  # Post-3c: sshd setup + Tailscale install fully removed. Cross-network
  # messaging routes through gh-as-bearer (envelope-encrypted gist),
  # which works on every platform with `gh auth login` — no privileged
  # daemon, no sign-in popup, no admin elevation. The earlier sshd-on-
  # by-default block (with sudo/UAC prompt + AIRC_SKIP_SSHD escape +
  # CI auto-detect) was deleted as part of issue #341 follow-up #345
  # (doctor's sshd probe also dropped); leaving this single tombstone
  # comment so a reader who finds 'sshd' in old git history sees why
  # it's not here anymore.

  # gh auth: required for the gist substrate. We CAN drive the login
  # interactively when stdin is a TTY (Joel 2026-04-29: 'thought that'd
  # be in setup or at least doctor (then claude could always do it for
  # them)'). The browser/device-code flow needs a real user to click
  # but doesn't need them to remember the command. Falls back to the
  # warning path for non-interactive installs (curl|bash piped without
  # a TTY, CI, etc).
  if command -v gh >/dev/null 2>&1; then
    if ! gh auth status >/dev/null 2>&1; then
      # Skip the interactive auth path under sudo/root: gh stores the token
      # for the calling user (root's keyring), but airc runs as the real
      # user and reads the real user's token. Authing as root silently
      # produces a working-as-root / broken-as-user state. Joel 2026-04-29:
      # 'detect and if not, open it if it isnt sudo'.
      _running_as_root=0
      if [ "${EUID:-$(id -u 2>/dev/null || echo 1000)}" = "0" ] || [ -n "${SUDO_USER:-}" ]; then
        _running_as_root=1
      fi
      if [ "$_running_as_root" = "1" ]; then
        warn "gh is not authenticated, and install is running as root/sudo."
        warn "  Don't auth gh as root — re-run as your normal user, or run once after install:"
        warn "    gh auth login -h github.com -s gist"
      elif [ -t 0 ] && [ -t 1 ]; then
        # Pause-with-Enter before handing the user off to gh's device-code
        # flow. Without this break, the gh prompt + browser popup arrives
        # mid-install-output and looks like the script hung — the user
        # has no signal that "you're now in a different tool". Match
        # Claude Code's installer convention: bold green "==>" headline,
        # bold action line, explicit "Press Enter / Ctrl+C" prompt.
        # Honor AIRC_INSTALL_YES=1 for power users who curl|bash often.
        printf '\n  \033[1;32m==>\033[0m GitHub authentication required for the gist substrate.\n'
        printf '      About to launch: \033[1mgh auth login -h github.com -s gist\033[0m\n'
        printf '      A browser will open; the device code shown in the terminal must be pasted there.\n'
        if [ "${AIRC_INSTALL_YES:-0}" != "1" ]; then
          printf '      Press Enter to continue, Ctrl+C to abort: '
          read -r _ || true
          printf '\n'
        fi
        if gh auth login -h github.com -s gist; then
          ok "gh auth complete"
          # Re-run setup-git so the just-acquired token gets wired.
          gh auth setup-git 2>/dev/null && info "  gh token wired into git credential helper" || true
        else
          warn "gh auth login did not complete — re-run when ready:"
          warn "    gh auth login -h github.com -s gist"
        fi
      else
        warn "gh CLI is not authenticated. Run once before 'airc join':"
        warn "    gh auth login -h github.com -s gist"
        warn "  (interactive; can't run from a non-TTY install)"
      fi
    else
      # Wire gh's token into git's credential helper. Without this,
      # every git-over-HTTPS op (gist fetch/push -- airc's substrate
      # hot path) prompts the user for a password, repeatedly. gh ships
      # with `gh auth git-credential` for exactly this purpose; the
      # `gh auth setup-git` one-liner registers it in ~/.gitconfig.
      # Idempotent (no-op if already configured), safe to always run.
      # Joel hit this on 2026-04-28 — Windows install where gh was
      # auth'd-in-keyring but git itself didn't know. Resulted in a
      # GUI password popup every airc operation that touched a gist.
      if ! git config --global --get-all credential.https://github.com.helper 2>/dev/null | grep -q 'gh auth git-credential'; then
        if gh auth setup-git 2>/dev/null; then
          info "  gh token wired into git credential helper (no more password popups for gist ops)"
        fi
      fi
    fi
  fi
}

ensure_prereqs

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ]; then
  # AIRC_INSTALL_NO_PULL=1: trust CLONE_DIR's checked-out tree exactly
  # as-is — no branch switch, no pull. CI uses this when it has already
  # staged the PR's tree at $CLONE_DIR via `cp -r .` and wants the
  # smoke matrix to exercise the PR's code, not whatever's on main.
  # Without this escape hatch, install.sh's "I'm-on-a-non-channel-branch
  # so let me reset to main" recovery path silently overwrites the
  # PR's code with origin/main's — making the PR's CI a no-op.
  if [ "${AIRC_INSTALL_NO_PULL:-0}" = "1" ]; then
    info "AIRC_INSTALL_NO_PULL=1 — using CLONE_DIR tree as-is, skipping branch-switch + pull"
  else
  info "Updating existing install"
  # Recovery: if the install dir is on a non-channel branch (e.g. someone
  # / some AI checked out a feature branch for testing and forgot to
  # switch back), the ff-pull below fails with cryptic "Not possible to
  # fast-forward". Worse, the user can't escape via `airc canary` if
  # they're on a pre-channels binary — `canary` is an unknown command
  # there. So install.sh itself takes responsibility: detect non-channel
  # branches + auto-switch to the saved channel (or main) before pulling.
  CURRENT_BRANCH=$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
  SAVED_CHANNEL=""
  [ -f "$CLONE_DIR/.channel" ] && SAVED_CHANNEL=$(tr -d '[:space:]' < "$CLONE_DIR/.channel")
  TARGET_BRANCH="${SAVED_CHANNEL:-main}"
  case "$CURRENT_BRANCH" in
    main|canary)
      # On a known channel — leave it alone unless the saved channel
      # disagrees (e.g. user just `airc channel canary`'d but didn't
      # update yet).
      if [ -n "$SAVED_CHANNEL" ] && [ "$SAVED_CHANNEL" != "$CURRENT_BRANCH" ]; then
        info "Saved channel '$SAVED_CHANNEL' differs from current branch '$CURRENT_BRANCH' — switching"
        git -C "$CLONE_DIR" fetch --quiet origin "$SAVED_CHANNEL"
        git -C "$CLONE_DIR" checkout -q "$SAVED_CHANNEL" \
          || git -C "$CLONE_DIR" checkout -q -B "$SAVED_CHANNEL" "origin/$SAVED_CHANNEL"
      fi
      ;;
    *)
      info "Install dir on '$CURRENT_BRANCH' (not a known channel) — switching to '$TARGET_BRANCH'"
      git -C "$CLONE_DIR" fetch --quiet origin "$TARGET_BRANCH" || {
        echo "ERROR: Couldn't fetch origin/$TARGET_BRANCH. Network? gh auth?" >&2
        exit 1
      }
      git -C "$CLONE_DIR" checkout -q "$TARGET_BRANCH" \
        || git -C "$CLONE_DIR" checkout -q -B "$TARGET_BRANCH" "origin/$TARGET_BRANCH" \
        || {
          cat >&2 <<EOF
ERROR: Couldn't switch $CLONE_DIR to '$TARGET_BRANCH'.
Recover manually:
  cd $CLONE_DIR
  git fetch origin
  git status               # see why checkout was blocked
  git stash                # if you have local edits worth keeping
  git checkout $TARGET_BRANCH
  git pull --ff-only
  bash install.sh
EOF
          exit 1
        }
      ;;
  esac
  if ! git -C "$CLONE_DIR" pull --ff-only --quiet 2>&1; then
    cat >&2 <<EOF
ERROR: Couldn't fast-forward $CLONE_DIR (currently on $(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)).
Likely cause: local edits or a divergent history.
Recover with:
  cd $CLONE_DIR
  git status
  git stash               # if you have local edits worth keeping
  git fetch origin
  git reset --hard origin/$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null)
  bash install.sh
EOF
    exit 1
  fi
  fi  # AIRC_INSTALL_NO_PULL guard
else
  # First install. Honor AIRC_CHANNEL if set so users can land on canary
  # directly via `AIRC_CHANNEL=canary curl|bash` without a follow-up
  # `airc canary && airc update` dance. Default to main (the release
  # branch) when AIRC_CHANNEL is unset. Caught by vhsm-d1f4 2026-04-28
  # during the #191 release-gate fresh-install verification: env var was
  # silently ignored, install landed on main.
  CHANNEL_TARGET="${AIRC_CHANNEL:-main}"
  case "$CHANNEL_TARGET" in
    main|canary) ;;
    *)
      warn "AIRC_CHANNEL='$CHANNEL_TARGET' is not a known channel (main, canary). Defaulting to main."
      CHANNEL_TARGET="main"
      ;;
  esac
  info "Installing AIRC (channel: $CHANNEL_TARGET)"
  git clone --quiet --branch "$CHANNEL_TARGET" "$REPO_URL" "$CLONE_DIR"
  # Persist the channel choice so future `airc update` follows the same
  # branch. Mirrors what `airc canary` / `airc main` write.
  echo "$CHANNEL_TARGET" > "$CLONE_DIR/.channel"
fi

# ── airc on PATH ───────────────────────────────────────────────────────

mkdir -p "$BIN_DIR"
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/airc"
# Back-compat: `relay` still works for muscle-memory and stale docs.
# The airc binary detects the invocation name and behaves identically.
ln -sf "$CLONE_DIR/airc" "$BIN_DIR/relay"

# Windows: also place airc.cmd + airc.ps1 forwarders on PATH.
# Without these, `airc` invoked from native PowerShell or cmd.exe
# resolves to the bash script, which PowerShell can't execute
# ("Cannot run a document in the middle of a pipeline"). PR #262
# made airc.ps1 a thin forwarder to bash, but that's moot if the
# .ps1 isn't on PATH. cp (not ln -sf) — Windows symlinks are
# privileged + flaky; copying is universal. Caught by
# continuum-b69f 2026-04-29 (issue #249 PowerShell row).
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    [ -f "$CLONE_DIR/airc.cmd" ] && cp -f "$CLONE_DIR/airc.cmd" "$BIN_DIR/airc.cmd"
    [ -f "$CLONE_DIR/airc.ps1" ] && cp -f "$CLONE_DIR/airc.ps1" "$BIN_DIR/airc.ps1"
    ;;
esac

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    if [ -f "$rc" ] && ! grep -q 'airc' "$rc"; then
      echo 'export PATH="$HOME/.local/bin:$PATH"  # airc' >> "$rc"
      ok "Added ~/.local/bin to PATH in $(basename "$rc")"
      break
    fi
  done
  export PATH="$BIN_DIR:$PATH"
fi

# ── Python venv with crypto deps (Phase E: envelope encryption) ────────
# airc's envelope-layer end-to-end encryption needs the cryptography
# package. We create a venv inside the install dir and pip-install
# there because PEP 668 makes `pip install --user` fail on managed
# Pythons (homebrew, system Python on Debian/Ubuntu/etc). The venv
# avoids touching system Python at all — fully self-contained.
#
# airc's bash wrapper detects this venv at AIRC_PYTHON resolution time
# and prefers it over system python3. If venv setup fails (no python3,
# pip module missing, network failure during install), airc falls back
# to system python3 and runs in plaintext mode. Per the "no scary
# popups" rule: pip-install never elevates, never prompts; failures
# print a non-fatal warning.
_airc_venv="$CLONE_DIR/.venv"
if [ ! -d "$_airc_venv" ] && command -v python3 >/dev/null 2>&1; then
  if python3 -m venv "$_airc_venv" 2>/dev/null; then
    ok "Python venv created: $_airc_venv"
  else
    warn "Could not create Python venv (python3-venv missing?). airc will run in plaintext mode."
  fi
fi
# Locate venv pip — POSIX vs Windows-Git-Bash paths.
_airc_venv_pip=""
if [ -x "$_airc_venv/bin/pip" ]; then
  _airc_venv_pip="$_airc_venv/bin/pip"
elif [ -x "$_airc_venv/Scripts/pip.exe" ]; then
  _airc_venv_pip="$_airc_venv/Scripts/pip.exe"
fi
if [ -n "$_airc_venv_pip" ]; then
  # Check if cryptography is already installed (idempotent install).
  _airc_venv_python_bin=""
  if [ -x "$_airc_venv/bin/python" ]; then
    _airc_venv_python_bin="$_airc_venv/bin/python"
  elif [ -x "$_airc_venv/Scripts/python.exe" ]; then
    _airc_venv_python_bin="$_airc_venv/Scripts/python.exe"
  fi
  if [ -n "$_airc_venv_python_bin" ] && \
     "$_airc_venv_python_bin" -c "import cryptography" >/dev/null 2>&1; then
    : # already installed; skip
  else
    if "$_airc_venv_pip" install -q --upgrade pip >/dev/null 2>&1; then : ; fi
    if "$_airc_venv_pip" install -q cryptography 2>&1 | tail -3; then
      ok "cryptography installed in venv (envelope encryption ready)"
    else
      warn "cryptography install failed; airc will run in plaintext mode"
      warn "  Manual fix:  $_airc_venv_pip install cryptography"
    fi
  fi
fi

# ── Skills into agent skill dirs (Claude Code + Codex) ─────────────────
#
# Both Claude Code and OpenAI Codex use the same on-disk skill format:
# a directory per skill, with a SKILL.md inside (YAML frontmatter +
# markdown body). They differ only in WHERE they look:
#   Claude Code → ~/.claude/skills/<name>/
#   Codex       → ~/.codex/skills/<name>/
#
# We symlink airc's skills into both whenever the corresponding agent
# is installed on the machine. Each agent picks up the same skill
# content; airc's skill text is intentionally written to be agent-
# generic where the operation is shell-callable (which most airc verbs
# are). Claude-Code-specific nuances like Monitor invocations are
# additive — Codex agents fall back to direct shell calls.

_install_airc_skills_into() {
  local skills_target="$1" agent_label="$2"
  [ -d "$CLONE_DIR/skills" ] || return 0
  mkdir -p "$skills_target"

  # Clean up old symlinks from previous installs.
  # Includes the airc-classic skill names (connect/send/rename/disconnect) that
  # were renamed to IRC-canonical (join/msg/nick/quit) — leaving the old symlinks
  # in place would shadow the new skills with stale content. (`uninstall` was
  # previously listed here when the skill didn't exist; now that we ship a real
  # /uninstall skill, the per-skill symlink loop below recreates it cleanly and
  # this list omits it.)
  local old
  for old in "$skills_target"/relay-* "$skills_target"/monitor "$skills_target"/setup \
             "$skills_target"/connect "$skills_target"/send "$skills_target"/rename "$skills_target"/disconnect; do
    [ -L "$old" ] && rm "$old" 2>/dev/null
  done

  local skill_dir skill_name target
  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$skills_target/$skill_name"
    # If the target is a real directory (from a pre-rename hand-install
    # or an old copy-based installer), it shadows the new symlink. Nuke it.
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      rm -rf "$target"
    elif [ -L "$target" ]; then
      rm "$target"
    fi
    ln -sf "$skill_dir" "$target"
    ok "Skill ($agent_label): /$skill_name"
  done
}

# Claude Code: install whenever the SKILLS_TARGET path exists or is
# requested via env. The previous behavior was unconditional; preserve.
_install_airc_skills_into "$SKILLS_TARGET" "claude-code"

# Codex: install only when `codex` is on PATH AND ~/.codex exists (i.e.
# Codex has been run at least once and created its config dir). Skips
# silently on machines where Codex isn't installed, so this is a
# no-op for Claude-Code-only setups. Honors CODEX_SKILLS_TARGET env
# override for the same reason BIN_DIR / SKILLS_TARGET do (test
# harnesses + non-default Codex layouts).
if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_skills_into "${CODEX_SKILLS_TARGET:-$HOME/.codex/skills}" "codex"
fi

# ── Codex permission profile (network access for gh subcommands) ───────
# Codex's default sandbox blocks subcommand network egress. airc's substrate
# IS gh-API-driven, so without elevation, every airc verb fails with
# 'error connecting to github.com' or 'token invalid' depending on which
# layer the call lands at. Codex skills can't declare required permissions
# inline, so the cleanest automation is to write a named permission profile
# scoped to ONLY github.com / api.github.com / gist.github.com, then set
# default_permissions = "airc" if no other default is configured. Per
# Codex docs, named permission profiles round-trip across TUI sessions
# and are the preferred way to grant scoped network access.
#
# Idempotent: only adds [permissions.airc.network] if not already present;
# only sets default_permissions = "airc" if no default is currently set.
# A user who has set a different default keeps it + can invoke airc-needing
# Codex sessions via `codex --profile airc`.
#
# Honors AIRC_SKIP_CODEX_CONFIG=1 if a user (or test harness) wants the
# skill symlinks but NOT the config write.

_install_airc_codex_permission_profile() {
  local config="$HOME/.codex/config.toml"
  [ "${AIRC_SKIP_CODEX_CONFIG:-0}" = "1" ] && return 0
  [ -f "$config" ] || touch "$config"

  local _changed=0

  # Append the named profile if absent. The block goes at the end of the
  # file (TOML allows section order to be arbitrary; downstream sections
  # don't capture this one because [permissions.airc.network] is its own
  # explicit header).
  if ! grep -q '^\[permissions\.airc\.network\]' "$config" 2>/dev/null; then
    cat >> "$config" <<'TOML'

# airc network permissions — added by airc install.sh so gh subcommands
# (which the substrate is built on) can reach GitHub from inside Codex's
# default sandbox. Scoped to ONLY the gh hosts airc actually uses; other
# domains stay restricted. Remove this block + `default_permissions = "airc"`
# below to opt out. Re-runs of install.sh detect existing presence and
# don't duplicate.
[permissions.airc.network]
enabled = true
mode = "limited"
domains = { "github.com" = "allow", "api.github.com" = "allow", "gist.github.com" = "allow" }
TOML
    _changed=1
  fi

  # Filesystem permissions: NOT WRITTEN. Initially we tried granting writes
  # to ~/.airc-src/ + ~/.airc/ + ~/.local/bin/airc + a :project_roots
  # block — Codex's runtime hard-rejected the profile at startup with:
  #   "permissions profile requests filesystem writes outside the
  #   workspace root, which is not supported until the runtime enforces
  #   FileSystemSandboxPolicy directly"
  # …meaning Codex 0.125 can't honor home-dir-scoped filesystem grants in
  # named profiles yet. Even the :project_roots-only variant didn't help.
  # The startup error broke every Codex session on the machine. We removed
  # the block entirely; living with Codex's "does not define any recognized
  # filesystem entries" warning is preferable to a hard-fail-on-startup.
  # When Codex's runtime supports outside-workspace filesystem profiles,
  # restore the block (history at git log -- install.sh).

  # Cleanup: machines that ran the buggy intermediate (3b20369..c1)
  # still have the [permissions.airc.filesystem] block in their
  # config.toml and Codex won't start. Detect and strip it on every
  # install.sh run so Codex starts cleanly without the user having
  # to hand-edit their config.
  if grep -q '^\[permissions\.airc\.filesystem\]' "$config" 2>/dev/null; then
    info "Removing stale [permissions.airc.filesystem] block from ~/.codex/config.toml (Codex 0.125 doesn't support outside-workspace filesystem profiles; was breaking session startup)..."
    "${AIRC_PYTHON:-python3}" - "$config" <<'PY'
import sys, re
path = sys.argv[1]
with open(path) as f:
    text = f.read()
# Strip from any '# airc filesystem permissions' header (or bare
# [permissions.airc.filesystem] header) through end of that section
# and any [permissions.airc.filesystem.<sub>] children. Section ends
# at the next top-level header that is NOT under [permissions.airc.filesystem].
lines = text.splitlines(keepends=True)
out = []
in_airc_fs = False
for line in lines:
    stripped = line.strip()
    if stripped.startswith('# airc filesystem permissions'):
        # Drop the leading comment block too (cohesive with the section)
        in_airc_fs = True
        continue
    if stripped.startswith('[permissions.airc.filesystem'):
        in_airc_fs = True
        continue
    if in_airc_fs:
        # Continue dropping comment lines and key=value lines until we
        # hit a new section header that isn't airc.filesystem.
        if stripped.startswith('[') and not stripped.startswith('[permissions.airc.filesystem'):
            in_airc_fs = False
            out.append(line)
        # else: drop (comment, blank, or key=value within the section)
        continue
    out.append(line)
# Collapse runs of >2 blank lines that the strip might have left.
result = ''.join(out)
result = re.sub(r'\n{3,}', '\n\n', result)
with open(path, 'w') as f:
    f.write(result)
PY
    _changed=1
  fi

  # Set default_permissions = "airc" at the file's top level, but only if
  # no default is currently set. A pre-existing default belongs to the
  # user; we don't overwrite. We prepend to the file so the assignment
  # lands at the top level and is not captured by any section that
  # already opens further down.
  if ! grep -qE '^[[:space:]]*default_permissions[[:space:]]*=' "$config" 2>/dev/null; then
    local _tmp; _tmp=$(mktemp)
    {
      printf '# airc: default permission profile (added by install.sh; remove to opt out)\n'
      printf 'default_permissions = "airc"\n\n'
      cat "$config"
    } > "$_tmp"
    mv "$_tmp" "$config"
    _changed=1
  elif ! grep -qE '^[[:space:]]*default_permissions[[:space:]]*=[[:space:]]*"airc"' "$config" 2>/dev/null; then
    # Different default already set — don't override, but tell the user
    # how to use airc explicitly without changing their default.
    info "  ~/.codex/config.toml already has default_permissions set; invoke airc-needing Codex sessions via:  codex --profile airc"
  fi

  if [ "$_changed" = "1" ]; then
    ok "Added airc network profile to ~/.codex/config.toml — restart Codex to activate (gh subcommands work in airc-needing sessions)."
  fi
}

if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_codex_permission_profile
fi

# ── Codex GH_TOKEN env injection ───────────────────────────────────────
# Codex's sandbox can't reliably reach the macOS Keychain to validate
# gh's stored token. Result: gh auth status flakes between ✓ and X
# within a single Codex session, airc join trips on the X path even
# though the token is real and valid (Joel hit this on the codex
# first-encounter QA; openai/codex#10695 is the upstream tracking bug
# with confirmation from a contributor that Codex's shell handlers
# don't merge dependency env into spawned processes; patch in flight).
#
# Workaround per OpenAI's own maintainer guidance ("If echo $GH_TOKEN
# is defined at app launch it's visible to sandboxed tools"): inject
# the current gh token into Codex's [shell_environment_policy.set]
# block. Codex's docs confirm this map is "Explicit environment
# overrides injected into every subprocess" — exactly what we need.
#
# Token plaintext on disk in ~/.codex/config.toml is the security
# trade-off. Same trust posture as ~/.codex/auth.json (which already
# holds the user's OpenAI credentials); both are 0600-by-default in
# the user's home dir. Joel signed off on this trade-off as cleaner
# than (a) PATH-shadowing codex, (b) shellrc-exporting GH_TOKEN to
# every shell, or (c) asking the user to type the launch one-liner
# every time.
#
# Idempotent + token-refreshing: every install.sh run (including
# `airc update`) strips any prior airc-managed block and rewrites
# with the current `gh auth token` output. Bracket markers make the
# block detectable + removable cleanly.
#
# Honors AIRC_SKIP_CODEX_TOKEN=1 if the user wants the network/
# permission profile but NOT the token injection (e.g. they prefer
# to manage GH_TOKEN themselves via shell alias).

_install_airc_codex_gh_token() {
  local config="$HOME/.codex/config.toml"
  [ "${AIRC_SKIP_CODEX_TOKEN:-0}" = "1" ] && return 0
  [ -f "$config" ] || return 0
  command -v gh >/dev/null 2>&1 || return 0

  # Pull current token. If gh is unauthed or fails for any reason,
  # silently skip — better to leave existing block alone than write
  # an empty token that breaks Codex sessions.
  local token; token=$(gh auth token 2>/dev/null) || return 0
  [ -z "$token" ] && return 0

  local marker_start='# AIRC-GH-TOKEN-START — managed by install.sh; airc update refreshes the token; remove this section through AIRC-GH-TOKEN-END to opt out'
  local marker_end='# AIRC-GH-TOKEN-END'

  # Strip any prior airc-managed block (handles token rotation across
  # install.sh runs). sed range-delete from start marker through end
  # marker, inclusive.
  if grep -qF "AIRC-GH-TOKEN-START" "$config" 2>/dev/null; then
    local _tmp; _tmp=$(mktemp)
    sed '/^# AIRC-GH-TOKEN-START/,/^# AIRC-GH-TOKEN-END/d' "$config" > "$_tmp"
    mv "$_tmp" "$config"
  fi

  # Append fresh block. Uses [shell_environment_policy.set] sub-table
  # rather than inline `set = { ... }` syntax so it composes with any
  # user-defined [shell_environment_policy] keys at the parent level
  # (e.g. inherit, include_only) without conflict.
  cat >> "$config" <<TOML

$marker_start
[shell_environment_policy.set]
GH_TOKEN = "$token"
$marker_end
TOML

  ok "Codex GH_TOKEN injection refreshed in ~/.codex/config.toml (gh's current token; restart Codex to apply)"
}

if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_codex_gh_token
fi

# ── Codex pre-approve airc command prefix ──────────────────────────────
# Codex's per-command approval gate also restricts network access — a
# command not in the user's "always run" allowlist runs in a stricter
# sandbox where network is blocked. Joel's Codex first-encounter QA
# hit this on `airc msg`: `airc join` had been pre-approved earlier so
# its gh API calls reached the network, but `airc msg` hadn't, so its
# gh API calls hit a network sandbox and failed. Codex prompted the
# user to "always run commands that start with airc msg" and once
# approved, it worked instantly.
#
# Codex supports declaring approved command prefixes statically in
# config.toml's [rules] block (per Codex docs config-reference). Adding
# `airc` as an allow-prefix pre-approves ALL airc verbs (join, msg,
# status, peers, etc) so the user never sees the per-command approval
# prompt cycle. Idempotent: only adds if not already present.
#
# Honors AIRC_SKIP_CODEX_RULES=1 if a user wants to manage approvals
# manually.

_install_airc_codex_command_rules() {
  local config="$HOME/.codex/config.toml"
  [ "${AIRC_SKIP_CODEX_RULES:-0}" = "1" ] && return 0
  [ -f "$config" ] || return 0
  if grep -qF 'AIRC-COMMAND-RULES-START' "$config" 2>/dev/null; then
    return 0
  fi
  cat >> "$config" <<'TOML'

# AIRC-COMMAND-RULES-START — managed by install.sh; pre-approves all
# `airc *` commands so they aren't blocked by Codex's per-command approval
# gate (which also restricts network access for un-approved commands).
# Without this, only commands the user has manually approved-with-always
# can reach the gist substrate; airc msg / airc status / etc would
# silently fail in the sandbox until first-time approval. Remove this
# block through AIRC-COMMAND-RULES-END to opt out.
[rules]
prefix_rules = [
  { pattern = [{ token = "airc" }], decision = "allow" }
]
# AIRC-COMMAND-RULES-END
TOML
  ok "Codex airc-command pre-approval rule added to ~/.codex/config.toml — restart Codex to activate (no per-command approval prompts for airc verbs)"
}

if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_codex_command_rules
fi


# ── Optional: background daemon for sleep/wake/crash survival (#382) ───
#
# Issue: by default the mesh dies when peer laptops sleep — `airc connect`
# is just a process, sleeps with the machine, never re-spawns on wake.
# The remedy (`airc daemon install`) already exists but was only surfaced
# AFTER the mesh had gone down (see the in-disconnect tip in the airc
# top-level). By that time peers have missed however many hours of mesh
# activity. This block surfaces the offer at install time, when the user
# is already engaged in setup and can flip the auto-restart on with one
# keystroke.
#
# Skip conditions:
#   - daemon already installed (idempotent re-run)
#   - non-TTY install (curl-bash piped without terminal)
#   - AIRC_INSTALL_NO_DAEMON=1 (explicit opt-out for headless servers,
#     CI runners, environments that manage daemons via their own
#     config-management like Ansible/Chef/Nix)
#   - AIRC_INSTALL_YES=1 (power-user one-liner: install the daemon
#     without asking)
# Source the centralized cross-platform daemon detector + its dependency
# (detect_platform from platform_adapters.sh). Lets install.sh ask the
# same "is the daemon installed?" question that cmd_daemon.sh + cmd_connect.sh
# ask, so the answer is consistent across darwin / linux / wsl / windows.
# Pre-fix install.sh had its own _daemon_already_installed() that only
# covered Darwin/Linux file paths — Copilot review on PR #388 caught
# that this would re-prompt on every install rerun on Windows Git Bash
# even after `airc daemon install` had registered the HKCU Run-key.
if [ -f "$CLONE_DIR/lib/airc_bash/platform_adapters.sh" ] \
   && [ -f "$CLONE_DIR/lib/airc_bash/lib_daemon_detect.sh" ]; then
  # shellcheck source=lib/airc_bash/platform_adapters.sh
  source "$CLONE_DIR/lib/airc_bash/platform_adapters.sh"
  # shellcheck source=lib/airc_bash/lib_daemon_detect.sh
  source "$CLONE_DIR/lib/airc_bash/lib_daemon_detect.sh"
else
  # Defensive fallback so install doesn't die on a weird CLONE_DIR layout.
  # The prompt block below tolerates the function being absent (treats
  # "unknown daemon state" as "not installed → offer prompt").
  airc_daemon_is_installed() { return 1; }
fi

# Order matters here. Four NON-prompt branches first, ordered so the
# loudest user intent wins:
#   1. AIRC_INSTALL_NO_DAEMON=1 — explicit opt-out trumps everything.
#   2. AIRC_INSTALL_YES=1       — explicit auto-install (Copilot #388:
#                                 must come BEFORE the non-TTY check
#                                 so `curl … | AIRC_INSTALL_YES=1 bash`
#                                 actually installs instead of falling
#                                 into the non-TTY tip branch).
#   3. daemon already installed  — idempotent re-run; nothing to do.
#   4. Non-TTY                   — no human to prompt; surface tip text.
#   5. TTY interactive prompt    — default path.
# Scope the daemon will end up wired to. Mirrors cmd_daemon.sh::_daemon_scope
# so the "is daemon installed for this scope?" check below matches what
# `airc daemon install` would actually create. b69f 2026-05-02 caught the
# scope-mismatch bug: any-daemon-registered → install.sh skipped → user
# left with no daemon for the scope they were bootstrapping.
INSTALL_DAEMON_SCOPE="${AIRC_HOME:-$(pwd -P)/.airc}"

if [ "${AIRC_INSTALL_NO_DAEMON:-0}" = "1" ]; then
  info "AIRC_INSTALL_NO_DAEMON=1 — skipping daemon install prompt"
elif [ "${AIRC_INSTALL_YES:-0}" = "1" ]; then
  if airc_daemon_is_installed_for_scope "$INSTALL_DAEMON_SCOPE"; then
    info "AIRC_INSTALL_YES=1 — airc daemon already installed for this scope (no-op)"
  else
    if airc_daemon_is_installed; then
      info "AIRC_INSTALL_YES=1 — daemon registered for a different scope; reinstalling for $INSTALL_DAEMON_SCOPE"
    else
      info "AIRC_INSTALL_YES=1 — installing airc daemon"
    fi
    if "$BIN_DIR/airc" daemon install; then
      ok "airc daemon installed"
    else
      warn "airc daemon install returned non-zero (continuing — re-run manually if needed)"
    fi
  fi
elif airc_daemon_is_installed_for_scope "$INSTALL_DAEMON_SCOPE"; then
  info "airc daemon already installed for this scope (skipping prompt)"
elif [ ! -t 0 ] || [ ! -t 1 ]; then
  # Non-TTY install can't prompt. Surface the option so the user sees it
  # in their install transcript and can run it later — the help string
  # mirrors the post-disconnect tip in airc's reconnect path.
  info "Tip: run 'airc daemon install' to keep the mesh alive across machine sleep/wake/crash"
else
  if airc_daemon_is_installed; then
    printf '\n  \033[1;32m==>\033[0m airc daemon is registered, but for a different scope.\n'
    printf '      Reinstall and wire it to %s?\n' "$INSTALL_DAEMON_SCOPE"
    printf '      Re-registers the launcher to point at this scope; safe to do.\n'
  else
    printf '\n  \033[1;32m==>\033[0m Install the airc background daemon?\n'
    printf '      Keeps the mesh alive across machine sleep/wake/crash without\n'
    printf '      requiring you to re-run `airc connect` after every wake. Adds\n'
    printf '      a launchd / systemd / HKCU-Run entry that auto-restarts the host.\n'
  fi
  printf '      Skip next time by setting AIRC_INSTALL_NO_DAEMON=1.\n'
  printf '      [Y/n] '
  read -r _daemon_reply || _daemon_reply=""
  case "${_daemon_reply}" in
    n|N|no|No|NO)
      info "Skipped daemon install. Run 'airc daemon install' later if you change your mind." ;;
    *)
      if "$BIN_DIR/airc" daemon install; then
        ok "airc daemon installed"
      else
        warn "airc daemon install returned non-zero — re-run manually:  airc daemon install"
      fi ;;
  esac
fi

# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  Next — open your agent:"
echo "    claude          # or codex, cursor, opencode, windsurf, openclaw, ..."
echo ""
echo "  Then, inside the agent:"
echo "    /join                          # auto-scopes to your project's room"
echo "    /msg @<peer> <message>         # DM (or omit @peer to broadcast)"
echo ""
echo "  Or run airc directly from this shell:"
echo "    airc join"
echo "    airc msg @<peer> <message>"
echo ""
echo "  Diagnose anytime:    airc doctor"
echo "  Repair if needed:    airc doctor --fix"
echo ""
