#!/usr/bin/env bash
# PowerShell 7 module paths can shadow Windows PowerShell 5 modules when Bash
# sits between the two hosts. Let PS5 build its own defaults for this child;
# preserve the caller's environment, arguments, output and exit status.
exec env -u PSModulePath powershell.exe "$@"
