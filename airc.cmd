@echo off
REM airc.cmd -- Windows shim that lets `airc <verb>` work from any shell
REM (PowerShell, cmd, Run dialog, Task Scheduler) by launching pwsh on
REM the sibling airc.ps1 with all forwarded arguments.
REM
REM install.ps1 places this next to airc.ps1 in
REM   %USERPROFILE%\AppData\Local\Programs\airc
REM and adds that directory to user PATH.
pwsh -NoLogo -NoProfile -File "%~dp0airc.ps1" %*
