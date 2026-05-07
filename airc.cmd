@echo off
REM airc.cmd -- Windows shim that lets `airc <verb>` work from any shell
REM (PowerShell, cmd, Run dialog, Task Scheduler) by launching Git Bash
REM directly with all forwarded arguments.
REM
REM Keep this shim deliberately boring: cmd -> bash -> airc. A prior
REM cmd -> powershell -> ps1 -> bash chain could hang before the bash
REM airc entrypoint ran under Claude Code Monitor on Windows.
REM
REM install.ps1 places this next to airc.ps1 in
REM   %USERPROFILE%\AppData\Local\Programs\airc
REM and adds that directory to user PATH.
setlocal

REM Single-source rule for dual Windows+WSL dev boxes: if the user has a
REM WSL airc install, run THAT clone. Otherwise Windows Monitor can run
REM %USERPROFILE%\.airc-src while WSL `airc update` updates
REM /home/<user>/.airc-src, leaving two drifting implementations.
REM Set AIRC_WINDOWS_NATIVE=1 or AIRC_DIR=... to force the native
REM Windows/Git-Bash clone.
if not defined AIRC_WINDOWS_NATIVE if not defined AIRC_DIR (
  where wsl.exe >nul 2>nul
  if not errorlevel 1 (
    wsl.exe bash -lc "test -x \"$HOME/.airc-src/airc\"" >nul 2>nul
    if not errorlevel 1 (
      REM Forwarding-args fix (post-#543): the previous shape
      REM    wsl.exe sh -lc "...\"$@\"..." airc %*
      REM dropped every positional arg silently. wsl.exe does not
      REM forward args after `-lc <string>` as $0, $1, $2... to the
      REM inline script — they get consumed by wsl.exe itself — so $@
      REM inside the script was always empty. `airc.cmd join` ended up
      REM running `exec airc` (no verb), fell through to the generic
      REM help banner, and Claude Code's Monitor saw airc print help
      REM and exit before any transport spawned. The "fresh start"
      REM banner the Windows trace showed was actually airc's no-args
      REM idle-host autostart path, not a real takeover.
      REM
      REM Fix: inline cmd's %* directly into the bash command string,
      REM so positional args become part of the script string before
      REM wsl.exe sees them. bash (not sh) matches the airc script's
      REM `#!/bin/bash` shebang and avoids subtle sh/dash divergence.
      wsl.exe bash -lc "exec \"$HOME/.airc-src/airc\" %*"
      exit /b %ERRORLEVEL%
    )
  )
)

set "AIRC_SRC=%AIRC_DIR%"
if not defined AIRC_SRC set "AIRC_SRC=%USERPROFILE%\.airc-src"
set "AIRC_SCRIPT=%AIRC_SRC%\airc"

if not exist "%AIRC_SCRIPT%" (
  echo airc.cmd: cannot find bash airc script at "%AIRC_SCRIPT%" 1>&2
  echo Set AIRC_DIR or reinstall airc. 1>&2
  exit /b 1
)

set "BASH_EXE="
if exist "%ProgramFiles%\Git\bin\bash.exe" set "BASH_EXE=%ProgramFiles%\Git\bin\bash.exe"
if not defined BASH_EXE if exist "%ProgramFiles(x86)%\Git\bin\bash.exe" set "BASH_EXE=%ProgramFiles(x86)%\Git\bin\bash.exe"
if not defined BASH_EXE if exist "%LOCALAPPDATA%\Programs\Git\bin\bash.exe" set "BASH_EXE=%LOCALAPPDATA%\Programs\Git\bin\bash.exe"
if not defined BASH_EXE for %%B in (bash.exe) do if not "%%~$PATH:B"=="" set "BASH_EXE=%%~$PATH:B"

if not defined BASH_EXE (
  echo airc.cmd: Git Bash bash.exe not found. Install Git for Windows and retry. 1>&2
  exit /b 1
)

"%BASH_EXE%" "%AIRC_SCRIPT%" %*
exit /b %ERRORLEVEL%
