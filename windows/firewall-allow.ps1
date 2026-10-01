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
  [switch]$CheckOnly,
  [string]$LogPath
)
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
  if ($rules | Where-Object { $_.Enabled -eq 'True' -and $_.Direction -eq 'Inbound' -and $_.Action -eq 'Block' }) { return $false }
  foreach ($protocol in @('TCP', 'UDP')) {
    $found = $false
    foreach ($rule in $rules) {
      if ($rule.Direction -ne 'Inbound' -or $rule.Action -ne 'Allow' -or
          $rule.Enabled -ne 'True' -or $rule.Profile -ne 'Any') { continue }
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

function Set-AircFirewallPolicy {
  param([string]$Program)
  # Previous prompts may have created Block rules. Replace local inbound rules
  # for this executable; preserve other programs and outbound/managed policy.
  Get-AircFirewallRules -Program $Program -Store 'PersistentStore' |
    Where-Object { $_.Direction -eq 'Inbound' } |
    Remove-NetFirewallRule -ErrorAction Stop
  foreach ($protocol in @('TCP', 'UDP')) {
    New-NetFirewallRule -DisplayName "airc (LAN $protocol inbound)" `
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
