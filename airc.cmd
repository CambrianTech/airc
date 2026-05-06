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
