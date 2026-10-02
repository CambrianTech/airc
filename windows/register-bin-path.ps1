[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$BinDirectory)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
$ErrorActionPreference = 'Stop'
function Get-AircRegisteredPath {
    param([string]$CurrentPath, [string]$SelectedDirectory)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $entries = [Collections.Generic.List[string]]::new()
    foreach ($entry in @($SelectedDirectory) + @($CurrentPath -split ';')) {
        if (-not $entry) { continue }
        $value = $entry.Trim().Trim('"')
        try { $key = [IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($value)).TrimEnd('\','/') }
        catch { $key = $value.TrimEnd('\','/') }
        if ($seen.Add($key)) { $entries.Add($value) }
    }
    return $entries -join ';'
}
$current = [Environment]::GetEnvironmentVariable('PATH','User')
$updated = Get-AircRegisteredPath -CurrentPath $current -SelectedDirectory $BinDirectory
if ($current -cne $updated) {
    [Environment]::SetEnvironmentVariable('PATH', $updated, 'User')
}

} # Installer diagnostic process boundary.
