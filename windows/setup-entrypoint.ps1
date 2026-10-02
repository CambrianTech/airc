# Generated from verified shared setup artifacts by windows/sync-bootstrap.ps1.
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
