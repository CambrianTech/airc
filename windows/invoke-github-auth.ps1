# Native process adapter only. GitHub onboarding policy lives in one shared stage.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$SourceDirectory)
$ErrorActionPreference = 'Stop'
$git = Get-Command git.exe -ErrorAction Stop
$gitRoot = Split-Path (Split-Path $git.Source -Parent) -Parent
$bash = Join-Path $gitRoot 'bin\bash.exe'
if (-not (Test-Path -LiteralPath $bash)) {
    $bash = Join-Path $gitRoot 'usr\bin\bash.exe'
}
if (-not (Test-Path -LiteralPath $bash)) { throw 'Git for Windows Bash is unavailable. Rerun AIRC setup to repair Git.' }
$stage = Join-Path $SourceDirectory 'setup\github-auth.sh'
# PowerShell 7's inherited module search path breaks Windows PowerShell 5.1
# grandchildren. Match windows/run-powershell.sh at this process boundary.
$previousModulePath = $env:PSModulePath
try {
    $env:PSModulePath = $null
    & $bash --noprofile --norc ($stage -replace '\\','/')
    if ($LASTEXITCODE -ne 0) { throw "AIRC GitHub authorization failed (exit $LASTEXITCODE). Rerun setup to resume." }
} finally { $env:PSModulePath = $previousModulePath }
