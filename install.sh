#!/usr/bin/env bash
#
# AIRC installer
#
# curl -fsSL https://raw.githubusercontent.com/CambrianTech/airc/main/install.sh | bash
#
# Clones the repo, puts the source `airc` on PATH, and installs AIRC skills.

set -euo pipefail

REPO_URL="https://github.com/CambrianTech/airc.git"
CLONE_DIR="${AIRC_DIR:-$HOME/.airc/src}"
# BIN_DIR remains for Windows shims. POSIX installs use $CLONE_DIR/airc
# directly so there is one public command file, not a second POSIX
# wrapper under BIN_DIR.
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
  # Issue #341 follow-up: openssl/Python crypto bootstrap removed.
  # Identity gen + signing live in Rust, so system OpenSSL and Python
  # package state are irrelevant to airc install correctness.

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
  # branch) when AIRC_CHANNEL is unset. Caught by QA 2026-04-28
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

_add_path_entry() {
  local path_entry="$1"
  if echo "$PATH" | tr ':' '\n' | grep -qx "$path_entry"; then
    return 0
  fi

  local rc
  for rc in "$HOME/.zshrc" "$HOME/.bashrc"; do
    [ -f "$rc" ] || continue
    # Strip any pre-existing airc-managed PATH lines (marked with the
    # trailing `# airc` comment) so reruns are idempotent and stale
    # entries pointing at retired install dirs (e.g., ~/.local/bin from
    # the pre-rust-rewrite install) don't accumulate. We only touch
    # lines we ourselves wrote; user-managed PATH lines stay put.
    if grep -qE '^export PATH=.*# airc$' "$rc"; then
      local tmp; tmp="$(mktemp "${rc}.airc.XXXXXX")"
      grep -vE '^export PATH=.*# airc$' "$rc" > "$tmp" && mv "$tmp" "$rc"
    fi
    # Ensure rc ends with a newline so the append starts on its own
    # line. Without this, an rc that ends with `alias foo="bar"` (no
    # trailing \n) produces the broken line
    # `alias foo="bar"export PATH="...:$PATH"  # airc`
    # which zsh parses as one malformed alias declaration and never
    # sets PATH. Caught live 2026-05-20 — Joel's `airc` resolved fine
    # from a manually-augmented PATH but new shells got nothing.
    if [ -s "$rc" ] && [ "$(tail -c1 "$rc")" != "$(printf '\n')" ]; then
      printf '\n' >> "$rc"
    fi
    printf 'export PATH="%s:$PATH"  # airc\n' "$path_entry" >> "$rc"
    ok "Added $path_entry to PATH in $(basename "$rc")"
    break
  done
  export PATH="$path_entry:$PATH"
}

# POSIX uses the source command directly: ~/.airc/src/airc. Windows still
# needs PATH shims because Git Bash / PowerShell / cmd resolve different
# executable suffixes.
case "$(uname -s 2>/dev/null)" in
  MINGW*|MSYS*|CYGWIN*)
    mkdir -p "$BIN_DIR"
    if [ -f "$CLONE_DIR/airc.shim" ]; then
      [ -f "$BIN_DIR/airc" ] && rm -f "$BIN_DIR/airc" 2>/dev/null || true
      cp -f "$CLONE_DIR/airc.shim" "$BIN_DIR/airc"
      chmod +x "$BIN_DIR/airc" 2>/dev/null || true
    else
      cp -f "$CLONE_DIR/airc" "$BIN_DIR/airc"
      chmod +x "$BIN_DIR/airc" 2>/dev/null || true
    fi
    [ -f "$CLONE_DIR/airc.cmd" ] && cp -f "$CLONE_DIR/airc.cmd" "$BIN_DIR/airc.cmd"
    [ -f "$CLONE_DIR/airc.ps1" ] && cp -f "$CLONE_DIR/airc.ps1" "$BIN_DIR/airc.ps1"
    _add_path_entry "$BIN_DIR"
    ;;
  *)
    chmod +x "$CLONE_DIR/airc"
    for stale in "$BIN_DIR/airc" "$BIN_DIR/airc-core"; do
      if [ -L "$stale" ] || [ -f "$stale" ]; then
        rm -f "$stale"
        ok "Removed stale PATH forwarder: $stale"
      fi
    done
    _add_path_entry "$CLONE_DIR"
    ok "Using command: $CLONE_DIR/airc"
    ;;
esac

_install_airc_core_binary() {
  [ "${AIRC_SKIP_RUST_BUILD:-0}" = "1" ] && { info "AIRC_SKIP_RUST_BUILD=1 -- skipping airc-core build"; return 0; }
  if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is required to build airc-core. Install Rust, then re-run install.sh."
  fi
  info "Building Rust CLI: airc-core"
  (cd "$CLONE_DIR" && cargo build --release -p airc-cli)
  local built="$CLONE_DIR/target/release/airc-core"
  [ -x "$built" ] || fail "airc-core build completed but binary is missing: $built"
  case "$(uname -s 2>/dev/null)" in
    MINGW*|MSYS*|CYGWIN*)
      cp -f "$built" "$BIN_DIR/airc-core.exe"
      ok "Installed airc-core: $BIN_DIR/airc-core.exe"
      ;;
    *)
      chmod +x "$built"
      ok "Built airc-core: $built"
      ;;
  esac
}

_install_airc_core_binary

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

  local skill_dir skill_name target
  for skill_dir in "$CLONE_DIR"/skills/*/; do
    [ -d "$skill_dir" ] || continue
    [ -f "$skill_dir/SKILL.md" ] || continue
    skill_name="$(basename "$skill_dir")"
    target="$skills_target/$skill_name"
    # If the target is a real directory, it shadows the current skill
    # link. Remove it and install the current skill.
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
# Codex currently has no Claude-style Monitor tool. A daemon can keep
# the transport alive, but the model will not notice inbound peer
# traffic unless it polls local state during a turn. Install a small
# model-visible instruction so future Codex sessions surface AIRC
# traffic reliably without hitting GitHub: `airc codex-poll` reads the
# local messages.jsonl cursor, suppresses empty output, and excludes
# this identity's own messages.
#
# We only write this if the user has no existing developer_instructions
# key. If they do, do not overwrite; they can copy the block manually or
# opt into a profile. This keeps install.sh from clobbering unrelated
# Codex configuration.

_install_airc_codex_developer_instructions() {
  local config="$HOME/.codex/config.toml"
  local hooks_json="$HOME/.codex/hooks.json"
  [ "${AIRC_SKIP_CODEX_INSTRUCTIONS:-0}" = "1" ] && return 0
  [ -f "$config" ] || return 0

  if grep -qE '^[[:space:]]*(hooks|codex_hooks)[[:space:]]*=[[:space:]]*true' "$config" 2>/dev/null \
     && [ -f "$hooks_json" ] \
     && grep -qF 'airc codex-hook user-prompt-submit' "$hooks_json" 2>/dev/null; then
    if grep -qF 'AIRC-CODEX-INSTRUCTIONS-START' "$config" 2>/dev/null; then
      local _tmp; _tmp=$(mktemp)
      sed '/^# AIRC-CODEX-INSTRUCTIONS-START/,/^# AIRC-CODEX-INSTRUCTIONS-END/d' "$config" > "$_tmp"
      mv "$_tmp" "$config"
    fi
    info "  Codex AIRC hook already installed; skipping developer_instructions polling contract"
    return 0
  fi

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
- At the beginning of every user turn, before substantive work, if `airc` is available on PATH, run `airc codex-poll`.
- `airc codex-poll` is local-only: it reads this scope's local message cursor, prints only unread peer messages, advances the cursor, and is silent when empty.
- If `airc codex-poll` prints messages, briefly surface them to the user and account for them before continuing. If it is silent or says the scope is not initialized, continue normally.
- Do not claim AIRC collaboration is healthy from process health alone. Distinguish transport/process liveness from whether this Codex session has actually polled and seen peer traffic.
"""
# AIRC-CODEX-INSTRUCTIONS-END

TOML
    cat "$config"
  } > "$_tmp"
  mv "$_tmp" "$config"
  ok "Added Codex AIRC turn contract to ~/.codex/config.toml — restart Codex to activate automatic local inbox polling"
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

  local _airc_core=""
  if [ -x "$CLONE_DIR/target/release/airc-core" ]; then
    _airc_core="$CLONE_DIR/target/release/airc-core"
  elif [ -x "$CLONE_DIR/target/debug/airc-core" ]; then
    _airc_core="$CLONE_DIR/target/debug/airc-core"
  elif command -v airc-core >/dev/null 2>&1; then
    _airc_core=$(command -v airc-core)
  else
    warn "Could not install Codex AIRC hook: airc-core not found"
    return 0
  fi

  local out
  if out=$("$_airc_core" codex-hook install-hooks --codex-home "$HOME/.codex" 2>&1); then
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
