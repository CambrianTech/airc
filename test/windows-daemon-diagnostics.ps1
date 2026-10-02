$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
. (Join-Path $root 'windows/setup-entrypoint.ps1')
Initialize-InstallerPowerShell
Invoke-InstallerEntryPoint {
Add-Type -Path (Join-Path $root 'windows/daemon-diagnostics.cs')
function Assert($condition, $message) { if (-not $condition) { throw $message } }
$name = 'airc-diagnostic-test-' + [guid]::NewGuid().ToString('N')
$endpoint = '\\.\pipe\' + $name
$missing = [AircDaemonDiagnostics]::Inspect($endpoint)
Assert ($missing.pipeOpenError -eq 2) 'Missing endpoint must retain exact Win32 error 2.'
Assert ($missing.serverPid -eq 'UNKNOWN' -and $missing.tokenUserSid -eq 'UNKNOWN') 'Failure cannot invent process identity.'
$server = New-Object IO.Pipes.NamedPipeServerStream($name, [IO.Pipes.PipeDirection]::InOut, 1, [IO.Pipes.PipeTransmissionMode]::Byte, [IO.Pipes.PipeOptions]::Asynchronous)
try {
    $receipt = [AircDaemonDiagnostics]::Inspect($endpoint)
    Assert ($receipt.serverPid -eq $PID) 'Native pipe server PID must identify the actual fixture owner.'
    Assert ($receipt.tokenUserSid -eq [Security.Principal.WindowsIdentity]::GetCurrent().User.Value) 'Token SID must come from the actual process.'
    Assert ($receipt.tokenElevated -is [bool]) 'Elevation must be observed, not inferred from task configuration.'
    Assert ($receipt.tokenIntegritySid -match '^S-1-16-') 'Integrity SID was not observed.'
    Assert ($receipt.imagePath -ne 'UNKNOWN') 'Actual fixture image path was not observed.'
} finally { $server.Dispose() }
# An outbound-only server rejects the diagnostic client's read/write open.
# This is a synthetic pipe, never an ACL mutation on a live daemon.
$server = New-Object IO.Pipes.NamedPipeServerStream($name, [IO.Pipes.PipeDirection]::Out, 1, [IO.Pipes.PipeTransmissionMode]::Byte, [IO.Pipes.PipeOptions]::Asynchronous)
try {
    $denied = [AircDaemonDiagnostics]::Inspect($endpoint)
    Assert ($denied.pipeOpenError -eq 5) 'Denied pipe must retain exact Win32 error 5.'
    Assert ($denied.serverPid -eq 'UNKNOWN' -and $denied.tokenElevated -eq 'UNKNOWN') 'Access denied is not proof of an elevated daemon.'
} finally { $server.Dispose() }

# Exercise the actual public diagnostic adapter in a separate hidden PS5 process.
# The server remains this fixture, so no machine daemon or TCP listener is used.
$server = New-Object IO.Pipes.NamedPipeServerStream($name, [IO.Pipes.PipeDirection]::InOut, 1, [IO.Pipes.PipeTransmissionMode]::Byte, [IO.Pipes.PipeOptions]::Asynchronous)
$child = New-Object Diagnostics.Process
try {
    $child.StartInfo.FileName = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $child.StartInfo.Arguments = '-NoProfile -ExecutionPolicy RemoteSigned -File "' + (Join-Path $root 'windows/diagnose-daemon.ps1') + '" -Endpoint "' + $endpoint + '" -CallerSid "' + [Security.Principal.WindowsIdentity]::GetCurrent().User.Value + '"'
    $child.StartInfo.UseShellExecute = $false
    $child.StartInfo.CreateNoWindow = $true
    $child.StartInfo.RedirectStandardOutput = $true
    $child.StartInfo.RedirectStandardError = $true
    Assert ($child.Start()) 'Could not start synthetic diagnostic child.'
    $stdout = $child.StandardOutput.ReadToEndAsync()
    $stderr = $child.StandardError.ReadToEndAsync()
    if (-not $child.WaitForExit(15000)) { $child.Kill(); throw 'Synthetic diagnostic child exceeded 15 seconds.' }
    Assert ($child.ExitCode -eq 0) ('Diagnostic child failed: ' + $stderr.Result)
    $childReceipt = $stdout.Result | ConvertFrom-Json
    Assert ($childReceipt.serverPid -eq $PID) 'Child must observe fixture server PID rather than itself.'
    Assert $childReceipt.diagnosticOnly 'Actual adapter must mark observation as diagnostic only.'
    Assert ($childReceipt.callerSid -eq $childReceipt.tokenUserSid) 'Original caller and observed fixture owner differ.'
} finally { $child.Dispose(); $server.Dispose() }

# Exercise the public diagnostic branch with only the external process boundary
# mocked. It must return before source acquisition, prerequisites or installation.
$entry = [IO.File]::ReadAllText((Join-Path $root 'install.ps1'))
$start = $entry.IndexOf('if ($DiagnoseDaemon) {', [StringComparison]::Ordinal)
$end = $entry.IndexOf('if ($FirewallOnly -and', $start, [StringComparison]::Ordinal)
$action = [scriptblock]::Create($entry.Substring($start, $end - $start) + "`nthrow 'Diagnostic mode fell through into installer mutation.'")
$script:elevations = 0
$script:clears = 0
$script:resolverFails = $false
function Initialize-ElevationSession { }
function Clear-Elevation { $script:clears++ }
function Invoke-InstallerProcess {
    param([switch]$OwnProcessTree, [Parameter(Position=0)]$Executable, [Parameter(Position=1)]$Arguments)
    Assert (($Arguments -join ' ') -eq 'ipc-endpoint --native') 'Must use canonical resolver before elevation.'
    $global:LASTEXITCODE = if ($script:resolverFails) { 2 } else { 0 }
    if (-not $script:resolverFails) { $endpoint }
}
function Invoke-Elevated {
    param($Reason, $CommandLine)
    $script:elevations++
    Assert ($CommandLine -contains $endpoint) 'Pass the original account endpoint through elevation.'
    Assert ($CommandLine -contains [Security.Principal.WindowsIdentity]::GetCurrent().User.Value) 'Pass original caller SID.'
    $global:LASTEXITCODE = 0
}
$savedSource = $env:AIRC_DIR
try {
    $env:AIRC_DIR = $root
    $DiagnoseDaemon = $true
    $FirewallOnly = $false
    $AircPath = Join-Path $PSHOME 'powershell.exe'
    if (-not (Test-Path $AircPath)) { $AircPath = Join-Path $PSHOME 'pwsh.exe' }
    & $action
    Assert ($script:elevations -eq 1 -and $script:clears -eq 1) 'One owned elevation must be released.'
    $script:resolverFails = $true
    $failed = $false
    try { & $action } catch { $failed = $_.ToString() -like '*cannot resolve its native endpoint*' }
    Assert $failed 'An older binary must refuse clearly before elevation.'
    Assert ($script:elevations -eq 1) 'A failed resolver cannot trigger consent.'
    $incomplete = Join-Path $env:TEMP ('airc-diagnostic-layout-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path (Join-Path $incomplete 'windows') -Force | Out-Null
    try {
        # Entry exists, but its native source dependency is absent.
        Copy-Item -LiteralPath (Join-Path $root 'windows/diagnose-daemon.ps1') -Destination (Join-Path $incomplete 'windows/diagnose-daemon.ps1')
        $env:AIRC_DIR = $incomplete
        $failed = $false
        try { & $action } catch { $failed = $_.ToString() -like '*missing windows\daemon-diagnostics.cs*' }
        Assert $failed 'Missing native observer source must fail before consent.'
        Assert ($script:elevations -eq 1) 'Incomplete diagnostic source cannot trigger consent.'
    } finally {
        Remove-Item -LiteralPath (Join-Path $incomplete 'windows/diagnose-daemon.ps1')
        Remove-Item -LiteralPath (Join-Path $incomplete 'windows')
        Remove-Item -LiteralPath $incomplete
    }
} finally { $env:AIRC_DIR = $savedSource }
Write-Host 'PASS read-only daemon diagnostics: actual synthetic endpoint/token, denied/absent UNKNOWN, original-account resolver and owned elevation.'
}
exit 0
