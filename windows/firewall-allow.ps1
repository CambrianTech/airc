<#
.SYNOPSIS
  Make airc reachable inbound on Windows — the firewall half of setup.

.DESCRIPTION
  On a typical Windows box, Defender Firewall blocks INBOUND for unknown
  programs by default, so airc's LAN listener is unreachable until a rule
  exists — and code-signing does NOT help (firewall != SmartScreen; the
  rule is required regardless of signature). Worse, each `airc daemon` bind
  trips Defender's "allow access?" prompt, and one "Cancel"/Block click (or
  repeated launches) leaves a pile of contradictory auto-created rules — and
  a BLOCK rule beats an ALLOW, so inbound stays dead (observed live: 8 Allow
  + 2 Block for airc.exe).

  Two modes so setup only ever prompts for elevation WHEN NEEDED:
    -CheckOnly : read-only, no admin. Exit 0 if the canonical allow rule is
                 present and there are no Block rules; exit 1 if a fix is
                 needed. install.sh runs this first (no UAC) and only elevates
                 to apply when it returns 1 — so steady-state updates are silent.
    (default)  : apply mode (needs admin). Removes every existing airc rule
                 (by display name AND by program path), then adds ONE
                 canonical inbound-allow rule for the binary. Idempotent.

  Invoked by install.sh; can also be re-run standalone (as admin):
    powershell -ExecutionPolicy Bypass -File windows\firewall-allow.ps1 -AircPath "C:\Users\<you>\.local\bin\airc.exe"

.PARAMETER AircPath
  Full path to the installed airc.exe the rule should allow.
.PARAMETER CheckOnly
  Report state via exit code without changing anything (no admin required).
#>
param(
  [Parameter(Mandatory = $true)][string]$AircPath,
  [switch]$CheckOnly
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $AircPath)) {
  Write-Error "airc binary not found at: $AircPath"
  exit 2
}
$AircPath = (Resolve-Path -LiteralPath $AircPath).Path

function Test-AircFirewallOk {
  # OK = a live inbound ALLOW rule for THIS binary exists AND there are no
  # airc Block rules (a Block would win and silently kill inbound).
  $blocks = Get-NetFirewallRule -DisplayName 'airc*' -ErrorAction SilentlyContinue |
    Where-Object { $_.Action -eq 'Block' }
  $allow = Get-NetFirewallApplicationFilter -Program $AircPath -ErrorAction SilentlyContinue |
    Get-NetFirewallRule -ErrorAction SilentlyContinue |
    Where-Object { $_.Direction -eq 'Inbound' -and $_.Action -eq 'Allow' -and $_.Enabled -eq 'True' }
  return ([bool]$allow -and -not [bool]$blocks)
}

if ($CheckOnly) {
  if (Test-AircFirewallOk) { exit 0 } else { exit 1 }
}

# Apply mode — changing firewall rules requires elevation.
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
  Write-Error 'firewall-allow.ps1 (apply mode) must run as Administrator.'
  exit 3
}

# 1) Drop the contradictory pile by display name (auto-created rules are
#    named after the program, e.g. "airc.exe").
Get-NetFirewallRule -DisplayName 'airc*' -ErrorAction SilentlyContinue |
  Remove-NetFirewallRule -ErrorAction SilentlyContinue

# 2) Belt-and-suspenders: remove ANY rule whose program filter points at this
#    exact binary, regardless of display name.
Get-NetFirewallApplicationFilter -Program $AircPath -ErrorAction SilentlyContinue |
  ForEach-Object {
    $_ | Get-NetFirewallRule -ErrorAction SilentlyContinue |
      Remove-NetFirewallRule -ErrorAction SilentlyContinue
  }

# 3) Add the single canonical inbound-allow rule (TCP, all profiles).
New-NetFirewallRule `
  -DisplayName 'airc (inbound)' `
  -Description 'Allow inbound LAN connections to the airc daemon (added by airc setup).' `
  -Direction Inbound `
  -Program $AircPath `
  -Action Allow `
  -Profile Any `
  -Protocol TCP | Out-Null

Write-Host "airc: firewall inbound-allow rule installed for $AircPath"
