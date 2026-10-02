# Native Windows entry. install.sh owns the shared install lifecycle; this
# bridge acquires Git/Bash and a source tree before handing it that lifecycle.
# Runs on stock Windows PowerShell 5.1. No PowerShell 7 or WSL required.
[CmdletBinding()]
param(
    [switch]$FirewallOnly,
    [switch]$DiagnoseDaemon,
    [switch]$RecoverElevatedDaemon,
    [string]$AircPath
)
$ErrorActionPreference = 'Stop'
# BEGIN GENERATED SHARED SETUP BOOTSTRAP
# BEGIN GENERATED RUNTIME MODULES
function Initialize-InstallerPowerShell {
    # A PS7 desktop host may pass its PSModulePath to Windows PowerShell 5.
    # Load the running engine's built-ins explicitly before autoload can select
    # another engine's Security/Utility type data. Keep user module paths intact.
    foreach ($name in @('Microsoft.PowerShell.Management', 'Microsoft.PowerShell.Utility', 'Microsoft.PowerShell.Security')) {
        $manifest = [IO.Path]::Combine($PSHOME, 'Modules', $name, ($name + '.psd1'))
        Import-Module $manifest -Global -ErrorAction Stop
    }
}

function Invoke-InstallerEntryPoint {
    param([Parameter(Mandatory = $true)][scriptblock]$Action)
    try {
        & $Action 2>&1 | ForEach-Object {
            if ($_ -is [Management.Automation.ErrorRecord]) {
                [Console]::Error.WriteLine($_.ToString())
            } else { Write-Output $_ }
        }
    } catch {
        [Console]::Error.WriteLine($_.ToString())
        exit 1
    }
}
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
# END GENERATED RUNTIME MODULES
# Dot-source the same small setup artifacts used by Continuum. AIRC does not
# install or require the Continuum application. Only immutable, verified bytes
# are loaded; dependency choices remain in the generated canonical manifest.
$aircSetupLock = @'
{
  "schemaVersion": 1,
  "continuumRevision": "961c4c70aa52d7b9df67cd3fbf1b200cbc2c727e",
  "elevationSha256": "6c6ba2fa18fe179c262af30c2fa0600735cc6d0345305deee42fd72f4109d94b",
  "manifestSha256": "116ae91fb1209b9c1ee5dee44734c913685b606e92cc4e959d61f015d7e28217"
}
'@ | ConvertFrom-Json
if ($aircSetupLock.schemaVersion -ne 1 -or $aircSetupLock.continuumRevision -notmatch '^[a-f0-9]{40}$') {
    throw 'Unsupported AIRC shared-setup artifact lock.'
}
$aircSetupCache = Join-Path $env:LOCALAPPDATA ('airc\setup-artifacts\' + $aircSetupLock.continuumRevision)

function Save-AircSetupArtifact {
    param([string]$Uri, [string]$OutFile)
    # Small pinned scripts only. Bound the entire response, including the body;
    # PS5 Invoke-WebRequest can hang in its legacy response processing path.
    Add-Type -AssemblyName System.Net.Http
    $client = New-Object Net.Http.HttpClient
    try {
        $client.Timeout = [TimeSpan]::FromSeconds(60)
        $client.MaxResponseContentBufferSize = 1048576
        $bytes = $client.GetByteArrayAsync($Uri).GetAwaiter().GetResult()
        [IO.File]::WriteAllBytes($OutFile, $bytes)
    } finally { $client.Dispose() }
}

function Get-AircSetupArtifact {
    param([string]$RelativePath, [string]$Sha256)
    if ($Sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid shared-setup artifact checksum.' }
    $path = Join-Path $aircSetupCache (Split-Path $RelativePath -Leaf)
    if ((Test-Path -LiteralPath $path -PathType Leaf) -and
        (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -eq $Sha256) { return $path }
    New-Item -ItemType Directory -Path $aircSetupCache -Force | Out-Null
    $temporary = Join-Path $aircSetupCache ([guid]::NewGuid().ToString('N') + '.download')
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $url = 'https://raw.githubusercontent.com/CambrianTech/continuum/' + $aircSetupLock.continuumRevision + '/' + $RelativePath
        Write-Host "Acquiring verified setup helper: $RelativePath"
        Save-AircSetupArtifact -Uri $url -OutFile $temporary
        if ((Get-FileHash -LiteralPath $temporary -Algorithm SHA256).Hash -ne $Sha256) {
            throw "Shared-setup artifact checksum mismatch: $RelativePath"
        }
        Move-Item -LiteralPath $temporary -Destination $path -Force
    } finally {
        if (Test-Path -LiteralPath $temporary) { Remove-Item -LiteralPath $temporary -Force }
    }
    return $path
}

$aircSetupManifest = Get-AircSetupArtifact 'tools/scripts/generated/manifest.windows.ps1' $aircSetupLock.manifestSha256
$aircSetupElevation = Get-AircSetupArtifact 'tools/scripts/lib/windows-elevation.ps1' $aircSetupLock.elevationSha256
. $aircSetupManifest
. $aircSetupElevation -GsudoSource $script:ContinuumManifest['gsudo'].source
# END GENERATED SHARED SETUP BOOTSTRAP
if ($DiagnoseDaemon -or $RecoverElevatedDaemon) {
    if ($DiagnoseDaemon -and $RecoverElevatedDaemon) { throw 'Choose diagnostics or elevated daemon recovery.' }
    if ($FirewallOnly) { throw 'Daemon diagnostics cannot be combined with firewall setup.' }
    if (-not $AircPath) { $AircPath = Join-Path $env:LOCALAPPDATA 'Programs\airc\airc.exe' }
    if (-not (Test-Path -LiteralPath $AircPath -PathType Leaf)) { throw 'Daemon diagnostics requires the installed AIRC executable via -AircPath.' }
    $diagnosticRoot = if ($env:AIRC_DIR) { $env:AIRC_DIR } else { $PSScriptRoot }
    if (-not $diagnosticRoot) { throw 'Run daemon diagnostics from the current AIRC setup checkout.' }
    foreach ($relative in @('windows\diagnose-daemon.ps1', 'windows\daemon-diagnostics.cs', 'windows\setup-entrypoint.ps1')) {
        if (-not (Test-Path -LiteralPath (Join-Path $diagnosticRoot $relative) -PathType Leaf)) {
            throw "Diagnostic source is incomplete (missing $relative). Run from the current AIRC setup checkout; diagnostic mode does not acquire or update source."
        }
    }
    $diagnosticHelper = Join-Path $diagnosticRoot 'windows\diagnose-daemon.ps1'
    # Resolve with the original account/environment before elevation; never
    # duplicate the canonical Windows endpoint hash in installer code.
    $callerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($RecoverElevatedDaemon -and (Test-IsAdmin)) { throw 'Recovery must start from the normal user token so adoption cannot create another elevated daemon.' }
    $endpoint = @(Invoke-InstallerProcess -OwnProcessTree $AircPath @('ipc-endpoint', '--native'))
    if ($global:LASTEXITCODE -ne 0 -or $endpoint.Count -ne 1 -or
        -not ([string]$endpoint[0]).StartsWith('\\.\pipe\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Installed AIRC cannot resolve its native endpoint. Deploy a build supporting ipc-endpoint --native before diagnostic elevation; no daemon was changed.'
    }
    if ($RecoverElevatedDaemon) {
        Invoke-InstallerProcess -OwnProcessTree $AircPath @('setup-recover-elevated-owner','--help') | Out-Null
        if ($global:LASTEXITCODE -ne 0) { throw 'Selected AIRC binary lacks supported elevated-owner recovery. Build/deploy the current installer candidate first.' }
        Write-Host 'AIRC setup will verify and gracefully stop only the selected same-account elevated AIRC owner, then adopt from this normal user token.'
    } else { Write-Host 'AIRC setup will inspect the selected daemon endpoint and its Windows process token. No daemon, permissions, task or firewall changes are made.' }
    try {
        Initialize-ElevationSession
        if ($RecoverElevatedDaemon) {
            Invoke-Elevated -Reason 'gracefully stopping the verified same-account elevated AIRC daemon' -CommandLine @(
                $AircPath, 'setup-recover-elevated-owner', '--endpoint', ([string]$endpoint[0]), '--caller-sid', $callerSid, '--installed-binary', $AircPath)
            if ($global:LASTEXITCODE -ne 0) { throw "Elevated daemon recovery refused or failed (exit $global:LASTEXITCODE); no force termination or permission change was attempted." }
            Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess $AircPath @('update','--adopt-installed')
            if ($global:LASTEXITCODE -ne 0) { throw "Normal-token adoption failed (exit $global:LASTEXITCODE); recovery remains incomplete." }
            return
        }
        Invoke-Elevated -Reason 'reading the selected AIRC daemon endpoint and Windows process token' -CommandLine @(
            (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'),
            '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'RemoteSigned', '-File', $diagnosticHelper,
            '-Endpoint', ([string]$endpoint[0]), '-CallerSid', $callerSid)
        if ($global:LASTEXITCODE -ne 0) { throw "Read-only daemon diagnostics failed (exit $global:LASTEXITCODE). No recovery action was taken." }
    } finally { Clear-Elevation }
    return
}
if ($FirewallOnly -and (-not $AircPath -or -not (Test-Path -LiteralPath $AircPath -PathType Leaf))) {
    throw 'Firewall-only setup requires the installed AIRC executable via -AircPath.'
}
function Refresh-Path {
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $paths = [Collections.Generic.List[string]]::new()
    foreach ($value in @([Environment]::GetEnvironmentVariable('PATH','User'),
                        [Environment]::GetEnvironmentVariable('PATH','Machine'), $env:PATH)) {
        foreach ($entry in @($value -split ';')) {
            if ($entry -and $seen.Add($entry.Trim())) { $paths.Add($entry.Trim()) }
        }
    }
    $env:PATH = $paths -join ';'
}
function Find-GitBash {
    $git = Get-Command git.exe -ErrorAction SilentlyContinue
    if ($git) {
        $execPath = Invoke-InstallerProcess -OwnProcessTree $git.Source @('--exec-path')
        if ($global:LASTEXITCODE -ne 0 -or -not (Test-Path (Join-Path $execPath 'git-remote-https.exe'))) { return $null }
        $root = Split-Path (Split-Path $git.Source -Parent) -Parent
        foreach ($relative in @('bin\bash.exe','usr\bin\bash.exe')) {
            $candidate = Join-Path $root $relative
            if (Test-Path -LiteralPath $candidate) { return $candidate }
        }
    }
    return $null
}
Refresh-Path
$bash = Find-GitBash
if (-not $bash) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw 'Git for Windows and winget are unavailable. Install Windows App Installer, then rerun AIRC setup.'
    }
    Write-Host 'Installing Git for Windows. If Windows requests consent, setup waits for you to approve it.'
    Invoke-InstallerProcess -OwnProcessTree 'winget' @('install', '--id', 'Git.Git', '--source', 'winget', '--exact', '--scope', 'user', '--silent', '--accept-package-agreements', '--accept-source-agreements', '--disable-interactivity')
    $installExit = $global:LASTEXITCODE
    if ($installExit -ne 0 -and $installExit -ne 3010) { throw "Git acquisition failed (winget exit $installExit)." }
    Refresh-Path
    $bash = Find-GitBash
    if (-not $bash) { throw "Git for Windows is unavailable after installation (exit $installExit). Rerun AIRC setup to resume." }
}
$managedSource = -not $env:AIRC_DIR -and -not ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml')))
$source = if ($env:AIRC_DIR) { $env:AIRC_DIR } elseif ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml'))) {
    $PSScriptRoot
} else { Join-Path $env:USERPROFILE '.airc\src' }
$channel = if ($env:AIRC_CHANNEL) { $env:AIRC_CHANNEL } else { 'canary' }
function Test-SetupLayout([string]$Directory) {
    foreach ($relative in @('install.sh','setup\github-auth.sh','windows\install-prereqs.ps1','windows\run-powershell.sh','windows\register-bin-path.ps1','windows\configure-firewall.ps1','windows\shared-setup.ps1','windows\setup-artifacts.lock.json','windows\install-session.ps1','windows\adopt-installed.ps1','windows\sync-bootstrap.ps1','windows\setup-entrypoint.ps1')) {
        if (-not (Test-Path -LiteralPath (Join-Path $Directory $relative))) { return $false }
    }
    return $true
}
if ((Test-Path (Join-Path $source 'Cargo.toml')) -and -not (Test-SetupLayout $source)) {
    if (-not $managedSource) {
        throw 'The explicitly selected source tree has an older setup layout. Update that checkout before invoking this installer; it will not switch or overwrite developer work.'
    }
    # Keep an older installer-owned checkout intact (it may contain user edits).
    # Acquire a compatible channel checkout beside it, using a bounded safe name.
    $hash = [Security.Cryptography.SHA256]::Create()
    try { $suffix = ([BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($channel))) -replace '-','').Substring(0,12).ToLowerInvariant() }
    finally { $hash.Dispose() }
    $source = Join-Path $env:USERPROFILE ('.airc\setup-source-' + $suffix)
    $sourceBase = $source
    $attempt = 0
    while ((Test-Path -LiteralPath $source) -and -not (Test-SetupLayout $source)) {
        $attempt++
        $source = $sourceBase + '-' + $attempt
    }
    Write-Host "Acquiring compatible $channel setup in $source; preserving the older checkout."
}
if (-not (Test-Path (Join-Path $source 'Cargo.toml'))) {
    $cloneArgs = @('clone','--quiet')
    # The published setup entry is canary; acquire matching helpers rather than
    # accidentally handing new setup to an older remote-default checkout.
    $cloneArgs += @('--branch',$channel)
    $cloneArgs += @('https://github.com/CambrianTech/airc.git',$source)
    Invoke-InstallerProcess -OwnProcessTree 'git' $cloneArgs
    if ($global:LASTEXITCODE -ne 0) { throw 'AIRC source acquisition failed. Rerun setup to resume.' }
}
if (-not (Test-SetupLayout $source)) { throw 'Selected source does not contain this setup version. Installation stopped before invoking an incompatible coordinator.' }
$source = (Resolve-Path -LiteralPath $source).Path
# Source update, prerequisites, auth, build, verification and integrations all
# run in the same coordinator used on Linux/macOS and Git Bash.
$names = @('AIRC_DIR','BIN_DIR','AIRC_WINDOWS_NATIVE','PSModulePath')
$saved = @{}
foreach ($name in $names) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }
$elevationReady = $false
try {
    . (Join-Path $source 'windows\shared-setup.ps1')
    $elevationReady = $true
    Initialize-ElevationSession
    $env:PSModulePath = $null
    if ($FirewallOnly) {
        # The same public source acquisition and owner serve standalone and
        # Continuum callers. Only AIRC owns the effective firewall policy.
        $powershell = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
        Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess $powershell @('-NoProfile', '-ExecutionPolicy', 'RemoteSigned', '-File', (Join-Path $source 'windows\configure-firewall.ps1'), '-AircPath', $AircPath)
        if ($global:LASTEXITCODE -ne 0) { throw "AIRC firewall setup failed (exit $global:LASTEXITCODE)." }
        return
    }
    $env:AIRC_DIR = $source
    # install.sh selects the explicit destination, existing PATH installation,
    # or platform default once for copy, firewall, startup and adoption.
    $env:AIRC_WINDOWS_NATIVE = '1'
    $env:PSModulePath = $null
    # Cancellation owns the build subtree; only a completed successful
    # coordinator may hand off its persistent daemon.
    Invoke-InstallerProcess -OwnProcessTree -PreserveChildrenOnSuccess $bash @('--noprofile', '--norc', ((Join-Path $source 'install.sh') -replace '\\','/'))
    $result = $global:LASTEXITCODE
} finally {
    try { if ($elevationReady) { Clear-Elevation } }
    finally {
        foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name,$saved[$name],'Process') }
        Refresh-Path
    }
}
if ($result -ne 0) { throw "AIRC setup failed (exit $result). Rerun the same setup to resume." }
$global:LASTEXITCODE = 0

} # Installer diagnostic process boundary.
