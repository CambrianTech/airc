#!/usr/bin/env pwsh
# airc.ps1 — Windows entry point. Forwards to the bash `airc` via Git Bash.
#
# History: this file used to be a 2968-line full PowerShell port of the
# bash `airc`. After Phase 1+2 substrate landed (PRs #222-#243+) the PS
# port lagged ~100 commits behind canary — every fix had to be ported by
# hand and never was. Issue #152 tracked the drift.
#
# Resolution: stop maintaining a parallel codebase. Git Bash ships with
# `git for Windows` (which install.ps1 ensures is present). Run the
# bash `airc` from PowerShell by locating bash.exe and forwarding all
# arguments. One codebase, zero drift, instant feature parity.
#
# Probe order for bash.exe:
#   1. PATH (whichever `bash` is first — usually Git Bash when the user
#      ran install.ps1's git step)
#   2. Standard Git Bash paths: %ProgramFiles%\Git\bin\bash.exe and
#      %ProgramFiles(x86)%\Git\bin\bash.exe and %LOCALAPPDATA%\...
#   3. Excluded: WSL bash (System32\bash.exe). It's a Linux bash and
#      can't resolve our Windows paths.
#
# If no Git Bash found: print install instructions, exit 1.

#Requires -Version 7.0
$ErrorActionPreference = 'Stop'

function Resolve-BashExe {
    # 1. PATH — but exclude WSL's System32 shim since it runs Linux
    #    bash which won't see our Windows paths.
    $cmd = Get-Command bash -ErrorAction SilentlyContinue
    if ($cmd) {
        $resolved = $cmd.Source
        if ($resolved -notmatch '\\System32\\bash\.exe$') {
            return $resolved
        }
    }
    # 2. Standard Git Bash install locations.
    $candidates = @(
        "$env:ProgramFiles\Git\bin\bash.exe"
        "${env:ProgramFiles(x86)}\Git\bin\bash.exe"
        "$env:LOCALAPPDATA\Programs\Git\bin\bash.exe"
    )
    foreach ($p in $candidates) {
        if ($p -and (Test-Path -LiteralPath $p)) {
            return $p
        }
    }
    return $null
}

# Locate the bash `airc` script next to this file.
$aircDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$bashAirc = Join-Path $aircDir 'airc'
if (-not (Test-Path -LiteralPath $bashAirc)) {
    Write-Error "airc.ps1: cannot find sibling 'airc' bash script at $bashAirc. Reinstall via install.ps1."
    exit 1
}

$bashExe = Resolve-BashExe
if (-not $bashExe) {
    Write-Error @"
airc.ps1: bash.exe not found.

This is a thin shim that forwards to the bash 'airc' via Git Bash. Install
Git for Windows (which ships Git Bash) and re-run:

  winget install --id Git.Git -e
  airc

If you already have Git for Windows but bash.exe isn't on PATH, the standard
location is 'C:\Program Files\Git\bin\bash.exe' — open a new shell after
install, or re-run the Git installer with the 'Add to PATH' option enabled.
"@
    exit 1
}

# Forward every argument verbatim. PowerShell's @args splat preserves
# argument boundaries cleanly across the bash invocation.
& $bashExe "$bashAirc" @args
exit $LASTEXITCODE
