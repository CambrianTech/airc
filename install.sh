#!/usr/bin/env bash
#
# AIRC installer — DEV PATH ONLY (zero-friction doctrine,
# docs/ZERO-FRICTION-PATH.md): users get prebuilt signed binaries and
# never see this script or a compiler. This source-build path serves
# contributors, grid operators on unreleased branches, and CI; it moves
# behind --dev once the release pipeline lands.
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
#
# Clones the repo, puts the source `airc` on PATH, and installs AIRC skills.

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/airc.git"

_default_clone_dir() {
  local script="${BASH_SOURCE[0]:-}"
  local script_dir=""
  if [ -n "$script" ] && [ -f "$script" ]; then
    script_dir="$(cd "$(dirname "$script")" && pwd -P)"
    if [ -f "$script_dir/Cargo.toml" ] && [ -d "$script_dir/crates/airc-cli" ]; then
      printf '%s\n' "$script_dir"
      return 0
    fi
  fi
  printf '%s\n' "$HOME/.airc/src"
}

CLONE_DIR="${AIRC_DIR:-$(_default_clone_dir)}"
# BIN_DIR holds the installed Rust binary copied from CLONE_DIR.
# PATH points at this stable binary, not at mutable source-tree build
# artifacts and not at a shell wrapper.
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
SKILLS_TARGET="${SKILLS_TARGET:-$HOME/.claude/skills}"

info()  { printf '  \033[1;34m->\033[0m %s\n' "$*"; }
ok()    { printf '  \033[1;32m->\033[0m %s\n' "$*"; }
warn()  { printf '  \033[1;33m!\033[0m %s\n' "$*" >&2; }
fail()  { printf '  \033[1;31m✗\033[0m %s\n' "$*" >&2; exit 1; }

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
# Required: git, gh, ssh-keygen, cargo (to build the Rust substrate CLI).
# Optional: tailscale (only needed for cross-LAN mesh; LAN works without)
# Deliberately not required: openssl or Python. Identity, signing, hooks,
# config, and message parsing are Rust-owned.
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
    cargo)
      case "$mgr" in
        brew)   echo "rust" ;;
        apt)    echo "cargo" ;;
        dnf)    echo "cargo" ;;
        pacman) echo "rust" ;;
        apk)    echo "cargo" ;;
        winget) echo "Rustlang.Rustup" ;;
        *)      echo "cargo" ;;
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



# Windows (Git Bash / MSYS / Cygwin): rustup's default target is
# x86_64-pc-windows-msvc, which CANNOT LINK without the Visual Studio C++
# build tools — `cargo build` dies with a per-crate wall of
# `error: linking with link.exe failed: exit code: 1` and no guidance.
# Validated live on a fresh Windows 11 box (2026-06-10). The windows-gnu
# toolchain is NOT a viable fallback for airc: windows-sys raw-dylib
# import libs hit the upstream bundled-dlltool bug (rust-lang/rust#103939)
# and `ring` needs a real C compiler regardless. So when we're on Windows
# with winget available, probe for the VC.Tools component via vswhere and
# auto-install the (license-free) VS 2022 Build Tools C++ workload.
_ensure_windows_msvc_toolchain() {
  local mgr="$1"
  case "$(uname -s 2>/dev/null)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *) return 0 ;;
  esac

  # %ProgramFiles(x86)% can't be read as a bash variable (parens are
  # invalid in names) — ask cmd for it, fall back to the standard path.
  local pf86
  pf86=$(cmd //c 'echo %ProgramFiles(x86)%' 2>/dev/null | tr -d '\r')
  [ -z "$pf86" ] || [ "$pf86" = '%ProgramFiles(x86)%' ] && pf86='C:\Program Files (x86)'
  local vswhere
  vswhere="$(_to_bash_path "$pf86")/Microsoft Visual Studio/Installer/vswhere.exe"
  [ -x "$vswhere" ] || vswhere=""

  if [ -n "$vswhere" ]; then
    local vs_path
    vs_path=$("$vswhere" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>/dev/null || true)
    if [ -n "$vs_path" ]; then
      ok "MSVC C++ build tools already installed"
      return 0
    fi
  fi

  if [ "$mgr" != "winget" ]; then
    warn "MSVC C++ build tools not found and winget unavailable — cargo cannot link on Windows without them."
    warn "  Install 'Visual Studio 2022 Build Tools' with the 'Desktop development with C++' workload, then re-run."
    return 0
  fi

  info "Installing Visual Studio 2022 Build Tools + C++ workload (required to link Rust on Windows; ~2 GB, several minutes)..."
  local wbin; wbin=$(command -v winget.exe 2>/dev/null || command -v winget 2>/dev/null || true)
  [ -z "$wbin" ] && return 0
  "$wbin" install --id Microsoft.VisualStudio.2022.BuildTools --exact --silent \
    --accept-source-agreements --accept-package-agreements --disable-interactivity \
    --override "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" \
    || warn "winget BuildTools install returned non-zero; probing anyway"

  # Re-probe: vswhere ships with Build Tools, so it exists now if the
  # install worked even when it didn't exist before. Reuse the resolved
  # pf86 (not a hardcoded /c/...) so Cygwin (/cygdrive/c) and relocated
  # ProgramFiles(x86) do not false-fail after a successful install.
  vswhere="$(_to_bash_path "$pf86")/Microsoft Visual Studio/Installer/vswhere.exe"
  if [ -x "$vswhere" ] && [ -n "$("$vswhere" -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>/dev/null)" ]; then
    ok "MSVC C++ build tools installed"
  else
    # Most likely: a VS/BuildTools product exists but lacks the C++
    # workload, and winget won't modify an existing install.
    fail "MSVC C++ build tools still missing after install attempt.
       Open 'Visual Studio Installer' -> Modify -> check 'Desktop development with C++' -> Install.
       Then re-run install.sh. (Without it, cargo cannot link on Windows.)"
  fi
}

# macOS: cargo needs a working C toolchain to link (ring, candle, etc.). Two
# failure modes seen on fresh/changed Macs:
#   1. No Command Line Tools at all -> `xcode-select --install`.
#   2. A full Xcode.app is the ACTIVE toolchain but its license isn't
#      accepted -> cc fails with "You have not agreed to the Xcode license"
#      and EVERY cargo + git op dies. Fix: switch to the (license-free) CLT,
#      or accept the license.
# We TELL the user before each privileged step (the sudo password prompt is
# the "press enter to proceed"); never silently mutate their toolchain.
_ensure_macos_build_toolchain() {
  [ "$(uname -s 2>/dev/null)" = "Darwin" ] || return 0

  # The real test: can cc actually compile + link? (Presence of xcode-select
  # is not enough — the Xcode-license case has cc present but refusing.)
  local probe rc
  probe=$(printf 'int main(void){return 0;}' | cc -x c - -o /tmp/.airc-cc-probe 2>&1); rc=$?
  rm -f /tmp/.airc-cc-probe 2>/dev/null
  if [ "$rc" -eq 0 ]; then ok "macOS C toolchain OK (cc links)"; return 0; fi

  local clt="/Library/Developer/CommandLineTools"
  if printf '%s' "$probe" | grep -qi "agreed to the Xcode license"; then
    warn "macOS C toolchain blocked: the active Xcode license isn't accepted — cargo + git can't run."
    if [ -d "$clt" ]; then
      info "Switching the active toolchain to the license-free Command Line Tools (asks for your password):"
      info "  sudo xcode-select --switch $clt"
      if sudo xcode-select --switch "$clt"; then ok "Toolchain switched to Command Line Tools — unblocked."
      else fail "Run: sudo xcode-select --switch $clt   (or: sudo xcodebuild -license accept), then re-run install.sh."; fi
    else
      info "Accepting the Xcode license (asks for your password):"
      info "  sudo xcodebuild -license accept"
      if sudo xcodebuild -license accept; then ok "Xcode license accepted — unblocked."
      else fail "Run: sudo xcodebuild -license accept, then re-run install.sh."; fi
    fi
  elif ! xcode-select -p >/dev/null 2>&1; then
    info "Installing the Command Line Tools (cargo needs a C compiler) — accept the macOS dialog, then re-run:"
    info "  xcode-select --install"
    xcode-select --install 2>/dev/null || true
    fail "Command Line Tools installing — finish the macOS dialog, then re-run install.sh."
  else
    fail "macOS C toolchain can't compile. cc said:
$probe
Try: xcode-select --install   OR   sudo xcode-select --switch $clt"
  fi
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
      warn "Required prereqs: git, gh, cargo"
      return 0
    fi
  fi

  local missing=() pkgs=() unmappable=()
  # jq, openssl, and Python are not install prereqs. JSON handling,
  # Ed25519 identity/signing, hooks, and config mutation are Rust-owned.
  for cmd in git gh ssh-keygen cargo; do
    # Strict probe: presence on PATH AND a successful --version invocation.
    # git/gh/cargo support --version cleanly. ssh-keygen does NOT have a version
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

  # Session PATH refresh after a winget rustup install. rustup-init puts
  # %USERPROFILE%\.cargo\bin on the *registry* User PATH, but this Git
  # Bash session inherited its PATH at launch — without this export the
  # script auto-installs Rust and then immediately dies at
  # _install_airc_binary's "cargo is required" probe. Caught live on a
  # fresh Windows 11 box, 2026-06-10.
  if ! command -v cargo >/dev/null 2>&1; then
    if [ -x "$HOME/.cargo/bin/cargo" ] || [ -x "$HOME/.cargo/bin/cargo.exe" ]; then
      export PATH="$HOME/.cargo/bin:$PATH"
      ok "Added ~/.cargo/bin to this session's PATH (rustup install is brand-new)"
    fi
  fi

  _ensure_windows_msvc_toolchain "$mgr"
  _ensure_macos_build_toolchain

  # Issue #341 follow-up: openssl/Python crypto bootstrap removed.
  # Identity gen + signing live in Rust, so system OpenSSL and Python
  # package state are irrelevant to airc install correctness.

  # Post-3c: sshd setup + Tailscale install fully removed from install.
  # Cross-network messaging is owned by Rust transports/discovery, while
  # GitHub remains rendezvous/control-plane only. The earlier sshd-on-
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

  # Git author identity. Agents in this substrate commit and open PRs,
  # and a fresh machine has no global user.name/user.email — the first
  # commit then dies with "Author identity unknown" (caught live on a
  # clean Windows box 2026-06-13). Derive it from the authenticated gh
  # account when unset; never clobber an identity the user already set.
  # Email prefers the account's public email, falling back to the GitHub
  # noreply alias (<id>+<login>@users.noreply.github.com), which always
  # matches the account and avoids leaking a private address.
  if command -v gh >/dev/null 2>&1 && command -v git >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    _need_name=0; _need_email=0
    if [ -z "$(git config --global user.name 2>/dev/null || true)" ]; then _need_name=1; fi
    if [ -z "$(git config --global user.email 2>/dev/null || true)" ]; then _need_email=1; fi
    if [ "$_need_name" = 1 ] || [ "$_need_email" = 1 ]; then
      _gh_login="$(gh api user --jq '.login' 2>/dev/null || true)"
      _gh_name="$(gh api user --jq '.name // .login' 2>/dev/null || true)"
      _gh_id="$(gh api user --jq '.id' 2>/dev/null || true)"
      _gh_email="$(gh api user --jq '.email // empty' 2>/dev/null || true)"
      if [ -z "$_gh_email" ] && [ -n "$_gh_id" ] && [ -n "$_gh_login" ]; then
        _gh_email="${_gh_id}+${_gh_login}@users.noreply.github.com"
      fi
      if [ "$_need_name" = 1 ] && [ -n "$_gh_name" ]; then
        git config --global user.name "$_gh_name"
        info "git user.name set from gh: $_gh_name (override: git config --global user.name ...)"
      fi
      if [ "$_need_email" = 1 ] && [ -n "$_gh_email" ]; then
        git config --global user.email "$_gh_email"
        info "git user.email set from gh: $_gh_email (override: git config --global user.email ...)"
      fi
    fi
  fi
}

ensure_prereqs

# ── Clone or update ─────────────────────────────────────────────────────

if [ -d "$CLONE_DIR/.git" ] || [ -f "$CLONE_DIR/.git" ]; then
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
  # Channel = the checkout's CURRENT BRANCH. git is the state manager:
  # a canary user is simply on the `canary` branch; switching channels is
  # `git checkout <branch>` in this repo — not a baked-in default and not
  # a side-channel `.channel` file. We fast-forward whatever branch is
  # checked out and NEVER silently switch to main (the old auto-switch
  # clobbered feature/channel branches — that's why AIRC_INSTALL_NO_PULL
  # above exists as the as-is escape; respecting the branch removes the
  # footgun at the source).
  CURRENT_BRANCH=$(git -C "$CLONE_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
  if [ -z "$CURRENT_BRANCH" ] || [ "$CURRENT_BRANCH" = "HEAD" ]; then
    cat >&2 <<EOF
ERROR: $CLONE_DIR is in detached HEAD — check out a channel branch first:
  cd $CLONE_DIR && git checkout <branch> && bash install.sh
EOF
    exit 1
  fi
  info "Channel = current branch '$CURRENT_BRANCH'"
  git -C "$CLONE_DIR" fetch --quiet origin "$CURRENT_BRANCH" || {
    echo "ERROR: Couldn't fetch origin/$CURRENT_BRANCH. Network? gh auth?" >&2
    exit 1
  }
  if ! git -C "$CLONE_DIR" pull --ff-only --quiet 2>&1; then
    cat >&2 <<EOF
ERROR: Couldn't fast-forward $CLONE_DIR on '$CURRENT_BRANCH'.
Likely cause: local edits or a divergent history.
Recover with:
  cd $CLONE_DIR
  git status
  git stash               # if you have local edits worth keeping
  git fetch origin
  git reset --hard origin/$CURRENT_BRANCH
  bash install.sh
EOF
    exit 1
  fi
  fi  # AIRC_INSTALL_NO_PULL guard
else
  # First install. The channel is just a git branch — git is the state
  # manager, so there is NO hardcoded "main" and NO main|canary allowlist:
  #   - AIRC_CHANNEL=<branch> clones that branch (any branch, e.g. canary),
  #   - unset → clone the remote's DEFAULT branch (git picks it; when the
  #     team flips the default, fresh installs follow without a code edit).
  # Future updates then track whatever branch is checked out (see the
  # update path above), and switching channels is `git checkout <branch>`.
  if [ -n "${AIRC_CHANNEL:-}" ]; then
    info "Installing AIRC (channel/branch: $AIRC_CHANNEL)"
    git clone --quiet --branch "$AIRC_CHANNEL" "$REPO_URL" "$CLONE_DIR" || {
      echo "ERROR: Couldn't clone branch '$AIRC_CHANNEL' from $REPO_URL." >&2
      echo "  Check the branch name (AIRC_CHANNEL), network, and gh auth." >&2
      exit 1
    }
  else
    info "Installing AIRC (remote default branch)"
    git clone --quiet "$REPO_URL" "$CLONE_DIR" || {
      echo "ERROR: Couldn't clone $REPO_URL. Network? gh auth?" >&2
      exit 1
    }
  fi
fi

# ── airc on PATH ───────────────────────────────────────────────────────

_add_path_entry() {
  local path_entry="$1"
  local rc rc_target=""
  # Always reconcile the rc file regardless of current-shell PATH
  # state. The rc is persistent; $PATH is transient and may have been
  # manually augmented by the operator in this shell. Conflating them
  # (early-return when PATH already contains the entry) was the bug
  # that left stale airc-managed PATH lines in ~/.zshrc after install
  # re-runs — caught live 2026-05-20.
  #
  # Pass 1: reconcile EVERY rc that already exists (strip stale
  # airc-managed lines so reruns are idempotent and entries pointing at
  # retired install dirs don't accumulate), and remember the first
  # existing one as the write target.
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    [ -f "$rc" ] || continue
    # Strip any pre-existing airc-managed PATH lines (marked with the
    # trailing `# airc` comment). We only touch lines we ourselves
    # wrote; user-managed PATH lines stay put.
    if grep -qE '^export PATH=.*# airc$' "$rc"; then
      local tmp; tmp="$(mktemp "${rc}.airc.XXXXXX")"
      # `|| true` is load-bearing: when the rc contains ONLY airc-managed
      # lines (e.g. a freshly-created ~/.bashrc holding just our PATH
      # entry), grep -v emits nothing and exits 1, which would skip the
      # mv under `&&` and leave the stale line in place — duplicating it
      # on the next append. Decouple the mv from grep's exit code.
      grep -vE '^export PATH=.*# airc$' "$rc" > "$tmp" || true
      mv "$tmp" "$rc"
    fi
    [ -z "$rc_target" ] && rc_target="$rc"
  done
  # No rc file existed at all. A fresh Git Bash / MSYS box ships NONE of
  # ~/.bashrc / ~/.bash_profile / ~/.profile, so the old "[ -f ] ||
  # continue" loop wrote the PATH line to NOTHING and `airc` ended up on
  # PATH in no shell — current OR future. Caught live on a clean Windows
  # Git Bash box 2026-06-13. Create the rc that matches the user's
  # shell so the entry has a home.
  if [ -z "$rc_target" ]; then
    case "${SHELL:-}" in
      *zsh) rc_target="$HOME/.zshrc" ;;
      *)    rc_target="$HOME/.bashrc" ;;
    esac
    : > "$rc_target"
    info "Created $(basename "$rc_target") (no shell rc existed) for the airc PATH entry"
    # Login bash (Git Bash launches `bash --login -i`) sources
    # ~/.bash_profile / ~/.bash_login / ~/.profile, NOT ~/.bashrc, unless
    # told to. Without this shim the freshly-created ~/.bashrc is never
    # read by a login shell and the PATH entry is dead on arrival. Only
    # create the shim when no login file already exists (don't clobber a
    # user's own ~/.bash_profile).
    if [ "$rc_target" = "$HOME/.bashrc" ] \
       && [ ! -f "$HOME/.bash_profile" ] \
       && [ ! -f "$HOME/.bash_login" ] \
       && [ ! -f "$HOME/.profile" ]; then
      printf '# created by airc installer: load ~/.bashrc in login shells\nif [ -f ~/.bashrc ]; then . ~/.bashrc; fi\n' > "$HOME/.bash_profile"
      info "Created ~/.bash_profile to source ~/.bashrc in login shells"
    fi
  fi
  # Ensure rc ends with a newline so the append starts on its own
  # line. Without this, an rc that ends with `alias foo="bar"` (no
  # trailing \n) produces the broken line
  # `alias foo="bar"export PATH="...:$PATH"  # airc`
  # which zsh parses as one malformed alias declaration and never
  # sets PATH. Caught live 2026-05-20 — Joel's `airc` resolved fine
  # from a manually-augmented PATH but new shells got nothing.
  if [ -s "$rc_target" ] && [ "$(tail -c1 "$rc_target")" != "$(printf '\n')" ]; then
    printf '\n' >> "$rc_target"
  fi
  printf 'export PATH="%s:$PATH"  # airc\n' "$path_entry" >> "$rc_target"
  ok "Added $path_entry to PATH in $(basename "$rc_target")"
  # Only export into current env if not already there. Avoids
  # gratuitously prepending duplicate PATH segments when the operator
  # has already sourced their rc.
  if ! echo "$PATH" | tr ':' '\n' | grep -qx "$path_entry"; then
    export PATH="$path_entry:$PATH"
  fi
}

# Minimum cargo version that can build this tree. Tracks Cargo.lock's
# lockfile format: lockfile v4 (current) requires Cargo >= 1.78. Bump
# this alongside any future Cargo.lock format bump or declared MSRV.
AIRC_MIN_CARGO="${AIRC_MIN_CARGO:-1.78.0}"

# _version_ge A B -> success (0) iff version A >= version B. Uses sort -V
# (version sort) so 1.70.0 vs 1.78.0 compares numerically, not lexically.
_version_ge() {
  [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]
}

# Cargo present-but-too-old is a real first-time-user failure mode: a
# machine with an old rustup default (e.g. 1.70 from 2023) passes the
# `cargo --version` prereq probe, then `cargo build` dies with the
# cryptic "lock file version 4 was found, but this version of Cargo does
# not understand this lock file" and `set -e` aborts the whole install
# with no guidance. ensure_prereqs only checks presence, not version, so
# this gate + auto-recovery lives right before the build. Recovery order:
# rustup (the common case) -> brew -> clear manual instructions.
ensure_cargo_recent() {
  command -v cargo >/dev/null 2>&1 || return 0  # absence handled by _install_airc_binary
  local have
  have="$(cargo --version 2>/dev/null | awk '{print $2}')"
  if [ -n "$have" ] && _version_ge "$have" "$AIRC_MIN_CARGO"; then
    return 0
  fi
  warn "cargo ${have:-unknown} is too old to build airc (need >= $AIRC_MIN_CARGO; Cargo.lock is lockfile v4)."

  if command -v rustup >/dev/null 2>&1; then
    info "Updating the Rust toolchain via rustup (rustup update stable)..."
    if rustup update stable; then
      # Make sure the just-updated stable is what cargo resolves to. If a
      # different toolchain is the rustup default, the shim still serves
      # the old cargo; only switch when no per-directory override pins it.
      rustup default stable >/dev/null 2>&1 || true
      have="$(cargo --version 2>/dev/null | awk '{print $2}')"
      if [ -n "$have" ] && _version_ge "$have" "$AIRC_MIN_CARGO"; then
        ok "Rust toolchain updated to cargo $have"
        return 0
      fi
      warn "rustup update ran but cargo is still ${have:-unknown} (a rust-toolchain override may be pinning an old version)."
    else
      warn "rustup update stable failed."
    fi
  fi

  # brew-managed rust (cargo from Homebrew rather than rustup).
  if command -v brew >/dev/null 2>&1 && brew list rust >/dev/null 2>&1; then
    info "Upgrading Homebrew rust (brew upgrade rust)..."
    if brew upgrade rust 2>/dev/null || true; then
      have="$(cargo --version 2>/dev/null | awk '{print $2}')"
      if [ -n "$have" ] && _version_ge "$have" "$AIRC_MIN_CARGO"; then
        ok "Rust toolchain updated to cargo $have"
        return 0
      fi
    fi
  fi

  fail "Could not obtain cargo >= $AIRC_MIN_CARGO (have ${have:-unknown}). Update Rust and re-run install.sh:
         rustup:    rustup update stable
         Homebrew:  brew upgrade rust
         no rustup: install from https://rustup.rs"
}

# Build the Rust binary and place it on PATH as `airc`. No shell
# wrapper, no .shim/.cmd/.ps1 trampolines — the Rust binary is the
# Resolve cargo's ACTUAL target directory rather than assuming
# "$CLONE_DIR/target". Cargo honors `CARGO_TARGET_DIR` AND a `[build]
# target-dir` in `~/.cargo/config.toml` / `.cargo/config.toml`, which a
# shared-build-cache setup commonly redirects (disk discipline). When it
# does, the binary lands outside `$CLONE_DIR/target`, and the old
# hard-coded path made `airc update` build successfully then "lose" the
# binary and bail. `cargo metadata` reports the resolved dir (same
# precedence the build used), so we always look where the binary actually
# is. JSON backslash-escapes / Windows backslashes are normalized to
# forward slashes so the path is usable in this (MSYS/git-bash) shell;
# falls back to the historical default if `cargo metadata` is unavailable.
_airc_target_dir() {
  local dir
  dir="$( (cd "$CLONE_DIR" && cargo metadata --format-version 1 --no-deps 2>/dev/null) \
          | grep -o '"target_directory":"[^"]*"' | head -1 \
          | sed 's/^"target_directory":"//; s/"$//' )"
  dir="$(printf '%s' "$dir" | sed 's#\\\\#/#g; s#\\#/#g')"
  if [ -n "$dir" ]; then printf '%s\n' "$dir"; else printf '%s\n' "$CLONE_DIR/target"; fi
}

# user surface. Copy the built binary into BIN_DIR so PATH never points
# at mutable target artifacts inside the source checkout.
# Windows-only: make airc reachable INBOUND without code-signing. Defender
# blocks unknown programs' inbound by default, and repeated `airc daemon`
# binds leave contradictory auto-created rules (a Block beats an Allow), so
# inbound silently dies. We install ONE canonical inbound-allow rule for the
# binary. Changing firewall rules requires elevation (signing wouldn't help —
# firewall != SmartScreen), so we CHECK first (read-only, no prompt) and only
# UAC-prompt when a fix is actually needed — every later update stays silent.
_setup_windows_firewall() {
  local ps1="$CLONE_DIR/windows/firewall-allow.ps1"
  [ -f "$ps1" ] || return 0   # tolerate older checkouts
  local airc_win ps1_win
  airc_win="$(_to_win_path "$BIN_DIR/airc.exe")"
  ps1_win="$(_to_win_path "$ps1")"
  # Read-only state check — no admin, no prompt.
  if powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
       -File "$ps1_win" -AircPath "$airc_win" -CheckOnly >/dev/null 2>&1; then
    ok "Windows Firewall: airc inbound already allowed"
    return 0
  fi
  info "Windows Firewall: allowing airc inbound (one UAC prompt — so LAN peers can reach this node)…"
  powershell.exe -NoProfile -Command \
    "Start-Process powershell -Verb RunAs -Wait -ArgumentList @('-NoProfile','-ExecutionPolicy','Bypass','-File','$ps1_win','-AircPath','$airc_win')" \
    >/dev/null 2>&1 || true
  if powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass \
       -File "$ps1_win" -AircPath "$airc_win" -CheckOnly >/dev/null 2>&1; then
    ok "Windows Firewall: airc inbound allowed"
  else
    warn "Windows Firewall rule not set (elevation declined?). airc inbound may be blocked. \
Re-run setup, or as admin: powershell -ExecutionPolicy Bypass -File '$ps1_win' -AircPath '$airc_win'"
  fi
}

_install_airc_binary() {
  [ "${AIRC_SKIP_RUST_BUILD:-0}" = "1" ] && { info "AIRC_SKIP_RUST_BUILD=1 -- skipping airc build"; return 0; }
  if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is required to build airc. Install Rust, then re-run install.sh."
  fi
  ensure_cargo_recent
  info "Building Rust CLI: airc"
  (cd "$CLONE_DIR" && cargo build --release -p airc-cli)
  mkdir -p "$BIN_DIR"

  # Where cargo ACTUALLY put it (honors CARGO_TARGET_DIR + cargo config),
  # not the assumed "$CLONE_DIR/target" — see `_airc_target_dir`.
  local target_dir; target_dir="$(_airc_target_dir)"
  case "$(uname -s 2>/dev/null)" in
    MINGW*|MSYS*|CYGWIN*)
      local built="$target_dir/release/airc.exe"
      [ -x "$built" ] || fail "airc build completed but binary is missing: $built (target dir: $target_dir)"
      cp -f "$built" "$BIN_DIR/airc.exe"
      ok "Installed airc: $BIN_DIR/airc.exe"
      # Reachable-inbound on a typical Windows box (idempotent; prompts for
      # elevation only when the firewall rule is missing/broken).
      _setup_windows_firewall
      ;;
    *)
      local built="$target_dir/release/airc"
      [ -x "$built" ] || fail "airc build completed but binary is missing: $built (target dir: $target_dir)"
      local tmp="$BIN_DIR/.airc.tmp.$$"
      cp -f "$built" "$tmp"
      chmod +x "$tmp"
      mv -f "$tmp" "$BIN_DIR/airc"
      ok "Installed airc: $BIN_DIR/airc"
      ;;
  esac

  # Reap legacy install-shape leftovers (bash wrapper, airc-core
  # binary, Windows trampolines). Reruns of install.sh on a machine
  # that previously had the wrapper-era install converge cleanly to
  # the redesigned layout.
  for stale in "$BIN_DIR/airc-core" "$BIN_DIR/airc-core.exe" "$BIN_DIR/airc.cmd" "$BIN_DIR/airc.ps1"; do
    if [ -L "$stale" ] || [ -e "$stale" ]; then
      rm -f "$stale"
      ok "Removed legacy install artifact: $stale"
    fi
  done

  _add_path_entry "$BIN_DIR"
}

_install_airc_binary

# ── Record the install source for `airc update` ────────────────────────
# install.sh's _default_clone_dir installs FROM a dev checkout (the cwd)
# when run inside one, and rust-rewrite currently ships ONLY as a dev
# checkout (no release channel yet) — so the source is frequently NOT
# ~/.airc/src. The Rust `airc update` reads this marker to find the
# source; without it, update died with "No git checkout at ~/.airc/src"
# for every dev-checkout install (caught live 2026-06-13). The native
# (non-MSYS) path form is REQUIRED: the airc binary is native and its
# std::fs / git -C cannot resolve a `/c/...` MSYS path on Windows.
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*) _install_source="$(_to_win_path "$CLONE_DIR")" ;;
  *)                    _install_source="$CLONE_DIR" ;;
esac
mkdir -p "$HOME/.airc"
printf '%s\n' "$_install_source" > "$HOME/.airc/install-source"
ok "Recorded install source for 'airc update': $_install_source"

# ── Skills into agent skill dirs (Claude Code + Codex) ─────────────────
#
# Both Claude Code and OpenAI Codex use the same on-disk skill format:
# a directory per skill, with a SKILL.md inside (YAML frontmatter +
# markdown body). They differ only in WHERE they look:
#   Claude Code → ~/.claude/skills/<name>/
#   Codex       → ~/.codex/skills/<name>/
#
# We copy airc's skills into both whenever the corresponding agent is
# installed on the machine. Copies avoid symlink resolution differences
# between shells, agents, Windows Git Bash, and future source checkouts.
# Each copied skill carries a marker so uninstall can remove only
# airc-owned skill directories.

_install_airc_skills_into() {
  local skills_target="$1" agent_label="$2"
  [ -d "$CLONE_DIR/skills" ] || return 0
  mkdir -p "$skills_target"

  local skill_dir skill_name target
  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    [ -f "$skill_dir/SKILL.md" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$skills_target/$skill_name"
    if [ -e "$target" ] || [ -L "$target" ]; then
      rm -rf "$target"
    fi
    mkdir -p "$target"
    cp -R "$skill_dir"/. "$target"/
    printf 'installed-by=airc\nsource=%s\n' "$skill_dir" > "$target/.airc-skill"
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
# skill install but NOT the config write.

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
  # to ~/.airc/src/ + ~/.airc/ + a :project_roots
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

  # Cleanup for managed [permissions.airc.filesystem] blocks lives in the
  # Rust Codex hook installer below. Keep this profile function focused
  # on the network profile it owns.

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

# ── Codex model-visible AIRC turn contract ─────────────────────────────
# Codex currently has no Claude-style Monitor tool. Keep `airc join`
# running as the live feed, and install a small model-visible
# instruction so future Codex sessions distinguish the live stream from
# the prompt-boundary hook catch-up.
#
# We only write this if the user has no existing developer_instructions
# key. If they do, do not overwrite; they can copy the block manually or
# opt into a profile. This keeps install.sh from clobbering unrelated
# Codex configuration.

_install_airc_codex_developer_instructions() {
  local config="$HOME/.codex/config.toml"
  [ "${AIRC_SKIP_CODEX_INSTRUCTIONS:-0}" = "1" ] && return 0
  [ -f "$config" ] || return 0

  if grep -qF 'AIRC-CODEX-INSTRUCTIONS-START' "$config" 2>/dev/null; then
    local _tmp; _tmp=$(mktemp)
    sed '/^# AIRC-CODEX-INSTRUCTIONS-START/,/^# AIRC-CODEX-INSTRUCTIONS-END/d' "$config" > "$_tmp"
    mv "$_tmp" "$config"
  fi

  if grep -qE '^[[:space:]]*developer_instructions[[:space:]]*=' "$config" 2>/dev/null; then
    info "  ~/.codex/config.toml already has developer_instructions; skipping airc Codex turn contract"
    return 0
  fi

  local _tmp; _tmp=$(mktemp)
  {
    cat <<'TOML'
# AIRC-CODEX-INSTRUCTIONS-START — managed by install.sh; remove this section through AIRC-CODEX-INSTRUCTIONS-END to opt out
developer_instructions = """
AIRC Codex runtime contract:
- Keep `airc join` running as this session's live AIRC feed when coordinating with peer agents.
- If no live join session id is available during a turn, run `airc codex-hook poll --wait-ms 1000` between tool steps. It is the bounded mid-turn feed and shares the same runtime cursor as the hook.
- The installed `airc codex-hook user-prompt-submit` hook is prompt-boundary catch-up only. Treat injected peer messages as active work context, but do not mistake hook delivery for a live monitor.
- Reply to direct peer questions with `airc msg`, not user-chat stdout. The peer sees AIRC, not this transcript.
- Do not claim AIRC collaboration is healthy from process health alone. Distinguish transport/process liveness from whether this Codex session has actually seen peer traffic.
"""
# AIRC-CODEX-INSTRUCTIONS-END

TOML
    cat "$config"
  } > "$_tmp"
  mv "$_tmp" "$config"
  ok "Added Codex AIRC turn contract to ~/.codex/config.toml — restart Codex to activate AIRC coordination guidance"
}

if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_codex_developer_instructions
fi

# ── Codex lifecycle hook for local AIRC inbox injection ────────────────
# Newer Codex CLI builds can run deterministic hooks at turn boundaries.
# Install a UserPromptSubmit hook so unread airc messages become
# developer context before each user prompt reaches the model. This is
# the closest Codex equivalent to Claude Code's Monitor notifications:
# no GitHub calls, no log-tail polling in the model, and no dependence
# on the agent remembering the turn contract.

_install_airc_codex_hooks() {
  [ "${AIRC_SKIP_CODEX_HOOKS:-0}" = "1" ] && return 0
  [ -f "$HOME/.codex/config.toml" ] || return 0

  local _airc=""
  local _tdir; _tdir="$(_airc_target_dir)"
  if [ -x "$_tdir/release/airc" ]; then
    _airc="$_tdir/release/airc"
  elif [ -x "$_tdir/debug/airc" ]; then
    _airc="$_tdir/debug/airc"
  elif command -v airc >/dev/null 2>&1; then
    _airc=$(command -v airc)
  else
    warn "Could not install Codex AIRC hook: airc binary not found"
    return 0
  fi

  local out
  if out=$("$_airc" codex-hook install-hooks --codex-home "$HOME/.codex" 2>&1); then
    if [ -n "$out" ]; then
      printf '%s\n' "$out" | while IFS= read -r line; do
        ok "Codex AIRC hook: $line"
      done
    fi
  else
    warn "Could not install Codex AIRC hook: $out"
  fi
}

if command -v codex >/dev/null 2>&1 && [ -d "$HOME/.codex" ]; then
  _install_airc_codex_hooks
fi

# ── Git fetch-before-commit/push staleness guard (card 64621946) ───────
# Wire the repo's own dev workflow with a pre-commit (advisory) + pre-push
# (may hard-gate) hook that fetches the integration base and warns/blocks
# when the local base is stale. This is what stops tonight's hazard:
# branches cut off a 5-commits-behind rust-rewrite → E0063 between slices.
#
# Composition contract (CBAR Extension Bar — extend, don't clobber):
# if $CLONE_DIR already has a pre-commit / pre-push hook that is NOT
# ours, we preserve it: the original is moved to <hook>.local and our
# wrapper chains to it after running the guard. Re-running install
# rewrites only the airc-owned wrapper (marker-gated), so it is fully
# idempotent. Installs ONLY into the airc clone's own .git/hooks — this
# is a dev-tree guard, not a product-surface change for end users.

_install_airc_git_hooks() {
  [ "${AIRC_SKIP_GIT_HOOKS:-0}" = "1" ] && return 0
  local hooks_dir="$CLONE_DIR/.git/hooks"
  local worker="$CLONE_DIR/integrations/git-hooks/airc-fetch-base.sh"
  # core.hooksPath override (e.g. when the repo points hooks elsewhere).
  local hp
  hp="$(git -C "$CLONE_DIR" config --get core.hooksPath 2>/dev/null || true)"
  if [ -n "$hp" ]; then
    case "$hp" in
      /*|[A-Za-z]:*) hooks_dir="$hp" ;;
      *) hooks_dir="$CLONE_DIR/$hp" ;;
    esac
  fi
  [ -d "$CLONE_DIR/.git" ] || { return 0; }   # not a clone (curl-piped src tree edge case)
  [ -f "$worker" ] || { warn "Git hook worker not found: $worker"; return 0; }
  mkdir -p "$hooks_dir" 2>/dev/null || { warn "Could not create $hooks_dir"; return 0; }
  chmod +x "$worker" 2>/dev/null || true

  local phase hook tmp marker="# AIRC-FETCH-HOOK"
  for phase in pre-commit pre-push; do
    hook="$hooks_dir/$phase"
    # If a foreign (non-airc) hook is already here, preserve it as .local
    # so our wrapper can chain to it. Don't re-stash our own wrapper.
    if [ -f "$hook" ] && ! grep -qF "$marker" "$hook" 2>/dev/null; then
      if [ ! -f "$hook.local" ]; then
        mv "$hook" "$hook.local"
        chmod +x "$hook.local" 2>/dev/null || true
        info "Preserved existing $phase hook as $phase.local (chained)"
      else
        # A .local already exists; drop the un-marked file rather than clobber it.
        rm -f "$hook"
      fi
    fi
    tmp="$hook.airc-tmp.$$"
    {
      printf '%s\n' "#!/usr/bin/env bash"
      printf '%s %s\n' "$marker" "— managed by airc install.sh (card 64621946); do not edit."
      printf '%s\n' "# Runs the fetch-before-commit/push staleness guard, then chains any"
      printf '%s\n' "# pre-existing hook preserved as this file + .local."
      printf '%s\n' "set -u"
      printf 'WORKER=%q\n' "$worker"
      printf 'LOCAL="${BASH_SOURCE[0]}.local"\n'
      printf '%s\n' "# Guard runs first. pre-push may exit non-zero (hard-gate when behind)."
      printf 'if [ -x "$WORKER" ] || [ -f "$WORKER" ]; then\n'
      printf '  bash "$WORKER" %q "$@" || exit $?\n' "$phase"
      printf 'fi\n'
      printf '%s\n' "# Chain a preserved local hook, forwarding stdin (pre-push gets refs on stdin)."
      printf 'if [ -x "$LOCAL" ]; then exec "$LOCAL" "$@"; fi\n'
      printf 'exit 0\n'
    } > "$tmp"
    mv "$tmp" "$hook"
    chmod +x "$hook" 2>/dev/null || true
    ok "Git hook installed: $phase (fetch-before-$phase staleness guard)"
  done
}

_install_airc_git_hooks

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


# ── Optional background daemon ─────────────────────────────────────────
#
# Deliberately not installed or prompted from install.sh. The public
# product surface is `airc join`; the daemon is only an explicit
# supervisor for unattended machines that need `airc join` restarted at
# login/sleep/wake. Keeping curl/install side-effect-light avoids macOS
# Login Items surprises and keeps first-run setup easy to trust.
if [ "${AIRC_INSTALL_YES:-0}" = "1" ]; then
  info "AIRC_INSTALL_YES=1 no longer installs the daemon automatically; run 'airc daemon install' explicitly if wanted."
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
