<#
.SYNOPSIS
  Reconcile AIRC's Windows LAN firewall policy before starting its listener.
.DESCRIPTION
  Setup owns TCP transport and UDP presence rules for the installed executable
  and local-subnet sources, including networks Windows classifies as Public.
  Does not change network categories or disable firewall notifications.
  CheckOnly reads effective policy; apply requires administrator consent.
#>
param(
  [Parameter(Mandatory = $true)][string]$AircPath,
  [string]$OwnerSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value,
  [switch]$CheckOnly,
  [string]$LogPath
)
. (Join-Path $PSScriptRoot 'setup-entrypoint.ps1')
Invoke-InstallerEntryPoint {
Initialize-InstallerPowerShell
$ErrorActionPreference = 'Stop'
if ($LogPath) { Start-Transcript -Path $LogPath -Force | Out-Null }

function Get-AircFirewallRules {
  param([string]$Program, [string]$Store = 'ActiveStore')
  # Match the executable, never the display-name prefix of unrelated apps.
  $associated = @(Get-NetFirewallApplicationFilter -PolicyStore $Store -ErrorAction Stop |
    Where-Object { $_.Program -eq $Program } |
    Get-NetFirewallRule -PolicyStore $Store -ErrorAction Stop)
  # Association results can omit runtime-only status. Retrieve the actual rule
  # from the requested store, then confirm its executable scope again.
  foreach ($name in @($associated.Name | Sort-Object -Unique)) {
    $rule = Get-NetFirewallRule -Name $name -PolicyStore $Store -ErrorAction Stop
    $application = $rule | Get-NetFirewallApplicationFilter -ErrorAction Stop
    if ($application.Program -eq $Program) { $rule }
  }
}

function Test-AircFirewallOk {
  param([string]$Program)
  $rules = @(Get-AircFirewallRules -Program $Program)
  if (@($rules | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and $_.Group -and $_.Group -ne (Get-AircFirewallGroup) }).Count) { return $false }
  if (@(Get-AircOwnedFirewallInventory | Where-Object Program -ne $Program).Count) { return $false }
  if ($rules | Where-Object { $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }) { return $false }
  # Canonical rules do not cancel a legacy broad allow. Continuum's former
  # program rule can coexist with them and still expose this app beyond LAN.
  foreach ($rule in @($rules | Where-Object { $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' })) {
    $address = $rule | Get-NetFirewallAddressFilter -ErrorAction Stop
    if (@($address.RemoteAddress).Count -ne 1 -or $address.RemoteAddress -ne 'LocalSubnet') { return $false }
  }
  foreach ($protocol in @('TCP', 'UDP')) {
    $found = $false
    foreach ($rule in $rules) {
      if ($rule.Direction -ne 'Inbound' -or $rule.Action -ne 'Allow' -or
          $rule.Enabled -ne 'True' -or $rule.Profile -ne 'Any' -or
          $rule.Group -ne (Get-AircFirewallGroup)) { continue }
      # Enabled is configuration, not proof that Windows enforces the rule.
      $enforcement = @($rule.EnforcementStatus)
      # NetSecurity formats CIM's Full (1) as "Enforced" in PowerShell.
      $enforced = @($enforcement | Where-Object { [string]$_ -in @('Enforced', 'Full', '1') })
      $failures = @($enforcement | Where-Object { [string]$_ -notin @('Enforced', 'Full', '1', 'ProfileInactive', 'InactiveProfile', '5') })
      # A rule spanning all profiles can be enforced on the active profile
      # while the remaining profiles are inactive. Actual CI provider receipt.
      if ($enforced.Count -eq 0 -or $failures.Count -ne 0) { continue }
      $port = $rule | Get-NetFirewallPortFilter -ErrorAction Stop
      $address = $rule | Get-NetFirewallAddressFilter -ErrorAction Stop
      if ($port.Protocol -eq $protocol -and $port.LocalPort -eq 'Any' -and
          $port.RemotePort -eq 'Any' -and @($address.RemoteAddress).Count -eq 1 -and
          $address.RemoteAddress -eq 'LocalSubnet' -and $address.LocalAddress -eq 'Any') {
        $found = $true
      }
    }
    if (-not $found) { return $false }
  }
  return $true
}

function Get-AircFirewallGroup {
  # Passed from the unelevated coordinator; do not reassign ownership to a
  # different administrator account used solely for setup consent.
  $null = New-Object Security.Principal.SecurityIdentifier($OwnerSid)
  return "AIRC setup account $OwnerSid"
}

function Test-AircInstallerRuleSignature {
  param($Rule)
  return $Rule.Direction -eq 'Inbound' -and $Rule.Action -eq 'Allow' -and
    $Rule.DisplayName -in @('airc (LAN TCP inbound)', 'airc (LAN UDP inbound)') -and
    $Rule.Description -eq 'AIRC setup: local-subnet transport and discovery for this executable.'
}

function Get-AircOwnedFirewallInventory {
  $group = Get-AircFirewallGroup
  foreach ($rule in @(Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop)) {
    if ($rule.Group -ne $group -or -not (Test-AircInstallerRuleSignature $rule)) { continue }
    $application = $rule | Get-NetFirewallApplicationFilter -ErrorAction Stop
    if (-not [IO.Path]::IsPathRooted($application.Program) -or [IO.Path]::GetFileName($application.Program) -ne 'airc.exe') { continue }
    [pscustomobject]@{ Rule=$rule; Program=$application.Program }
  }
}

function Write-AircFirewallInventory {
  param([string]$Program)
  $owned = @(Get-AircOwnedFirewallInventory)
  $legacyCurrent = @(Get-AircFirewallRules -Program $Program -Store 'PersistentStore' | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and (-not $_.Group -or $_.Group -eq (Get-AircFirewallGroup)) -and $_.Name -notin @($owned.Rule.Name) })
  $ambiguous = @(Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop | Where-Object {
    $_.Direction -eq 'Inbound' -and $_.DisplayName -match '^(airc($|[ .(_-])|continuum[-_])' -and
    $_.Name -notin @($owned.Rule.Name) -and $_.Name -notin @($legacyCurrent.Name)
  })
  Write-Host "AIRC firewall reconciliation: $($owned.Count + $legacyCurrent.Count + $ambiguous.Count) candidate rules; $($owned.Count) account-owned Allow rules; $($legacyCurrent.Count) current-path legacy rules; $($ambiguous.Count) unowned candidates preserved."
  foreach ($rule in $ambiguous) {
    $application = $rule | Get-NetFirewallApplicationFilter -ErrorAction Stop
    Write-Host ("Preserved unowned firewall rule: " + (@{ name=$rule.Name; displayName=$rule.DisplayName; program=$application.Program; group=$rule.Group; description=$rule.Description; action=[string]$rule.Action } | ConvertTo-Json -Compress))
  }
}

function Set-AircFirewallPolicy {
  param([string]$Program)
  $current = @(Get-AircFirewallRules -Program $Program -Store 'PersistentStore')
  Write-AircFirewallInventory -Program $Program
  if (@(Get-AircFirewallRules -Program $Program | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' -and $_.Enabled -eq 'True' }).Count) {
    throw 'An explicit inbound Block policy applies to AIRC. Setup preserved it and did not change firewall rules.'
  }
  if (@($current | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and $_.Group -and $_.Group -ne (Get-AircFirewallGroup) }).Count) {
    throw 'The selected AIRC executable has Allow policy owned by another firewall Group. Setup preserved it and did not change firewall rules.'
  }
  $owned = @(Get-AircOwnedFirewallInventory)
  # Setup explicitly reconciles Allow policy for this selected executable.
  # Other executable paths require the durable installer/account signature.
  $legacyCurrent = @($current | Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and $_.Name -notin @($owned.Rule.Name) })
  @($owned.Rule) + $legacyCurrent | Sort-Object Name -Unique | Remove-NetFirewallRule -ErrorAction Stop
  Write-Host "Removed $($owned.Count + $legacyCurrent.Count) selected-program or installer-owned Allow rules; other policy was preserved."
  foreach ($protocol in @('TCP', 'UDP')) {
    New-NetFirewallRule -DisplayName "airc (LAN $protocol inbound)" `
      -Group (Get-AircFirewallGroup) `
      -Description 'AIRC setup: local-subnet transport and discovery for this executable.' `
      -Direction Inbound -Program $Program -Action Allow -Enabled True `
      -Profile Any -Protocol $protocol -RemoteAddress LocalSubnet `
      -EdgeTraversalPolicy Block -PolicyStore PersistentStore -ErrorAction Stop | Out-Null
  }
  if (-not (Test-AircFirewallOk -Program $Program)) {
    # Make failed setup actionable without asking users to inspect policy by
    # hand. Only this executable's rule/filter metadata is printed.
    foreach ($rule in @(Get-AircFirewallRules -Program $Program)) {
      $port = $rule | Get-NetFirewallPortFilter -ErrorAction Stop
      $address = $rule | Get-NetFirewallAddressFilter -ErrorAction Stop
      Write-Host (@{ direction=[string]$rule.Direction; action=[string]$rule.Action;
        enabled=[string]$rule.Enabled; profile=[string]$rule.Profile;
        enforcement=@($rule.EnforcementStatus); protocol=[string]$port.Protocol;
        localPort=@($port.LocalPort); remotePort=@($port.RemotePort);
        localAddress=@($address.LocalAddress); remoteAddress=@($address.RemoteAddress)
      } | ConvertTo-Json -Compress)
    }
    throw 'Effective firewall policy does not allow AIRC TCP and UDP on the local subnet. Setup is incomplete; organization policy may prohibit local rules.'
  }
}

if (-not (Test-Path -LiteralPath $AircPath)) { throw "AIRC binary not found at: $AircPath" }
$AircPath = (Resolve-Path -LiteralPath $AircPath).Path
if ($CheckOnly) {
  try {
    Write-AircFirewallInventory -Program $AircPath
    if (Test-AircFirewallOk -Program $AircPath) { exit 0 } else { exit 1 }
  } catch {
    # Some Windows installations restrict even policy reads to administrators.
    # Distinguish that boundary from an absent rule or a failed verification.
    if ($_.CategoryInfo.Category -eq 'PermissionDenied') { exit 4 }
    throw
  }
}
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { throw 'AIRC firewall setup requires Windows administrator consent.' }
Set-AircFirewallPolicy -Program $AircPath
Write-Host "airc: verified TCP and UDP local-subnet firewall rules for $AircPath"

} # Installer diagnostic process boundary.
