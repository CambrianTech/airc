# Sourced by airc. Cross-platform helpers — proc_*, port_*, file_*,
# detect_platform, iso_to_epoch. See top-of-file comment for the
# extracted-from-airc rationale (#152 Phase 3).

# ── Platform adapters ───────────────────────────────────────────────────
#
# Single-purpose helpers that hide platform-specific differences in the
# process / port / filesystem APIs. Every callsite that needs "find
# children of PID X" or "find PIDs listening on port Y" goes through
# these helpers, NOT inline pgrep/lsof. That way:
#
#   1. The platform-specific implementation lives in ONE place per
#      capability — adding a Windows fallback for `lsof` (e.g. via
#      `netstat -ano`) means editing one helper, not 4+ callsites.
#   2. The business logic above the adapter line stays platform-
#      agnostic. Refactor risk drops.
#   3. We hold the line on Joel's "fixing one platform shouldn't
#      degrade another" rule (2026-04-26): without adapters, a Mac
#      AI's tweak to a pgrep callsite easily diverges from the Linux
#      AI's tweak. With adapters, both AIs touch the same helper.
#
# Each adapter takes simple inputs and emits a one-thing-per-line
# stream, suitable for `while IFS= read -r` consumption. Callers can
# `tr '\n' ' '` if they want space-separated, but the canonical
# representation is newline-delimited (POSIX-friendly).
#
# Conventions:
#   - `proc_*` — process / PID introspection
#   - `port_*` — TCP port introspection
#   - `file_*` — filesystem metadata
#   - `detect_*` — environment classification

# Return PIDs of direct children of $1, one per line.
# Implementations: pgrep -P (POSIX/macOS/Linux), ps fallback for
# environments without pgrep (Git Bash for Windows ships only msys
# coreutils — no pgrep by default; the fallback uses `ps -axo pid,ppid`
# which msys2 ps DOES support). Empty output if no children or pid is
# already gone.
proc_children() {
  local pid="$1"
  [ -z "$pid" ] && return 0
  if command -v pgrep >/dev/null 2>&1; then
    pgrep -P "$pid" 2>/dev/null
  else
    # POSIX-portable fallback. Works on Git Bash (msys ps), Linux ps,
    # macOS ps. Awk filters by ppid column.
    ps -axo pid,ppid 2>/dev/null | awk -v p="$pid" '$2 == p { print $1 }'
  fi
}

# Return parent PID of $1. Empty if $1 is gone.
proc_parent() {
  local pid="$1"
  [ -z "$pid" ] && return 0
  ps -p "$pid" -o ppid= 2>/dev/null | tr -d ' '
}

# Return the command line of $1 (full argv, space-joined). Empty if gone.
proc_cmdline() {
  local pid="$1"
  [ -z "$pid" ] && return 0
  ps -p "$pid" -o command= 2>/dev/null
}

# Find airc-related PIDs owned by the current user matching a pattern.
# Used by `airc teardown --all` to nuke every airc process.
# Pattern is a regex passed to pgrep -f or to awk's =~.
proc_airc_pids_matching() {
  local pattern="$1"
  [ -z "$pattern" ] && return 0
  if command -v pgrep >/dev/null 2>&1; then
    pgrep -u "$(id -u)" -f "$pattern" 2>/dev/null
  else
    # Fallback: ps + awk. Less precise than pgrep -f (no anchored regex)
    # but covers the same shape. Filter by user since msys ps -u option
    # may not match POSIX semantics.
    local me; me=$(whoami 2>/dev/null)
    ps -axo pid,user,command 2>/dev/null \
      | awk -v u="$me" -v p="$pattern" 'NR>1 && $2 == u && $0 ~ p { print $1 }'
  fi
}

# Return PIDs listening on TCP port $1 (LISTEN state), one per line.
# Implementations:
#   1. lsof -tiTCP:<port> -sTCP:LISTEN — macOS, most BSDs, modern Linux
#      with lsof installed.
#   2. ss -tlnp — modern Linux distros (iproute2 default since ~2017),
#      replaces deprecated netstat. Output post-processing extracts pid.
#   3. netstat -ano — Windows native (cmd / PowerShell), and also a
#      fallback on minimal Linux containers without lsof or ss. Output
#      shape differs per platform; awk parses the LISTENING column.
# Empty output = nobody listening.
port_listeners() {
  local port="$1"
  [ -z "$port" ] && return 0
  if command -v lsof >/dev/null 2>&1; then
    lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null
  elif command -v ss >/dev/null 2>&1; then
    # ss output: 'LISTEN 0 ... users:(("python",pid=12345,fd=4))'
    # Awk extracts pid= number.
    ss -tlnp "( sport = :$port )" 2>/dev/null \
      | awk 'NR>1 { match($0, /pid=[0-9]+/); if (RSTART) print substr($0, RSTART+4, RLENGTH-4) }'
  elif command -v netstat >/dev/null 2>&1; then
    # netstat -ano output (Windows + some Linux):
    #   TCP 0.0.0.0:7547 0.0.0.0:0 LISTENING 12345
    # Trailing column is PID. Match $port at end of local-address column.
    netstat -ano 2>/dev/null \
      | awk -v p=":$port" '$2 ~ p"$" && /LISTEN/ { print $NF }'
  fi
}

# ── Portable stat helpers — must validate numeric output, not just exit code ──
#
# stat differs across BSD/GNU AND has a Windows-MSYS trap: `stat -f` is
# BSD's "format specifier" but GNU's "filesystem info." On MSYS Git Bash
# (GNU stat), `stat -f %m FILE` exits 0 and prints multi-line filesystem
# metadata to STDOUT — silently succeeding with non-numeric junk. The
# usual `bsd_cmd || gnu_cmd || fallback` chain DOESN'T fall through
# because the BSD attempt exits 0. b69f 2026-05-02 hit this on Windows:
# arithmetic at lib_auth.sh:78 expanded the captured string and bash
# strict-mode flagged "File: unbound variable" because the captured
# value started with `  File: "<path>"`.
#
# Fix shape: each helper validates that the output is a non-empty
# all-digits string. If not, fall through to the next strategy. Final
# fallback echoes "0" (safe for arithmetic, signals "couldn't tell").

# Internal: emit value to stdout if it's a non-negative integer, else
# return non-zero so the caller's || chain advances.
_emit_if_numeric() {
  case "$1" in
    ''|*[!0-9]*) return 1 ;;
    *) printf '%s\n' "$1"; return 0 ;;
  esac
}

# Return file mtime as epoch seconds. Echoes 0 on any failure.
# Try GNU stat first (Linux + MSYS) since BSD `stat -f` on MSYS
# silently succeeds with filesystem metadata — that's the trap.
file_mtime() {
  local path="$1"
  [ -f "$path" ] || { echo 0; return 0; }
  local v
  if v=$(stat -c %Y "$path" 2>/dev/null) && _emit_if_numeric "$v"; then return 0; fi
  if v=$(stat -f %m "$path" 2>/dev/null) && _emit_if_numeric "$v"; then return 0; fi
  echo 0
}

# Return file size in bytes. Echoes 0 on any failure.
file_size() {
  local path="$1"
  [ -f "$path" ] || { echo 0; return 0; }
  local v
  if v=$(stat -c %s "$path" 2>/dev/null) && _emit_if_numeric "$v"; then return 0; fi
  if v=$(stat -f %z "$path" 2>/dev/null) && _emit_if_numeric "$v"; then return 0; fi
  if v=$(wc -c < "$path" 2>/dev/null | tr -d '[:space:]') && _emit_if_numeric "$v"; then return 0; fi
  echo 0
}

# Detect platform: emits one of macos, linux, wsl, windows-bash (Git Bash
# on Windows native), unknown. Most callers don't need this — they
# should use the proc_/port_/file_ adapters, which handle platform
# differences internally. detect_platform is for the rare case where
# a top-level decision genuinely depends on platform (e.g. Tailscale.app
# launching on macOS).
detect_platform() {
  case "$(uname -s 2>/dev/null)" in
    Darwin)               echo darwin ;;
    Linux)                grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null && echo wsl || echo linux ;;
    MINGW*|MSYS*|CYGWIN*) echo windows ;;
    *)                    echo unknown ;;
  esac
}

# Convert an ISO 8601 UTC timestamp to a Unix epoch (seconds since 1970).
# Echoes the epoch on success, empty on failure.
#
# Migrated to airc_core.datetime as Phase 0a of the Python truth-layer
# (#152 architecture). Pre-migration this was a 3-fallback adapter
# chain inline in bash (BSD date / GNU date / python3 heredoc).
# Post-migration the bash function is a one-line call into the
# Python module — same contract, same stdout shape, but the logic
# lives in a testable Python file with no bash → python heredoc
# substitution risk. First migration; pattern for the rest.
iso_to_epoch() {
  local ts="${1:-}"
  [ -z "$ts" ] && return 0
  "$AIRC_PYTHON" -m airc_core.datetime iso_to_epoch "$ts" 2>/dev/null
}

# MSYS / Git Bash path conversion. Six callsites in airc + three in
# install.sh used the same `if command -v cygpath ... else sed ...`
# block; #205 Target #3 collapsed them. cygpath when present (MSYS2,
# modern Git Bash); sed fallback for stripped-down environments.
# Both directions exposed so callers don't have to remember which sed
# regex inverts the other.
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

# ── End platform adapters ───────────────────────────────────────────────
