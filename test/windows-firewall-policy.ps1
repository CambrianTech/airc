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
$OwnerSid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
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
        } else { $state.rules }
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
    param($DisplayName,$Description,$Group,$Direction,$Program,$Action,$Enabled,$Profile,$Protocol,$RemoteAddress,$EdgeTraversalPolicy,$PolicyStore,$ErrorAction)
    if ($state.failWrite) { throw 'fixture: rule write denied' }
    if ($PolicyStore -ne 'PersistentStore' -or $EdgeTraversalPolicy -ne 'Block') { throw 'Unexpected rule policy' }
    $state.next++
    $state.rules+= [pscustomobject]@{Name=$state.next;DisplayName=$DisplayName;Description=$Description;Group=$Group;Program=$Program;Direction=$Direction;Action=$Action;Enabled=$Enabled;Profile=$Profile;Protocol=$Protocol;RemoteAddress=$RemoteAddress;LocalAddress='Any';LocalPort='Any';RemotePort='Any';EnforcementStatus=$state.enforcement}
}
$binary="C:\fixture O'Brien\airc.exe"
if (Test-AircFirewallOk $binary) { throw 'Missing rules accepted' }
Set-AircFirewallPolicy $binary
if (-not (Test-AircFirewallOk $binary) -or $state.rules.Count -ne 2) { throw 'Fresh setup did not produce TCP and UDP rules' }
# Account-marked obsolete paths are owned even after the executable disappears.
# Names/signatures alone at other paths or other accounts do not grant ownership.
$signature='AIRC setup: local-subnet transport and discovery for this executable.'
New-NetFirewallRule -DisplayName 'airc (LAN TCP inbound)' -Description $signature -Group (Get-AircFirewallGroup) -Program 'D:\old-build\airc.exe' -Direction Inbound -Action Allow -Enabled True -Profile Any -Protocol TCP -RemoteAddress LocalSubnet -EdgeTraversalPolicy Block -PolicyStore PersistentStore
$staleName=$state.next
New-NetFirewallRule -DisplayName 'airc (LAN TCP inbound)' -Description $signature -Program 'D:\other-install\airc.exe' -Direction Inbound -Action Allow -Enabled True -Profile Any -Protocol TCP -RemoteAddress LocalSubnet -EdgeTraversalPolicy Block -PolicyStore PersistentStore
$unknownName=$state.next
New-NetFirewallRule -DisplayName 'airc (LAN TCP inbound)' -Description $signature -Group 'AIRC setup account S-1-5-18' -Program 'D:\other-account\airc.exe' -Direction Inbound -Action Allow -Enabled True -Profile Any -Protocol TCP -RemoteAddress LocalSubnet -EdgeTraversalPolicy Block -PolicyStore PersistentStore
$foreignName=$state.next
New-NetFirewallRule -DisplayName 'airc (LAN UDP inbound)' -Description $signature -Group (Get-AircFirewallGroup) -Program 'D:\old-build\airc.exe' -Direction Inbound -Action Block -Enabled True -Profile Any -Protocol UDP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
$blockName=$state.next
if (Test-AircFirewallOk $binary) { throw 'Current canonical rules hid obsolete account-owned allowances' }
Set-AircFirewallPolicy $binary
if ($staleName -in $state.rules.Name -or $unknownName -notin $state.rules.Name -or $foreignName -notin $state.rules.Name -or $blockName -notin $state.rules.Name) { throw 'Owned cleanup crossed account/signature/Allow boundaries' }
if (-not (Test-AircFirewallOk $binary)) { throw 'Owned obsolete allowance was not reconciled' }
$state.rules=@($state.rules | Where-Object Program -eq $binary)
$state.rules | ForEach-Object { $_.Group=$null }
if (Test-AircFirewallOk $binary) { throw 'Untagged rules did not request ownership migration' }
Set-AircFirewallPolicy $binary
if (@($state.rules | Where-Object Group -eq (Get-AircFirewallGroup)).Count -ne 2) { throw 'Proven current-path canonical rules did not migrate' }
# Anonymized reported inventory shape: canonical pair, one ambiguous ProgramAny
# allowance, and 44 Windows prompt rules for build/runtime/test executables.
# Eight Blocks and all unproven paths must survive; do not infer ownership from
# Query User naming, the executable basename, or the developer cache directory.
$state.rules | ForEach-Object { $_.Group=$null }
New-NetFirewallRule -DisplayName 'airc inbound TCP band (continuum grid)' -Program Any -Direction Inbound -Action Allow -Enabled True -Profile Any -Protocol TCP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
for ($index=1; $index -le 44; $index++) {
  $display=if($index -le 20) { 'airc_lib-fixture.exe' } else { 'continuum_core-fixture.exe' }
  $kind=if($index -le 8) { 'Block' } else { 'Allow' }
  New-NetFirewallRule -DisplayName $display -Description $display -Program "D:\fixture-cache\debug\deps\$index\$display" -Direction Inbound -Action $kind -Enabled True -Profile Public -Protocol TCP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
}
$preserved=@($state.rules | Where-Object Program -ne $binary | ForEach-Object Name)
Set-AircFirewallPolicy $binary
if ($state.rules.Count -ne 47 -or @($state.rules | Where-Object Action -eq 'Block').Count -ne 8 -or @($preserved | Where-Object { $_ -notin $state.rules.Name }).Count) { throw 'Reported unowned inventory was guessed away' }
$report=@(Write-AircFirewallInventory $binary 6>&1) -join "`n"
if ($report -notmatch '47 candidate rules' -or $report -notmatch '45 unowned candidates preserved') { throw "Unresolved inventory hidden on healthy rerun: $report" }
$state.rules=@($state.rules | Where-Object Program -eq $binary)
# A broad Continuum legacy allow must not hide beside both canonical rules.
New-NetFirewallRule -DisplayName 'airc daemon inbound (continuum grid)' -Program $binary -Direction Inbound -Action Allow -Enabled True -Profile Any -Protocol Any -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
if (Test-AircFirewallOk $binary) { throw 'Canonical rules masked the legacy broad Continuum allowance' }
New-NetFirewallRule -DisplayName 'airc.exe' -Description 'airc.exe' -Program $binary -Direction Inbound -Action Allow -Enabled True -Profile Public -Protocol UDP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
Set-AircFirewallPolicy $binary
if (-not (Test-AircFirewallOk $binary) -or $state.rules.Count -ne 2) { throw 'Exact selected executable broad/Windows prompt allowances did not migrate' }
New-NetFirewallRule -DisplayName 'airc.exe' -Group 'AIRC setup account S-1-5-18' -Program $binary -Direction Inbound -Action Allow -Enabled True -Profile Public -Protocol UDP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
$foreignCurrent=$state.next; $before=@($state.rules.Name); $rejected=$false
if (Test-AircFirewallOk $binary) { throw 'Foreign ownership of selected executable was accepted' }
try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'another firewall Group' }
if (-not $rejected -or ($before -join ',') -ne ($state.rules.Name -join ',')) { throw 'Foreign Group was changed or refusal occurred after mutation' }
$state.rules=@($state.rules | Where-Object Name -ne $foreignCurrent)
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
# Blocks are explicit policy regardless of whether Windows or an owner created them.
New-NetFirewallRule -DisplayName 'airc.exe' -Program $binary -Direction Inbound -Action Block -Enabled True -Profile Any -Protocol UDP -RemoteAddress Any -EdgeTraversalPolicy Block -PolicyStore PersistentStore
if (Test-AircFirewallOk $binary) { throw 'Prompt-created block ignored' }
$before=@($state.rules.Name)
$rejected=$false
try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'explicit inbound Block' }
if (-not $rejected -or ($before -join ',') -ne ($state.rules.Name -join ',')) { throw 'Explicit Block policy was overridden or rules changed before refusal' }
$state.rules=@($state.rules | Where-Object { -not ($_.Program -eq $binary -and $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block') })
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
$state.enforcement=@('ProfileInactive','Enforced')
Set-AircFirewallPolicy $binary
if (-not (Test-AircFirewallOk $binary)) { throw 'Enforced active profile with inactive alternate profiles rejected' }
$state.enforcement=@('Enforced','LocalFirewallRulesDisallowed')
$rejected=$false
try { Set-AircFirewallPolicy $binary } catch { $rejected=$_.Exception.Message -match 'Effective firewall policy' }
if (-not $rejected) { throw 'Enforced status masked a policy prohibition' }
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
