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
Initialize-InstallerPowerShell
# END GENERATED RUNTIME MODULES
# Dot-source the same small setup artifacts used by Continuum. AIRC does not
# install or require the Continuum application. Only immutable, verified bytes
# are loaded; dependency choices remain in the generated canonical manifest.
$aircSetupLock = Get-Content -LiteralPath (Join-Path $PSScriptRoot 'setup-artifacts.lock.json') -Raw | ConvertFrom-Json
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
