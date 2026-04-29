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

  # sshd: airc joiners ssh into the host's airc_home to tail messages.
  # Every airc user who'll host a room (which is most users — first to
  # discover becomes the host) needs sshd RUNNING. install.sh actually
  # turns it on instead of just warning, since "warn + leave it to the
  # user" was Joel's "this needs to be in the install dude" pushback
  # 2026-04-27. ONE sudo / UAC prompt during install (same shape as
  # install_with_pkgmgr already uses for apt/dnf/etc); after that
  # airc just works for hosting.
  #
  # AIRC_SKIP_SSHD=1 short-circuits the whole block — for headless CI
  # boxes that genuinely don't host, or environments that manage sshd
  # via their own config-management (Ansible, Chef).
  #
  # Auto-detect: GitHub Actions sets CI=true; so does almost every CI
  # system (Travis, CircleCI, GitLab, BuildKite, Jenkins). On macOS
  # specifically, the osascript admin-prompt path hangs forever in CI
  # because there's no Touch ID / password input — the runner job
  # silently runs for the full 6-hour timeout. Skip when CI=true so
  # the install completes cleanly and CI tests the rest of the path.
  # Post-3c: sshd setup + Tailscale install fully removed. Cross-network
  # messaging routes through gh-as-bearer (envelope-encrypted gist),
  # which works on every platform with `gh auth login` — no privileged
  # daemon, no sign-in popup, no admin elevation.

  # gh auth: required for the gist substrate. We CAN drive the login
  # interactively when stdin is a TTY (Joel 2026-04-29: 'thought that'd
  # be in setup or at least doctor (then claude could always do it for
  # them)'). The browser/device-code flow needs a real user to click
  # but doesn't need them to remember the command. Falls back to the
  # warning path for non-interactive installs (curl|bash piped without
  # a TTY, CI, etc).
  if command -v gh >/dev/null 2>&1; then
    if ! gh auth status >/dev/null 2>&1; then
      if [ -t 0 ] && [ -t 1 ]; then
        info "gh is not authenticated — launching 'gh auth login -s gist' now."
        info "  (Browser will open; sign in to GitHub. The 'gist' scope is required for the substrate.)"
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

# ── Skills into Claude Code ─────────────────────────────────────────────

if [ -d "$CLONE_DIR/skills" ]; then
  mkdir -p "$SKILLS_TARGET"

  # Clean up old symlinks from previous installs.
  # Includes the airc-classic skill names (connect/send/rename/disconnect) that
  # were renamed to IRC-canonical (join/msg/nick/quit) — leaving the old symlinks
  # in place would shadow the new skills with stale content. (`uninstall` was
  # previously listed here when the skill didn't exist; now that we ship a real
  # /uninstall skill, the per-skill symlink loop below recreates it cleanly and
  # this list omits it.)
  for old in "$SKILLS_TARGET"/relay-* "$SKILLS_TARGET"/monitor "$SKILLS_TARGET"/setup \
             "$SKILLS_TARGET"/connect "$SKILLS_TARGET"/send "$SKILLS_TARGET"/rename "$SKILLS_TARGET"/disconnect; do
    [ -L "$old" ] && rm "$old" 2>/dev/null
  done

  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$SKILLS_TARGET/$skill_name"
    # If the target is a real directory (from a pre-rename hand-install
    # or an old copy-based installer), it shadows the new symlink. Nuke it.
    if [ -d "$target" ] && [ ! -L "$target" ]; then
      rm -rf "$target"
    elif [ -L "$target" ]; then
      rm "$target"
    fi
    ln -sf "$skill_dir" "$target"
    ok "Skill: /$skill_name"
  done
fi


# ── Done ────────────────────────────────────────────────────────────────

echo ""
ok "Installed."
echo ""
echo "  Next:"
echo "    airc join                      # auto-#general (joins existing or hosts)"
echo "    airc msg @<peer> <message>     # DM (or omit @peer to broadcast)"
echo ""
echo "  Diagnose anytime:    airc doctor"
echo "  Repair if needed:    airc doctor --fix"
echo ""
