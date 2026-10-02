#!/usr/bin/env bash
# PowerShell 7 module paths can shadow Windows PowerShell 5 modules when Bash
# sits between the two hosts. Let PS5 build its own defaults for this child;
# preserve the caller's environment, arguments, output and exit status.
# Keep this shell alive until the native child completes. MSYS exec emulation
# can leave Win32 ParentProcessId pointing at an exited intermediate process,
# breaking both installer ancestry validation and gsudo's process-tree cache.
unset PSModulePath
powershell.exe "$@"
result=$?
exit "$result"
