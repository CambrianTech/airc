# Exercise production policy functions against controlled firewall observations.
# No elevation, network-profile changes, or host firewall writes.
$ErrorActionPreference='Stop'
$source=Join-Path (Split-Path $PSScriptRoot -Parent) 'windows/firewall-allow.ps1'
$tokens=$null; $parseErrors=$null
$ast=[Management.Automation.Language.Parser]::ParseFile($source,[ref]$tokens,[ref]$parseErrors)
if ($parseErrors.Count) { throw $parseErrors[0] }
foreach ($definition in $ast.FindAll({param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst]},$true)) {
    . ([scriptblock]::Create($definition.Extent.Text))
}
$state=@{rules=@(); next=0; ineffective=$false; failWrite=$false; enforcement='Enforced'}
function Get-NetFirewallApplicationFilter {
    param([Parameter(ValueFromPipeline=$true)]$InputObject,$PolicyStore)
    process {
        if ($InputObject) { $InputObject }
        elseif (-not ($PolicyStore -eq 'ActiveStore' -and $state.ineffective)) { $state.rules }
    }
}
function Get-NetFirewallRule {
    param([Parameter(ValueFromPipeline=$true)]$InputObject,$PolicyStore,$Name)
    process {
        if ($PolicyStore -notin @('ActiveStore','PersistentStore')) { throw 'Firewall rule association lost the requested policy store' }
        if ($Name) { $state.rules | Where-Object Name -eq $Name }
        elseif ($InputObject) {
            $copy=$InputObject.PSObject.Copy()
            $copy.EnforcementStatus='NotApplicable'
            $copy
        }
    }
}
function Get-NetFirewallPortFilter {
    param([Parameter(ValueFromPipeline=$true)]$InputObject)
    process { $InputObject }
}
function Get-NetFirewallAddressFilter {
    param([Parameter(ValueFromPipeline=$true)]$InputObject)
    process { $InputObject }
}
function Remove-NetFirewallRule {
    param([Parameter(ValueFromPipeline=$true)]$InputObject)
    process { $state.rules=@($state.rules | Where-Object { $_.Name -ne $InputObject.Name }) }
}
function New-NetFirewallRule {
    param($DisplayName,$Description,$Direction,$Program,$Action,$Enabled,$Profile,$Protocol,$RemoteAddress,$EdgeTraversalPolicy,$PolicyStore,$ErrorAction)
    if ($state.failWrite) { throw 'fixture: rule write denied' }
    if ($PolicyStore -ne 'PersistentStore' -or $EdgeTraversalPolicy -ne 'Block') { throw 'Unexpected rule policy' }
    $state.next++
    $state.rules+= [pscustomobject]@{Name=$state.next;DisplayName=$DisplayName;Program=$Program;Direction=$Direction;Action=$Action;Enabled=$Enabled;Profile=$Profile;Protocol=$Protocol;RemoteAddress=$RemoteAddress;LocalAddress='Any';LocalPort='Any';RemotePort='Any';EnforcementStatus=$state.enforcement}
}
$binary="C:\fixture O'Brien\airc.exe"
if (Test-AircFirewallOk $binary) { throw 'Missing rules accepted' }
Set-AircFirewallPolicy $binary
if (-not (Test-AircFirewallOk $binary) -or $state.rules.Count -ne 2) { throw 'Fresh setup did not produce TCP and UDP rules' }
# The old helper's TCP-only / all-remote-address policy must not pass.
$state.rules=@($state.rules | Where-Object Protocol -eq 'TCP')
$state.rules[0].RemoteAddress='Any'
if (Test-AircFirewallOk $binary) { throw 'Legacy TCP-only broad rule accepted' }
# A similarly named unrelated executable and outbound policy must survive.
$other='C:\other-app\aircraft.exe'
New-NetFirewallRule -DisplayName 'aircraft' -Program $other -Direction Inbound -Action Block -Enabled True -Profile Any -Protocol TCP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
New-NetFirewallRule -DisplayName 'airc outbound' -Program $binary -Direction Outbound -Action Block -Enabled True -Profile Any -Protocol TCP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
Set-AircFirewallPolicy $binary
if (@($state.rules | Where-Object Program -eq $other).Count -ne 1 -or @($state.rules | Where-Object Direction -eq 'Outbound').Count -ne 1) { throw 'Unrelated policy removed' }
# Windows prompt-generated block rules defeat allows; setup must reconcile them.
New-NetFirewallRule -DisplayName 'airc.exe' -Program $binary -Direction Inbound -Action Block -Enabled True -Profile Any -Protocol UDP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
if (Test-AircFirewallOk $binary) { throw 'Prompt-created block ignored' }
Set-AircFirewallPolicy $binary
if (-not (Test-AircFirewallOk $binary)) { throw 'Prompt-created block not repaired' }
Set-AircFirewallPolicy $binary
if ($state.rules.Count -ne 4) { throw 'Repeat apply duplicated rules' }
$udp=$state.rules | Where-Object { $_.Program -eq $binary -and $_.Protocol -eq 'UDP' }
$udp.Profile='Private'
if (Test-AircFirewallOk $binary) { throw 'Private-only rule accepted for Public-network support' }
$udp.Profile='Any'; $udp.Enabled='False'
if (Test-AircFirewallOk $binary) { throw 'Disabled discovery rule accepted' }
$state.ineffective=$true
$rejected=$false
try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'Effective firewall policy' }
if (-not $rejected) { throw 'Ineffective policy reported as successful' }
$state.ineffective=$false
foreach ($status in @('NotApplicable','LocalFirewallRulesDisallowed','CategoryOff','RemoteAddressResolutionEmpty')) {
    $state.enforcement=$status
    $rejected=$false
    try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'Effective firewall policy' }
    if (-not $rejected) { throw "Enabled but unenforced rules accepted: $status" }
}
$state.enforcement='Full'; $state.failWrite=$true
$rejected=$false
try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'write denied' }
if (-not $rejected) { throw 'Rule creation failure suppressed' }
Write-Host 'PASS: TCP/UDP local-subnet policy, missing/legacy/blocked rules, Public profile, idempotence, unrelated policy preserved, write/effective-policy failures'
