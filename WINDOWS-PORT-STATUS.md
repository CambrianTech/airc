# Windows Support — Two-Tier Strategy

This branch carries Windows-support work for airc. **Strategy reversed mid-branch** (Joel 2026-04-24, after Toby reported he uses Git Bash as his Windows Terminal default): try bash + Git Bash first, treat the PowerShell port as a fallback only if Git Bash compat truly can't be made to work.

## Current state

- **Tier 1 (active):** make bash `airc` work in Git Bash on Windows. Single codebase across mac / linux / WSL / Git Bash. Preferred outcome.
- **Tier 2 (scaffolded but on ice):** native PowerShell port (`airc.ps1`, `install.ps1`). Files exist as scaffolding insurance. Don't continue this work unless Tier 1 is shown to be infeasible.

## Tier 1 (Git Bash compat) — required fixes

Audited the bash file for Windows-Python and Git-Bash incompatible bits. Surface is small.

| Issue | Severity | Status | Fix |
|---|---|---|---|
| `signal.SIGALRM` / `signal.alarm` (watchdog python) | **Hard blocker** — Windows Python has no SIGALRM | ✓ done | try/except: SIGALRM on POSIX, threading.Timer fallback on Windows. `_arm_watchdog()` wrapper handles both. |
| `python3` not on PATH (Git Bash typically ships `python` only) | Soft blocker | ✓ done | bash function wrapper at top of file: `python3 () { command python "$@"; }` if python3 is missing but python exists. Hard fail with install hints if neither. |
| `pgrep -P $$` / `pkill` patterns in cmd_teardown | Maybe — needs Git Bash test | ⚠ untested | Git Bash usually ships procps-ng; if missing, fall back to cmd.exe `taskkill` via airc.pid file. Test on Windows first. |
| File permissions (`chmod 600` for SSH keys) | Probably fine | ⚠ untested | Git Bash uses Windows OpenSSH which respects ACLs; chmod is a no-op but doesn't error. |
| SSH paths (`~/.ssh/`) | Probably fine | ⚠ untested | Git Bash and OpenSSH-Windows use the same `%USERPROFILE%\.ssh\` location. |
| TCP listener (`bind('0.0.0.0', $host_port)`) | Should work | ⚠ untested | Windows allows binding to 0.0.0.0; firewall may prompt on first run. |

## Tier 1 — handoff checklist for Windows-Claude

When Windows-Claude (running in Git Bash on Joel's Windows machine) picks this up:

1. `git fetch origin && git checkout feat/powershell-native-port` (branch name kept for git history continuity; scope is now broader than the original PowerShell port)
2. `cd ~/.airc-src` (or wherever, this is for the install dir on Windows) — `bash install.sh` to install airc fresh from this branch
3. `airc version` → should print
4. `airc help` → should print
5. `airc connect` → host mode; expect TCP bind on 7547. Windows Defender may prompt — allow.
6. From anvil or bigmama-wsl: paste the join string Windows-Claude printed → other peer should pair
7. Bidirectional `airc msg` exchange
8. `airc teardown` → should kill cleanly without orphaning processes
9. `airc connect` again → resume should work

If any of those fail, file the symptom + stderr in the PR thread. The Tier 2 (PowerShell port) scaffold stays on this branch as the fallback.

## Tier 2 (PowerShell port) — on ice

Files preserved on this branch:
- `airc.ps1` — skeleton with scope detection, config helpers, version, help, command-dispatch stubs
- `install.ps1` — Windows-native installer
- (See git history for the original WINDOWS-PORT-STATUS that framed Tier 2 as the primary path.)

These are insurance. Do NOT iterate on them while Tier 1 is unconfirmed. If Tier 1 succeeds end-to-end, delete the Tier 2 files in a follow-up commit and the single-codebase claim holds across all platforms.

## Promotion gate

Same as before:

1. Windows-Claude works this branch until they feel good (Tier 1 commands functional in Git Bash, scenarios pass).
2. Merge feature branch → canary.
3. Three-peer E2E on canary: anvil (mac/bash), bigmama-wsl (WSL2/bash), windows-claude (Git Bash on native Windows). Real cross-implementation chat for one work session.
4. If all three peers good → promote canary → main.

## Joel's directive (verbatim)

> "we should remain 'pure shell' just do same for powershell yeah and in a PR"
>
> _later, after Toby's setup was reported:_
>
> "yeah just update the PR description and work" + "and try to code for both yeah"

The interpretation that lands: code for both (bash with Windows-Python compat AND PowerShell as fallback insurance), but invest in Tier 1 first.
