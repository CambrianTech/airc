#!/usr/bin/env pwsh
# airc.ps1 - Windows-native PowerShell port of `airc` (the bash original).
#
# Single codebase per platform: bash `airc` for POSIX (macOS / Linux / WSL),
# this airc.ps1 for native Windows. Same wire protocol, same on-disk layout,
# same skills. They interoperate over SSH + gh gists.
#
# Hard requirements on Windows (install.ps1 sets all of these up):
#   - PowerShell 7+        (#Requires -Version 7 enforces it)
#   - Git for Windows      (provides openssl.exe + base toolchain)
#   - OpenSSH client       (Windows Capability -- ssh, ssh-keygen)
#   - Python 3             (monitor formatter heredoc + LAN-IP probe)
#   - GitHub CLI (gh)      (gist room substrate)
#   - Tailscale (optional) (peer addressing -- LAN/hostname fallback works)

#Requires -Version 7.0
$ErrorActionPreference = 'Stop'

# -- Version -------------------------------------------------------------
# Bash reports git short-sha + branch via `cmd_version`. We do the same.
# Static fallback for users running outside a git checkout.
$AIRC_FALLBACK_VERSION = '0.1.0-windows-port'

# -- Cross-platform Python invocation -----------------------------------
# Bash wraps `python3` to fall through to `python` on Git-Bash-on-Windows.
# We do the analogous probe up front and stash the chosen binary in
# $script:Py for use by every formatter / heredoc invocation. Probe
# order: python (skip the App Execution Alias stub), python3, py -3.
function Resolve-PythonBin {
    foreach ($name in @('python', 'python3')) {
        $cmd = Get-Command $name -ErrorAction SilentlyContinue
        if (-not $cmd) { continue }
        # Skip the Microsoft Store stub at WindowsApps\python.exe -- it just
        # prints "Python was not found; run without arguments to install"
        # and exits 9009 on any actual invocation.
        if ($cmd.Source -like '*\WindowsApps\*') { continue }
        return @{ Bin = $cmd.Source; Args = @() }
    }
    $py = Get-Command 'py' -ErrorAction SilentlyContinue
    if ($py -and $py.Source -notlike '*\WindowsApps\*') {
        return @{ Bin = $py.Source; Args = @('-3') }
    }
    # Well-known install-location fallback. winget's Python.Python.3.12
    # lands at $env:LOCALAPPDATA\Programs\Python\Python3XX\python.exe;
    # python.org Program Files installer at C:\Program Files\Python3XX\.
    # Both are added to User PATH by the installer, but a process launched
    # with a snapshotted PATH (or before the install) won't see them.
    # Same defensive pattern as Resolve-OpenSSL / Resolve-TailscaleBin.
    # Without this, airc.ps1 in such a process hits `& $null @(...)` in
    # the monitor pipeline and dies with "expression after '&' must be
    # a command."
    $candidates = @()
    foreach ($root in @(
        (Join-Path $env:LOCALAPPDATA 'Programs\Python'),
        $env:ProgramFiles,
        ${env:ProgramFiles(x86)}
    )) {
        if ($root -and (Test-Path $root)) {
            $candidates += Get-ChildItem -Path $root -Filter 'Python3*' -Directory -ErrorAction SilentlyContinue `
                         | ForEach-Object { Join-Path $_.FullName 'python.exe' }
        }
    }
    foreach ($p in $candidates) {
        if ($p -and (Test-Path $p)) { return @{ Bin = $p; Args = @() } }
    }
    return $null
}
$script:PythonResolved = Resolve-PythonBin

function Invoke-Python {
    param([Parameter(ValueFromRemainingArguments)] [string[]] $PyArgs)
    if (-not $script:PythonResolved) {
        throw "Python 3 is required but was not found on PATH. Run 'airc doctor' for install instructions."
    }
    & $script:PythonResolved.Bin @($script:PythonResolved.Args + $PyArgs)
}

# -- Scope / paths ------------------------------------------------------
# Bash: scope = $PWD/.airc (or AIRC_HOME override). Identity is tied to
# the cwd you ran airc from. Multi-tab on one machine = different cwd =
# different peer. We mirror exactly.
function Get-AircScope {
    if ($env:AIRC_HOME) { return $env:AIRC_HOME }
    # Resolve symlinks so /tmp/x and /private/tmp/x are the same scope.
    return (Join-Path (Resolve-Path .).Path '.airc')
}

$AIRC_WRITE_DIR = Get-AircScope
$CONFIG       = Join-Path $AIRC_WRITE_DIR 'config.json'
$IDENTITY_DIR = Join-Path $AIRC_WRITE_DIR 'identity'
$PEERS_DIR    = Join-Path $AIRC_WRITE_DIR 'peers'
$MESSAGES     = Join-Path $AIRC_WRITE_DIR 'messages.jsonl'

# -- Helpers -------------------------------------------------------------

function Die($msg) {
    Write-Error "ERROR: $msg" -ErrorAction Continue
    exit 1
}

function Ensure-Init {
    if (-not (Test-Path $CONFIG)) {
        Die "Not initialized ($AIRC_WRITE_DIR). Run: airc connect"
    }
}

function Get-Timestamp {
    # ISO8601 UTC, second precision (matches bash `date -u +%Y-%m-%dT%H:%M:%SZ`)
    return [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ", [Globalization.CultureInfo]::InvariantCulture)
}

# -- Config (JSON read/write) -------------------------------------------
# Bash uses inline Python for json.load/json.dump. PS has it native.

function Get-ConfigVal {
    param([string]$Key, [string]$Default = '')
    if (-not (Test-Path $CONFIG)) { return $Default }
    try {
        $cfg = Get-Content $CONFIG -Raw | ConvertFrom-Json -AsHashtable
        if ($cfg.ContainsKey($Key)) { return [string]$cfg[$Key] }
    } catch { }
    return $Default
}

function Set-ConfigVal {
    param([Parameter(Mandatory)] [hashtable] $Updates)
    $cfg = [ordered]@{}
    if (Test-Path $CONFIG) {
        try {
            $existing = Get-Content $CONFIG -Raw | ConvertFrom-Json -AsHashtable
            foreach ($k in $existing.Keys) { $cfg[$k] = $existing[$k] }
        } catch { }
    }
    foreach ($k in $Updates.Keys) {
        if ($null -eq $Updates[$k]) { $cfg.Remove($k) } else { $cfg[$k] = $Updates[$k] }
    }
    $json = $cfg | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText($CONFIG, $json, [Text.UTF8Encoding]::new($false))
}

function Get-Name {
    return (Get-ConfigVal -Key 'name' -Default 'unknown')
}

# -- derive_name: cwd basename + 4-char hash ----------------------------
function Derive-Name {
    param([string]$Dir = $null)
    if (-not $Dir) { $Dir = (Resolve-Path .).Path }
    # Prefer git root basename when inside a repo (more meaningful than a
    # subdir basename), but hash the actual cwd so subdirs still differ.
    $baseDir = $Dir
    try {
        $gitRoot = (& git -C $Dir rev-parse --show-toplevel 2>$null)
        if ($LASTEXITCODE -eq 0 -and $gitRoot) { $baseDir = $gitRoot.Trim() }
    } catch { }
    $base = (Split-Path -Leaf $baseDir).ToLower()
    $base = ($base -replace '[^a-z0-9-]', '-')
    if ($base.Length -gt 12) { $base = $base.Substring(0, 12) }
    if (-not $base) { $base = 'airc' }

    $bytes = [Text.Encoding]::UTF8.GetBytes($Dir)
    $sha   = [System.Security.Cryptography.SHA256]::Create().ComputeHash($bytes)
    $hash  = ([BitConverter]::ToString($sha) -replace '-','').ToLower().Substring(0, 4)
    return "$base-$hash"
}

# -- resolve_name: env > config > derive > hostname ---------------------
function Resolve-AircName {
    $name = ''
    if ($env:AIRC_NAME) { $name = $env:AIRC_NAME }
    elseif (Test-Path $CONFIG) { $name = Get-Name }
    if ($name -like '-*') { $name = '' }   # reject flag-shaped (defensive)
    if (-not $name -or $name -eq 'unknown') { $name = Derive-Name }
    if (-not $name) {
        $h = ([Environment]::MachineName).ToLower() -replace '[^a-z0-9-]', '-'
        if ($h.Length -gt 16) { $h = $h.Substring(0, 16) }
        $name = $h
    }
    return $name
}

# -- Tailscale helpers (parity with bash: resolve_tailscale_bin,
#    is_peer_offline_in_tailnet, advise_tailscale_if_down) -----------------
# Extracted into named helpers so Get-AircHost, Invoke-Send, and
# Invoke-Connect all resolve the binary the same way. Mirrors canary's
# 4d41dab / 64b604d / 0f8d8a7.
function Resolve-TailscaleBin {
    # Priority:
    #   1. tailscale / tailscale.exe on PATH
    #   2. C:\Program Files\Tailscale\tailscale.exe
    #   3. C:\Program Files (x86)\Tailscale\tailscale.exe
    foreach ($name in @('tailscale', 'tailscale.exe')) {
        $cmd = Get-Command $name -ErrorAction SilentlyContinue
        if ($cmd) { return $cmd.Source }
    }
    foreach ($p in @(
        'C:\Program Files\Tailscale\tailscale.exe',
        'C:\Program Files (x86)\Tailscale\tailscale.exe'
    )) {
        if (Test-Path $p) { return $p }
    }
    return $null
}

function Test-CgnatIp {
    # Tailscale CGNAT range 100.64.0.0/10 = 100.64.0.0 .. 100.127.255.255
    param([string]$Ip)
    if (-not $Ip) { return $false }
    if ($Ip -match '^100\.(\d+)\.') {
        $second = [int]$matches[1]
        return ($second -ge 64 -and $second -le 127)
    }
    return $false
}

function Test-PeerOfflineInTailnet {
    # Return $true only when we can CONFIRM the peer at the given IP is
    # offline according to our local tailscale status. Used as a fast-path
    # gate in Invoke-Send so a known-offline peer skips the 10s SSH
    # ConnectTimeout and queues straight away. Mirrors bash
    # is_peer_offline_in_tailnet (commit 64b604d).
    param([string]$TargetHost)
    if (-not $TargetHost) { return $false }
    if (-not (Test-CgnatIp -Ip $TargetHost)) { return $false }
    $ts = Resolve-TailscaleBin
    if (-not $ts) { return $false }
    $out = & $ts status 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $out) { return $false }
    foreach ($line in $out) {
        # Plain-text: <IP>  <hostname>  <owner>  <os>  <state...>
        # When a peer is offline the state column has the literal word
        # "offline" on the same line. Match IP at column 1 + word offline.
        $cols = ($line -split '\s+', 2)
        if ($cols.Count -ge 1 -and $cols[0] -eq $TargetHost -and $line -match '\boffline\b') {
            return $true
        }
    }
    return $false
}

function Advise-TailscaleIfDown {
    # When a saved pairing points at a Tailscale CGNAT address and the
    # local Tailscale daemon is NOT running, cmd_connect would silently
    # hang on SSH ConnectTimeout. Instead, print fail-loud instructions
    # and return $true so the caller exits. Mirrors bash
    # advise_tailscale_if_down (commit 0f8d8a7).
    # Returns $true when the caller should ABORT (we printed guidance).
    # Returns $false when it is safe to proceed (non-CGNAT, env override,
    # or tailnet already up).
    param([string]$TargetHost)
    if ($env:AIRC_NO_TAILSCALE -eq '1') { return $false }
    if (-not $TargetHost) { return $false }
    if (-not (Test-CgnatIp -Ip $TargetHost)) { return $false }

    $ts = Resolve-TailscaleBin
    if ($ts) {
        & $ts status 2>$null | Out-Null
        if ($LASTEXITCODE -eq 0) { return $false }   # daemon up, proceed
    }

    Write-Host ''
    Write-Host "X airc: can't reach Tailscale-routed host $TargetHost -- Tailscale appears down on this machine."
    Write-Host ''
    if (-not $ts) {
        Write-Host '   Tailscale is not installed. airc needs it only for cross-machine mesh.'
        Write-Host '   Install:'
        Write-Host '     winget install --id tailscale.tailscale'
        Write-Host '     (or https://tailscale.com/download/windows)'
        Write-Host ''
        Write-Host '   After install, bring the tailnet up and re-run airc join.'
        return $true
    }
    Write-Host '   Tailscale CLI is installed but the daemon is not running. Start it:'
    Write-Host '     (Windows) Click the Tailscale tray icon to start the app.'
    Write-Host '               Or from an elevated PowerShell:  Start-Service Tailscale'
    Write-Host ''
    return $true
}

# -- get_host: tailscale IP > LAN IP > hostname -------------------------
# Priority order matches bash: tailscale IP first (works across the whole
# tailnet), LAN IP next (no Tailscale required for same-LAN mesh), then
# hostname as last resort. AIRC_NO_TAILSCALE=1 forces past tailscale.
function Get-AircHost {
    $tsBin = $null
    if ($env:AIRC_NO_TAILSCALE -ne '1') {
        $tsBin = Resolve-TailscaleBin
    }
    if ($tsBin) {
        try {
            $tsIp = (& $tsBin ip -4 2>$null)
            if ($LASTEXITCODE -eq 0 -and $tsIp) {
                $tsIp = ($tsIp -split "`n")[0].Trim()
                if ($tsIp) { return $tsIp }
            }
        } catch { }
    }
    # LAN IP via UDP-socket trick: connect a UDP socket to a public IP
    # (no packet sent), then ask the local endpoint which interface IP
    # the kernel chose. Same trick the bash version uses inline-Python
    # for. We do it in pure .NET -- cheaper than spawning python.exe.
    try {
        $udp = [System.Net.Sockets.UdpClient]::new()
        $udp.Client.ReceiveTimeout = 500
        $udp.Connect('8.8.8.8', 80)
        $localEp = [System.Net.IPEndPoint]$udp.Client.LocalEndPoint
        $udp.Close()
        $ip = $localEp.Address.ToString()
        if ($ip -and -not $ip.StartsWith('127.') -and $ip -match '^\d+\.\d+\.\d+\.\d+$') {
            return $ip
        }
    } catch { }
    return [Environment]::MachineName.ToLower()
}

# -- humanhash: hex -> 4-word mnemonic ----------------------------------
# Same dictionary + XOR-fold algorithm as the bash version. Bytes are
# split into N segments; each segment XOR-folds to a single byte that
# indexes the 256-word dictionary.
$script:HumanhashDict = @(
    'ack','alabama','alanine','alaska','alpha','angel','apart','april','arizona','arkansas',
    'artist','asparagus','aspen','august','autumn','avocado','bacon','bakerloo','batman','beer',
    'berlin','beryllium','black','blossom','blue','bluebird','bravo','bulldog','burger','butter',
    'california','carbon','cardinal','carolina','carpet','cat','ceiling','cello','center','charlie',
    'chicken','coffee','cola','cold','colorado','comet','connecticut','crazy','cup','dakota',
    'december','delaware','delta','diet','don','double','early','earth','east','echo',
    'edward','eight','eighteen','eleven','emma','enemy','equal','failed','fanta','fillet',
    'finch','fish','five','fix','floor','florida','football','four','fourteen','foxtrot',
    'freddie','friend','fruit','gee','georgia','glucose','golf','green','grey','hamper',
    'happy','harry','hawaii','helium','high','hot','hotel','hydrogen','idaho','illinois',
    'india','indigo','ink','iowa','island','item','jersey','jig','johnny','juliet',
    'july','jupiter','kansas','kentucky','kilo','king','kitten','lactose','lake','lamp',
    'lemon','leopard','lima','lion','lithium','london','louisiana','low','magazine','magnesium',
    'maine','mango','march','mars','maryland','massachusetts','may','mexico','michigan','mike',
    'minnesota','mirror','missouri','mobile','mockingbird','monkey','montana','moon','mountain','muppet',
    'music','nebraska','neptune','network','nevada','nine','nineteen','nitrogen','north','november',
    'nuts','october','ohio','oklahoma','one','orange','oranges','oregon','oscar','oven',
    'oxygen','papa','paris','pasta','pennsylvania','pip','pizza','pluto','potato','princess',
    'purple','quebec','queen','quiet','red','river','robert','robin','romeo','rugby',
    'sad','salami','saturn','september','seven','seventeen','shade','sierra','single','sink',
    'six','sixteen','skylark','snake','social','sodium','solar','south','spaghetti','speaker',
    'spring','stairway','steak','stream','summer','sweet','table','tango','ten','tennessee',
    'tennis','texas','thirteen','three','timing','triple','twelve','twenty','two','uncle',
    'undress','uniform','uranus','utah','vegan','venus','vermont','victor','video','violet',
    'virginia','washington','west','whiskey','white','william','winner','winter','wisconsin','wolfram',
    'wyoming','xray','yankee','yellow','zebra','zulu'
)

function Get-Humanhash {
    param([string]$HexInput, [int]$NWords = 4)
    if (-not $HexInput) { return '' }
    $bytes = New-Object 'System.Collections.Generic.List[int]'
    for ($i = 0; $i -lt $HexInput.Length - 1; $i += 2) {
        $bytes.Add([Convert]::ToInt32($HexInput.Substring($i, 2), 16))
    }
    $nBytes  = $bytes.Count
    if ($nBytes -lt 1) { return '' }
    $segSize = [Math]::Max(1, [int]($nBytes / $NWords))
    $words = @()
    for ($seg = 0; $seg -lt $NWords; $seg++) {
        $acc   = 0
        $start = $seg * $segSize
        $end   = if ($seg -eq $NWords - 1) { $nBytes } else { $start + $segSize }
        for ($j = $start; $j -lt $end -and $j -lt $nBytes; $j++) { $acc = $acc -bxor $bytes[$j] }
        $words += $script:HumanhashDict[$acc]
    }
    return ($words -join '-')
}

# -- openssl wrapper (signing + Ed25519 keygen) -------------------------
# Git for Windows ships openssl at usr/bin/openssl.exe. install.ps1
# guarantees Git is present, but the bin dir isn't always on PATH for
# users running Git via the "Git from PATH" minimal install. Probe the
# usual locations.
function Resolve-OpenSSL {
    $cmd = Get-Command openssl -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    foreach ($p in @(
        "$env:ProgramFiles\Git\usr\bin\openssl.exe",
        "${env:ProgramFiles(x86)}\Git\usr\bin\openssl.exe",
        "$env:LOCALAPPDATA\Programs\Git\usr\bin\openssl.exe"
    )) {
        if ($p -and (Test-Path $p)) { return $p }
    }
    return $null
}
$script:OpenSSLBin = Resolve-OpenSSL

function Invoke-OpenSSL {
    # NOTE: deliberately NO param() block. Adding [Parameter()] makes this
    # an advanced function and PowerShell injects common parameters
    # (-OutBuffer / -OutVariable / etc). Then any openssl flag that has
    # 'out' as a prefix -- e.g. `-out file.pem` -- gets parsed as a
    # PS parameter and fails with the ambiguity error before reaching
    # openssl.exe at all. Use $args to collect verbatim.
    if (-not $script:OpenSSLBin) {
        Die "openssl not found. Install Git for Windows (it bundles openssl) or run 'airc doctor'."
    }
    & $script:OpenSSLBin @args
}

function Sign-Message {
    param([string]$Payload)
    $tmpFile = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllText($tmpFile, $Payload, [Text.UTF8Encoding]::new($false))
        $signedBytes = & $script:OpenSSLBin pkeyutl -sign -inkey (Join-Path $IDENTITY_DIR 'private.pem') -in $tmpFile 2>$null
        # openssl writes binary to stdout; we want base64. Easiest: pipe into openssl base64.
        # PS arg-for-arg: read raw bytes from stdout via -RawObject. Native call captures as
        # System.Object[] of bytes only when -OutputType Byte; cleaner to base64 in a second
        # openssl call.
        $sigB64 = & $script:OpenSSLBin pkeyutl -sign -inkey (Join-Path $IDENTITY_DIR 'private.pem') -in $tmpFile 2>$null `
                  | & $script:OpenSSLBin base64 -A 2>$null
        return ($sigB64 -join '').Trim()
    } finally {
        Remove-Item $tmpFile -Force -ErrorAction SilentlyContinue
    }
}

# -- SSH wrapper --------------------------------------------------------
# Same options as bash relay_ssh: per-identity key, accept-new host keys,
# 10s connect timeout, 30s ServerAliveInterval to keep monitor tails alive.
function Invoke-AircSsh {
    param([Parameter(ValueFromRemainingArguments)] [string[]] $SshArgs)
    $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
    $opts = @(
        '-o','StrictHostKeyChecking=accept-new',
        '-o','ConnectTimeout=10',
        '-o','ServerAliveInterval=30'
    )
    if (Test-Path $sshKey) {
        & ssh '-i' $sshKey @opts @SshArgs
    } else {
        & ssh @opts @SshArgs
    }
}

# Path on the remote where the host's airc state lives (config, messages).
function Get-RemoteHome {
    $h = Get-ConfigVal -Key 'host_airc_home' -Default ''
    if (-not $h) { $h = '$HOME/.airc' }
    return $h
}

# -- Identity init: Ed25519 sign keypair + SSH keypair ------------------
# Ed25519 sign keys for message signing (openssl pkeyutl -sign), separate
# SSH keys for the wire (ssh -i). authorized_keys is appended so the
# joiner can SSH to the host (and vice versa, since each peer also acts
# as sshd via the OS's openssh server -- though for room/joiner mode the
# host is the only one ssh'd-into).
function Init-Identity {
    param([string]$Name)
    foreach ($d in @($AIRC_WRITE_DIR, $IDENTITY_DIR, $PEERS_DIR)) {
        if (-not (Test-Path $d)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }
    }
    $privPem = Join-Path $IDENTITY_DIR 'private.pem'
    $pubPem  = Join-Path $IDENTITY_DIR 'public.pem'
    if (-not (Test-Path $privPem)) {
        Invoke-OpenSSL genpkey -algorithm Ed25519 -out $privPem 2>$null | Out-Null
        Invoke-OpenSSL pkey -in $privPem -pubout -out $pubPem 2>$null | Out-Null
        # chmod 600 equivalent on Windows is an ACL change. The Windows
        # OpenSSH agent strict-perms check rejects keys that are world-
        # readable. We tighten with icacls.
        & icacls $privPem /inheritance:r /grant:r "$($env:USERNAME):F" 2>$null | Out-Null
    }
    $sshKey    = Join-Path $IDENTITY_DIR 'ssh_key'
    $sshKeyPub = "$sshKey.pub"
    if (-not (Test-Path $sshKey)) {
        # ssh-keygen -N '' = empty passphrase (no encryption on the key).
        # Single-quoted empty string in PS is a true zero-length string and
        # survives intact through .NET native-command marshaling. The prior
        # `-N '""'` form passed the literal two-character string `""` as the
        # passphrase on some Windows shells, producing a key that ssh.exe
        # could not use without prompting -- exact symptom: "auth failed"
        # at use time despite the key being in authorized_keys.
        & ssh-keygen -t ed25519 -f $sshKey -N '' -C "airc-$Name" -q
        if (-not (Test-Path $sshKey)) {
            Die "ssh-keygen failed to create $sshKey"
        }
        & icacls $sshKey /inheritance:r /grant:r "$($env:USERNAME):F" 2>$null | Out-Null
        $sshDir = Join-Path $env:USERPROFILE '.ssh'
        if (-not (Test-Path $sshDir)) { New-Item -ItemType Directory -Force -Path $sshDir | Out-Null }
        $authKeys = Join-Path $sshDir 'authorized_keys'
        $pubLine = (Get-Content $sshKeyPub -Raw).Trim()
        $existing = if (Test-Path $authKeys) { Get-Content $authKeys -Raw -ErrorAction SilentlyContinue } else { '' }
        if ($existing -notlike "*$pubLine*") {
            Add-Content -Path $authKeys -Value $pubLine
        }
    }
    if (-not (Test-Path $MESSAGES)) { New-Item -ItemType File -Force -Path $MESSAGES | Out-Null }
}

# Append a key to authorized_keys idempotently. Used for both host->joiner
# and joiner->host directions during the pair handshake.
function Add-AuthorizedKey {
    param([string]$PubKey)
    if (-not $PubKey) { return }
    $sshDir = Join-Path $env:USERPROFILE '.ssh'
    if (-not (Test-Path $sshDir)) { New-Item -ItemType Directory -Force -Path $sshDir | Out-Null }
    $authKeys = Join-Path $sshDir 'authorized_keys'
    $existing = if (Test-Path $authKeys) { Get-Content $authKeys -Raw -ErrorAction SilentlyContinue } else { '' }
    $trimmed = $PubKey.Trim()
    if ($existing -notlike "*$trimmed*") {
        Add-Content -Path $authKeys -Value $trimmed
    }
}

# -- Port helpers -------------------------------------------------------

function Test-PortListening {
    param([int]$Port)
    return [bool] (Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue)
}

function Get-FreeAircPort {
    param([int]$Start = 7547, [int]$Range = 20)
    for ($p = $Start; $p -lt $Start + $Range; $p++) {
        if (-not (Test-PortListening -Port $p)) { return $p }
    }
    Die "No free port in range $Start-$($Start + $Range)."
}

# -- Process tree termination -------------------------------------------
# Bash uses pgrep -P + kill. Windows: taskkill /T /F walks the process
# tree (children of children too) and force-kills. One call.
function Stop-ProcessTree {
    param([int]$ProcId)
    if (-not $ProcId -or $ProcId -le 0) { return }
    & taskkill /PID $ProcId /T /F 2>$null | Out-Null
}

# -- PID file management ------------------------------------------------
# Bash writes "$$ $PAIR_PID" (space-separated). We write one PID per line
# (cleaner and parses identically: split on whitespace).
function Write-AircPidFile {
    param([int[]]$Pids)
    $pidFile = Join-Path $AIRC_WRITE_DIR 'airc.pid'
    Set-Content -Path $pidFile -Value ($Pids -join "`n")
}

function Read-AircPidFile {
    $pidFile = Join-Path $AIRC_WRITE_DIR 'airc.pid'
    if (-not (Test-Path $pidFile)) { return @() }
    $content = Get-Content $pidFile -Raw -ErrorAction SilentlyContinue
    if (-not $content) { return @() }
    return $content -split '\s+' | Where-Object { $_ -match '^\d+$' } | ForEach-Object { [int]$_ }
}

# -- gh wrapper ---------------------------------------------------------
function Test-GhAvailable {
    return [bool] (Get-Command gh -ErrorAction SilentlyContinue)
}

function Get-GhGistList {
    param([int]$Limit = 50)
    if (-not (Test-GhAvailable)) { return @() }
    # `gh gist list --limit N` outputs TAB-separated: id, description, files, visibility, updated
    $raw = & gh gist list --limit $Limit 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $raw) { return @() }
    $rows = @()
    foreach ($line in $raw) {
        if (-not $line) { continue }
        $cols = $line -split "`t"
        if ($cols.Count -lt 2) { continue }
        $rows += [pscustomobject]@{
            Id          = $cols[0]
            Description = $cols[1]
            Updated     = if ($cols.Count -gt 3) { $cols[3] } else { '' }
        }
    }
    return $rows
}

# Fetch the content of the first file in a gist by ID. Uses `gh api` over
# `gh gist view --raw` because the latter prepends the gist description
# ("airc room: general\n\n{...}") which corrupts JSON parsing.
function Get-GistContent {
    param([string]$GistId)
    if (-not (Test-GhAvailable)) { return $null }
    $json = & gh api "gists/$GistId" 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $json) { return $null }
    try {
        $obj = $json | ConvertFrom-Json
        $first = $obj.files.PSObject.Properties | Select-Object -First 1
        if ($first) { return $first.Value.content }
    } catch { return $null }
    return $null
}

# -- monitor_formatter (embedded Python heredoc) ------------------------
# 250+ lines of stateful Python with cross-platform watchdog (SIGALRM
# fallback to threading.Timer), rename protocol, ping/pong handling,
# message filtering, mirror-on-joiner-only, offset tracking. Rewriting
# this in PS would be 600+ lines for no protocol benefit. We keep it
# verbatim from the bash version, with one Windows tweak: the auto-pong
# subprocess uses the airc.cmd shim path (passed via env var).
$script:MonitorFormatterPython = @'
import sys, json, os, re, time, signal, io
# Force UTF-8 on stdout/stderr regardless of Windows locale (default cp1252
# can't encode emoji + a lot of unicode that peers post). errors='replace'
# guarantees the formatter never crashes on a single bad codepoint and
# kill the monitor pipeline. Reconfigure once at startup.
try:
    sys.stdout.reconfigure(encoding='utf-8', errors='replace')
    sys.stderr.reconfigure(encoding='utf-8', errors='replace')
except Exception:
    sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8', errors='replace', line_buffering=True)
    sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding='utf-8', errors='replace', line_buffering=True)

WATCHDOG_SEC = 150
def _watchdog_exit(signum=None, frame=None):
    sys.stderr.write(f"[airc:monitor] no inbound in {WATCHDOG_SEC}s - exiting for probe\n")
    sys.stderr.flush()
    os._exit(2)

# Cross-platform watchdog. POSIX: signal.SIGALRM. Windows: threading.Timer.
try:
    signal.signal(signal.SIGALRM, _watchdog_exit)
    signal.alarm(WATCHDOG_SEC)
    def _arm_watchdog():
        signal.alarm(WATCHDOG_SEC)
except (AttributeError, ValueError):
    import threading
    _wd_timer_holder = [None]
    def _arm_watchdog():
        if _wd_timer_holder[0] is not None:
            _wd_timer_holder[0].cancel()
        t = threading.Timer(WATCHDOG_SEC, _watchdog_exit)
        t.daemon = True
        t.start()
        _wd_timer_holder[0] = t
    _arm_watchdog()

peers_dir   = os.environ.get("PEERS_DIR", "")
scope_dir   = os.path.dirname(peers_dir)
config_path = os.path.join(scope_dir, "config.json")
local_log   = os.path.join(scope_dir, "messages.jsonl")
offset_path = os.path.join(scope_dir, "monitor_offset")
airc_cmd    = os.environ.get("AIRC_CMD_PATH", "airc")  # Windows shim path

is_joiner = False
try:
    is_joiner = bool(json.load(open(config_path)).get("host_target", ""))
except Exception:
    pass

room_path = os.path.join(scope_dir, "room_name")
try:
    room_name = open(room_path).read().strip() or "general"
except Exception:
    room_name = "1:1"

def current_name():
    try:
        return json.load(open(config_path)).get("name", "")
    except Exception:
        return ""

RENAME_RE = re.compile(r"^\[rename\] old=([a-z0-9-]+) new=([a-z0-9-]+)(?:\s+host=(\S+))?")

def _rename_files(old, new):
    old_json = os.path.join(peers_dir, f"{old}.json")
    new_json = os.path.join(peers_dir, f"{new}.json")
    if not os.path.isfile(old_json):
        return False
    try:
        os.rename(old_json, new_json)
        d = json.load(open(new_json))
        d["name"] = new
        json.dump(d, open(new_json, "w"), indent=2)
    except Exception:
        pass
    old_pub = os.path.join(peers_dir, f"{old}.pub")
    new_pub = os.path.join(peers_dir, f"{new}.pub")
    if os.path.isfile(old_pub):
        try: os.rename(old_pub, new_pub)
        except Exception: pass
    return True

def _find_peer_by_host(host):
    if not host or not os.path.isdir(peers_dir):
        return None
    for entry in os.listdir(peers_dir):
        if not entry.endswith(".json"): continue
        try:
            d = json.load(open(os.path.join(peers_dir, entry)))
        except Exception:
            continue
        if d.get("host") == host:
            return d.get("name") or entry[:-5]
    return None

def handle_rename(msg, ts):
    m = RENAME_RE.match(msg)
    if not m: return False
    old, new, host = m.group(1), m.group(2), m.group(3)
    if _rename_files(old, new):
        print(f"airc: nick {old} -> {new}", flush=True)
        return True
    if host:
        current = _find_peer_by_host(host)
        if current and current != new and _rename_files(current, new):
            print(f"airc: nick (chain-repair) {current} -> {new}", flush=True)
            return True
    return False

offset_counter = 0
try:
    with open(offset_path) as f:
        offset_counter = int(f.read().strip() or 0)
except Exception:
    pass

for line in sys.stdin:
    _arm_watchdog()
    line = line.strip()
    if not line: continue
    offset_counter += 1
    try:
        with open(offset_path, "w") as f:
            f.write(str(offset_counter))
    except Exception:
        pass
    try:
        m = json.loads(line)
    except Exception:
        continue
    ts = m.get("ts", "")
    fr = m.get("from", "?")
    to = m.get("to", "")
    msg = m.get("msg", "")
    if fr == current_name():
        continue
    if is_joiner:
        try:
            with open(local_log, "a") as f:
                f.write(line + "\n")
        except Exception:
            pass
    if handle_rename(msg, ts):
        continue
    ping_match = re.match(r"^\[PING:([a-f0-9-]+)\]", msg or "")
    pong_match = re.match(r"^\[PONG:([a-f0-9-]+)\]", msg or "")
    if ping_match:
        ping_id = ping_match.group(1)
        my_current = current_name()
        if to == my_current:
            import subprocess, sys
            try:
                pong_msg = f"[PONG:{ping_id}]"
                # Pass AIRC_HOME explicitly so the subprocess's scope
                # detection lands on THIS scope no matter what cwd it
                # inherits through cmd.exe -> pwsh -> airc.ps1. Without
                # this, cwd ambiguity (Python -> cmd -> .cmd shim ->
                # pwsh) can land the spawned `airc send` in a sibling
                # scope where there's no host_target -- it then writes
                # only to a local mirror and never reaches the wire.
                child_env = os.environ.copy()
                child_env["AIRC_HOME"] = scope_dir
                # Capture auto-pong stderr to a per-scope log so we can
                # diagnose silent failures of the subprocess chain
                # (cmd.exe -> airc.cmd -> pwsh -> airc.ps1 send). Without
                # this, every link in the chain swallows errors with
                # nowhere to surface them. Append-mode so consecutive
                # pings accumulate. Tail with: airc auto-pong-log
                pong_log = os.path.join(scope_dir, "auto_pong.log")
                pong_err = open(pong_log, "ab")
                pong_err.write(f"--- pong attempt for {fr} ping {ping_id} ---\n".encode())
                pong_err.flush()
                if sys.platform == "win32":
                    # Windows CreateProcess can't run .cmd files directly
                    # when shell=False -- it only handles real PE binaries.
                    # Route through cmd.exe /c so airc.cmd interprets
                    # correctly. shell=False is fine here since we control
                    # every argv element (peer name + uuid).
                    subprocess.Popen(
                        ["cmd.exe", "/c", airc_cmd, "send", f"@{fr}", pong_msg],
                        stdout=pong_err,
                        stderr=pong_err,
                        shell=False,
                        env=child_env,
                    )
                else:
                    subprocess.Popen(
                        [airc_cmd, "send", f"@{fr}", pong_msg],
                        stdout=pong_err,
                        stderr=pong_err,
                        shell=False,
                        env=child_env,
                    )
            except Exception:
                pass
        continue
    if pong_match:
        continue
    # No length cap -- consumers (Claude Code Monitor, Codex, log
    # tailers, etc.) decide their own display truncation. Truncating in
    # the substrate forced everyone downstream to fall back to
    # `airc logs` to see the actual content (anti-pattern Joel called
    # out 2026-04-24). Newlines collapsed to spaces so each emitted
    # event is still a single line, but full body always reaches the
    # consumer.
    msg_one_line = (msg or "").replace("\n", " ").replace("\r", " ").strip()
    try:
        if fr in ("airc", "sys"):
            print(f"airc: [#{room_name}] {msg_one_line}", flush=True)
        elif to and to not in ("all", ""):
            print(f"airc: [#{room_name}] {fr} -> {to}: {msg_one_line}", flush=True)
        else:
            print(f"airc: [#{room_name}] {fr}: {msg_one_line}", flush=True)
    except Exception as e:
        # Belt-and-suspenders -- the UTF-8 reconfigure at the top should
        # already neutralize encoding errors, but if some other I/O
        # error fires we never want one bad message to take the whole
        # monitor down. Surface to stderr (which the PS retry loop
        # captures) and keep going.
        try:
            sys.stderr.write(f"[airc:formatter] skipped one line: {e}\n")
            sys.stderr.flush()
        except Exception:
            pass
'@

# Run the formatter against a stream of inbound JSONL lines on its stdin.
# Returns the python.exe exit code (caller maps 2 = watchdog timeout).
function Invoke-MonitorFormatter {
    param([string]$MyName)
    if (-not $script:PythonResolved) { Die 'python missing for monitor formatter' }
    $env:PEERS_DIR     = $PEERS_DIR
    $env:AIRC_CMD_PATH = (Resolve-Path (Join-Path $PSScriptRoot 'airc.cmd') -ErrorAction SilentlyContinue).Path
    if (-not $env:AIRC_CMD_PATH) { $env:AIRC_CMD_PATH = 'airc.cmd' }
    & $script:PythonResolved.Bin @($script:PythonResolved.Args + @('-u', '-c', $script:MonitorFormatterPython))
    return $LASTEXITCODE
}

# -- Monitor: tail messages.jsonl (local for host, remote for joiner) ---
# pipe to formatter, retry on disconnect with a probe-before-spam policy.
# Mirrors bash monitor() closely.
function Start-AircMonitor {
    param([string]$MyName)
    if (-not $script:PythonResolved) {
        Die @"
Python 3 is required for the monitor formatter but was not found.
Run 'airc doctor' for install instructions, or:
  winget install --id Python.Python.3.12
After install, open a NEW terminal so PATH refreshes (or re-run airc).
"@
    }
    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
    $offsetFile = Join-Path $AIRC_WRITE_DIR 'monitor_offset'

    function Get-TailOffset {
        if (Test-Path $offsetFile) {
            $n = (Get-Content $offsetFile -Raw -ErrorAction SilentlyContinue).Trim()
            if ($n -match '^\d+$') { return ([int]$n + 1) }
        }
        return 0   # 0 = "tail from current end"
    }

    if ($hostTarget) {
        $rhome  = Get-RemoteHome
        $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
        $consecutiveTimeouts = 0
        $ESCALATE_AFTER = 2   # 2 * WATCHDOG_SEC = 5min dead-host detection

        while ($true) {
            $cycleStart = Get-Date
            $offset = Get-TailOffset
            $tailFlag = if ($offset -gt 0) { "+$offset" } else { '0' }
            $remoteCmd = "tail -n $tailFlag -F $rhome/messages.jsonl 2>/dev/null"

            # ssh.exe stdout -> python formatter stdin. Native PS pipeline.
            $env:PEERS_DIR     = $PEERS_DIR
            $env:AIRC_CMD_PATH = (Resolve-Path (Join-Path $PSScriptRoot 'airc.cmd') -ErrorAction SilentlyContinue).Path
            if (-not $env:AIRC_CMD_PATH) { $env:AIRC_CMD_PATH = 'airc.cmd' }
            $tailArgs = @(
                '-i', $sshKey,
                '-o', 'StrictHostKeyChecking=accept-new',
                '-o', 'ServerAliveInterval=30',
                '-o', 'ServerAliveCountMax=3',
                $hostTarget, $remoteCmd
            )
            # Capture ssh stderr to a per-scope log so we can diagnose
            # silent failures of the long-running tail.
            $sshErr = Join-Path $AIRC_WRITE_DIR 'monitor_ssh.log'

            # PowerShell's native-command `|` pipeline buffers text between
            # ssh.exe and python.exe in a way that never flushes on a
            # long-running stream producer -- the formatter received ZERO
            # stdin bytes for 150s while ssh's stdout had plenty (host was
            # posting every few seconds). Watchdog fired every cycle and
            # nothing ever got mirrored or auto-ponged.
            #
            # Replace the PS pipeline with explicit [Diagnostics.Process]
            # handles + an async stream copy. ssh stdout reads go straight
            # to python stdin with no PS/StringObject layer in between.
            $sshInfo = [System.Diagnostics.ProcessStartInfo]::new('ssh.exe')
            foreach ($a in $tailArgs) { [void]$sshInfo.ArgumentList.Add($a) }
            $sshInfo.RedirectStandardOutput = $true
            $sshInfo.RedirectStandardError  = $true
            $sshInfo.UseShellExecute        = $false
            $sshInfo.CreateNoWindow         = $true
            $sshProc = [System.Diagnostics.Process]::new()
            $sshProc.StartInfo = $sshInfo
            [void]$sshProc.Start()

            # Formatter lives on disk as a .py file so we can pass it as
            # argv (cleaner than -c with a multi-line heredoc that PS
            # might re-escape through CreateProcess).
            $pyFile = Join-Path $AIRC_WRITE_DIR 'monitor_formatter.py'
            [System.IO.File]::WriteAllText(
                $pyFile,
                $script:MonitorFormatterPython,
                [System.Text.UTF8Encoding]::new($false)
            )
            $pyInfo = [System.Diagnostics.ProcessStartInfo]::new($script:PythonResolved.Bin)
            foreach ($pa in ($script:PythonResolved.Args + @('-u', $pyFile))) {
                [void]$pyInfo.ArgumentList.Add($pa)
            }
            $pyInfo.RedirectStandardInput = $true
            $pyInfo.UseShellExecute       = $false
            $pyInfo.CreateNoWindow        = $true
            # Pass through PEERS_DIR + AIRC_CMD_PATH explicitly.
            $pyInfo.EnvironmentVariables['PEERS_DIR']     = $env:PEERS_DIR
            $pyInfo.EnvironmentVariables['AIRC_CMD_PATH'] = $env:AIRC_CMD_PATH
            # Force UTF-8 IO so peers' emoji / non-cp1252 chars can't crash
            # the formatter. Belt-and-suspenders alongside the
            # sys.stdout.reconfigure call inside the heredoc.
            $pyInfo.EnvironmentVariables['PYTHONIOENCODING'] = 'utf-8'
            $pyInfo.EnvironmentVariables['PYTHONUTF8']       = '1'
            $pyProc = [System.Diagnostics.Process]::new()
            $pyProc.StartInfo = $pyInfo
            [void]$pyProc.Start()

            # Async-forward ssh stderr to the log file (non-blocking).
            $sshErrStream = [System.IO.File]::Open($sshErr, [System.IO.FileMode]::Create)
            $sshErrTask = $sshProc.StandardError.BaseStream.CopyToAsync($sshErrStream)

            # Pump ssh stdout -> python stdin synchronously; this is the
            # hot path for every inbound line from the remote tail.
            try {
                while ($true) {
                    $line = $sshProc.StandardOutput.ReadLine()
                    if ($null -eq $line) { break }
                    $pyProc.StandardInput.WriteLine($line)
                    $pyProc.StandardInput.Flush()
                }
            } catch { } finally {
                try { $pyProc.StandardInput.Close() } catch { }
            }
            $pyProc.WaitForExit()
            $fmtExit = $pyProc.ExitCode
            try { $sshProc.WaitForExit(1000) | Out-Null } catch { }
            if (-not $sshProc.HasExited) { try { $sshProc.Kill() } catch { } }
            try { $sshErrTask.Wait(1000) | Out-Null } catch { }
            try { $sshErrStream.Close() } catch { }
            $cycleLifetime = ((Get-Date) - $cycleStart).TotalSeconds

            if ($fmtExit -eq 2) {
                # Probe-before-spam: distinguish healthy idle from dead host.
                $probeOk = $false
                try {
                    $probeArgs = @(
                        '-i', $sshKey,
                        '-o', 'StrictHostKeyChecking=accept-new',
                        '-o', 'ConnectTimeout=5',
                        '-o', 'BatchMode=yes',
                        $hostTarget, 'true'
                    )
                    $r = & ssh @probeArgs 2>$null
                    if ($LASTEXITCODE -eq 0) { $probeOk = $true }
                } catch { }
                if ($probeOk) {
                    $consecutiveTimeouts = 0   # healthy idle, stay quiet
                } else {
                    Write-Host 'airc: host went quiet (probe failed) - restarting'
                    $consecutiveTimeouts++
                }
            } elseif ($cycleLifetime -lt 30) {
                Write-Host 'airc: host unreachable (cycle <30s) - restarting'
                $consecutiveTimeouts++
            } else {
                $consecutiveTimeouts = 0
            }

            if ($consecutiveTimeouts -ge $ESCALATE_AFTER) {
                $savedRoom = ''
                $roomFile = Join-Path $AIRC_WRITE_DIR 'room_name'
                if (Test-Path $roomFile) { $savedRoom = (Get-Content $roomFile -Raw).Trim() }
                if ($savedRoom) {
                    Write-Error "Host of #$savedRoom dead for $consecutiveTimeouts cycles - exiting for daemon respawn / self-heal"
                    exit 99
                } else {
                    Write-Warning "$consecutiveTimeouts watchdog timeouts on legacy invite scope - host may be down"
                    $consecutiveTimeouts = 0
                }
            }
            Start-Sleep -Seconds 3
        }
    } else {
        # Host mode: tail our own messages.jsonl (no SSH).
        while ($true) {
            $offset = Get-TailOffset
            $tailFlag = if ($offset -gt 0) { "+$offset" } else { '0' }
            # Use Get-Content -Wait for `tail -F` semantics on Windows. We
            # apply our own offset by skipping the first $offset lines.
            $env:PEERS_DIR     = $PEERS_DIR
            $env:AIRC_CMD_PATH = (Resolve-Path (Join-Path $PSScriptRoot 'airc.cmd') -ErrorAction SilentlyContinue).Path
            if (-not $env:AIRC_CMD_PATH) { $env:AIRC_CMD_PATH = 'airc.cmd' }
            try {
                Get-Content -Path $MESSAGES -Wait -Tail 0 `
                  | & $script:PythonResolved.Bin @($script:PythonResolved.Args + @('-u', '-c', $script:MonitorFormatterPython))
            } catch { }
            Start-Sleep -Seconds 1
        }
    }
}

# ========================================================================
# COMMANDS
# ========================================================================

# -- cmd_version --------------------------------------------------------
function Invoke-Version {
    $here = $PSScriptRoot
    $dir = $null
    if ($here -and (Test-Path (Join-Path $here '.git'))) { $dir = $here }
    elseif ($env:AIRC_DIR -and (Test-Path (Join-Path $env:AIRC_DIR '.git'))) { $dir = $env:AIRC_DIR }
    elseif (Test-Path (Join-Path $env:USERPROFILE '.airc-src\.git')) { $dir = (Join-Path $env:USERPROFILE '.airc-src') }

    if (-not $dir) {
        Write-Host "  airc $AIRC_FALLBACK_VERSION (no git metadata)"
        return
    }
    $sha     = (& git -C $dir rev-parse --short HEAD 2>$null)
    $subject = (& git -C $dir log -1 --format=%s 2>$null)
    $branch  = (& git -C $dir rev-parse --abbrev-ref HEAD 2>$null)
    $dirty   = ''
    if (& git -C $dir status --porcelain 2>$null) { $dirty = ' (dirty)' }
    Write-Host "  airc $sha$dirty on $branch"
    if ($subject) { Write-Host "  $subject" }
    Write-Host "  install: $dir"
}

# -- cmd_help -----------------------------------------------------------
function Invoke-Help {
    @'
AIRC - Agentic Internet Relay Chat for AI peers
(Windows-native; bash port lives on the same canary)

Common verbs (IRC-canonical, all aliases work):
  airc join                       Auto-#general (joins existing or hosts)
  airc join --room <name>         Enter (or host) a named channel
  airc join <gist-id>             Enter via shared gist id (cross-account)
  airc join <mnemonic>            Enter via humanhash like oregon-uncle-bravo-eleven
  airc list / rooms               List open rooms + invites on your gh account
  airc part                       Leave the current room
  airc msg <text>                 Broadcast to the current room
  airc msg @<peer> <text>         DM a peer
  airc nick <new>                 Rename this session (notifies peers)
  airc quit / disconnect          Leave the mesh (keep identity)
  airc peers                      List connected peers
  airc ping @peer [timeout]       Monitor-liveness probe

Operations:
  airc doctor                     Check prereqs + health (gh, ssh, python, tailscale, ...)
  airc status [--probe]           Liveness snapshot
  airc update [--channel <name>]  Pull latest; switch channel with --channel canary|main
  airc channel [<name>]           Show or set release channel
  airc canary                     Shortcut: airc update --channel canary
  airc daemon [install|...]       Auto-start via Task Scheduler (Windows)
  airc reminder <seconds>         Nudge if silent (off / pause / 300)
  airc teardown [--flush] [--all] Kill scope airc processes
  airc invite                     Print join string (share with peers)
  airc logs [N]                   Show recent messages

Identity resolution (highest priority first):
  AIRC_NAME env > config.json name > cwd basename > hostname
  AIRC_HOME env overrides state dir (default $PWD/.airc)
  AIRC_PORT env overrides host listen port (default 7547)
  AIRC_REMINDER env overrides reminder interval seconds (default 300)
  Join string may include :port - name@user@host:7548#key
'@ | Write-Host
}

# -- cmd_doctor ---------------------------------------------------------
# User-emphasized: "if they dont have gh for example doctor would say
# hey get that". Concrete winget commands so the user can copy-paste.
function Invoke-Doctor {
    Write-Host ''
    Write-Host '  airc doctor - environment health'
    Write-Host '  --------------------------------'
    Write-Host ''
    $issues = @()

    function Probe($Name, $TestBlock, $FixHint) {
        if (& $TestBlock) {
            Write-Host "  [ok] $Name"
        } else {
            Write-Host "  [MISSING] $Name"
            Write-Host "         Fix: $FixHint"
            $script:DoctorIssues += $Name
        }
    }
    $script:DoctorIssues = @()

    Probe 'PowerShell 7+' {
        $PSVersionTable.PSVersion.Major -ge 7
    } 'winget install --id Microsoft.PowerShell  (then re-launch in pwsh)'

    Probe 'git' {
        Get-Command git -ErrorAction SilentlyContinue
    } 'winget install --id Git.Git'

    Probe 'python' {
        $r = Resolve-PythonBin
        $null -ne $r
    } 'winget install --id Python.Python.3.12'

    Probe 'gh (GitHub CLI)' {
        Get-Command gh -ErrorAction SilentlyContinue
    } 'winget install --id GitHub.cli  (then: gh auth login -s gist)'

    Probe 'gh authenticated (gist scope)' {
        if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { return $false }
        & gh auth status 2>$null | Out-Null
        $LASTEXITCODE -eq 0
    } 'gh auth login -s gist'

    Probe 'ssh (OpenSSH client)' {
        Get-Command ssh -ErrorAction SilentlyContinue
    } 'Settings -> Apps -> Optional Features -> Add -> OpenSSH Client (or: Add-WindowsCapability -Online -Name OpenSSH.Client*)'

    Probe 'ssh-keygen' {
        Get-Command ssh-keygen -ErrorAction SilentlyContinue
    } 'Comes with OpenSSH Client (see above)'

    Probe 'openssl' {
        $null -ne $script:OpenSSLBin
    } 'winget install --id Git.Git  (Git for Windows bundles openssl in usr/bin)'

    Probe 'tailscale (optional)' {
        Get-Command tailscale -ErrorAction SilentlyContinue
    } 'winget install --id tailscale.tailscale  (then: tailscale up)  - LAN-only mode works without it'

    # State-dir + identity
    Write-Host ''
    Write-Host '  Scope:'
    Write-Host "    AIRC_HOME = $AIRC_WRITE_DIR"
    if (Test-Path $CONFIG) {
        $name = Get-Name
        $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
        if ($hostTarget) {
            Write-Host "    Identity: $name (joiner of $hostTarget)"
        } else {
            Write-Host "    Identity: $name (host or unconnected)"
        }
    } else {
        Write-Host "    Identity: not initialized (run 'airc connect' to set up)"
    }

    Write-Host ''
    if ($script:DoctorIssues.Count -eq 0) {
        Write-Host '  All required prereqs present. You are ready to: airc join'
    } else {
        Write-Host "  $($script:DoctorIssues.Count) prereq(s) missing - see fix lines above."
        Write-Host '  Fastest path: re-run install.ps1 (it auto-installs via winget):'
        Write-Host '    iwr https://raw.githubusercontent.com/CambrianTech/airc/canary/install.ps1 | iex'
    }
    Write-Host ''
}

# -- cmd_peers ----------------------------------------------------------
function Invoke-Peers {
    Ensure-Init
    if (-not (Test-Path $PEERS_DIR)) { Write-Host '  No peers yet.'; return }
    $entries = Get-ChildItem -Path $PEERS_DIR -Filter '*.json' -File -ErrorAction SilentlyContinue
    if (-not $entries -or $entries.Count -eq 0) { Write-Host '  No peers yet.'; return }
    foreach ($f in $entries) {
        try {
            $d = Get-Content $f.FullName -Raw | ConvertFrom-Json
            Write-Host "  $($d.name) -> $($d.host)"
        } catch {
            Write-Host "  (malformed: $($f.Name))"
        }
    }
}

# -- cmd_invite ---------------------------------------------------------
function Invoke-Invite {
    Ensure-Init
    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
    if ($hostTarget) {
        # Joiner: reconstruct the host's join string from config so other
        # peers can converge on the same host.
        $hostName    = Get-ConfigVal -Key 'host_name'    -Default ''
        $hostPort    = Get-ConfigVal -Key 'host_port'    -Default '7547'
        $hostSshPub  = Get-ConfigVal -Key 'host_ssh_pub' -Default ''
        if (-not $hostName -or -not $hostSshPub) {
            Die 'Host info missing from config. Re-pair with airc teardown then airc connect <join>.'
        }
        $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($hostSshPub.Trim()))
        $portSuffix = if ($hostPort -ne '7547') { ":$hostPort" } else { '' }
        Write-Host "$hostName@$hostTarget$portSuffix#$b64"
    } else {
        $name  = Resolve-AircName
        $user  = $env:USERNAME
        $hostA = Get-AircHost
        $port  = if (Test-Path (Join-Path $AIRC_WRITE_DIR 'host_port')) {
            (Get-Content (Join-Path $AIRC_WRITE_DIR 'host_port') -Raw).Trim()
        } else { '7547' }
        $sshPub = (Get-Content (Join-Path $IDENTITY_DIR 'ssh_key.pub') -Raw -ErrorAction SilentlyContinue).Trim()
        if (-not $sshPub) { Die "No SSH key yet (run 'airc connect' to host)." }
        $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sshPub))
        $portSuffix = if ($port -ne '7547') { ":$port" } else { '' }
        Write-Host "$name@$user@$hostA$portSuffix#$b64"
    }
}

# -- cmd_rooms / cmd_list -----------------------------------------------
function Invoke-Rooms {
    if (-not (Test-GhAvailable)) {
        Write-Error 'airc rooms requires gh CLI: winget install --id GitHub.cli'
        return
    }
    $rows = Get-GhGistList -Limit 50
    $matches = @()
    foreach ($r in $rows) {
        if ($r.Description -like 'airc room:*') {
            $matches += [pscustomobject]@{ Kind = 'room'; Id = $r.Id; Description = $r.Description; Updated = $r.Updated }
        } elseif ($r.Description -like 'airc invite for*') {
            $matches += [pscustomobject]@{ Kind = 'invite'; Id = $r.Id; Description = $r.Description; Updated = $r.Updated }
        }
    }
    if ($matches.Count -eq 0) {
        Write-Host '  No open airc rooms or invites on your gh account.'
        Write-Host '  Host the default room:  airc connect'
        Write-Host '  Host a named room:      airc connect --room <name>'
        return
    }
    Write-Host ''
    Write-Host "  $($matches.Count) open on your gh account:"
    Write-Host ''
    foreach ($m in $matches) {
        $marker = if ($m.Kind -eq 'room') { '#' } else { '(1:1)' }
        $hh = Get-Humanhash -HexInput $m.Id
        Write-Host "    $marker $($m.Description)"
        Write-Host "      id:       $($m.Id)"
        Write-Host "      mnemonic: $hh"
        Write-Host "      updated:  $($m.Updated)"
        Write-Host ''
    }
    Write-Host '  Join (auto on same gh account): airc connect'
    Write-Host '  Join by id (cross-account):     airc connect <id>'
    Write-Host ''
}

# -- cmd_part -----------------------------------------------------------
function Invoke-Part {
    Ensure-Init
    $gistIdFile  = Join-Path $AIRC_WRITE_DIR 'room_gist_id'
    $roomFile    = Join-Path $AIRC_WRITE_DIR 'room_name'
    $roomName    = if (Test-Path $roomFile) { (Get-Content $roomFile -Raw).Trim() } else { '(unnamed)' }
    $hostTarget  = Get-ConfigVal -Key 'host_target' -Default ''

    if (-not $hostTarget) {
        # Host path: delete the room gist if we created one
        if (Test-Path $gistIdFile) {
            $gid = (Get-Content $gistIdFile -Raw).Trim()
            if (Test-GhAvailable) {
                Write-Host "  Host of #$roomName parting - deleting room gist $gid ..."
                & gh gist delete $gid --yes 2>$null
                if ($LASTEXITCODE -eq 0) { Write-Host '  + Room gist deleted.' }
                else { Write-Host "  ! Couldn't delete gist $gid (already gone? gh auth?). Continuing teardown." }
            } else {
                Write-Host "  ! gh CLI not available - delete manually: gh gist delete $gid --yes"
            }
        } else {
            Write-Host "  Host of #$roomName parting (no gist published; nothing to clean up in gh)."
        }
        Remove-Item $gistIdFile, $roomFile -Force -ErrorAction SilentlyContinue
    } else {
        Write-Host "  Joiner of #$roomName parting - host gist stays open for others."
        Remove-Item $roomFile -Force -ErrorAction SilentlyContinue
    }
    Invoke-Teardown
}

# -- cmd_channel --------------------------------------------------------
function Invoke-Channel {
    param([string[]]$Argv)
    $dir = if ($env:AIRC_DIR) { $env:AIRC_DIR } else { Join-Path $env:USERPROFILE '.airc-src' }
    $channelFile = Join-Path $dir '.channel'
    $current = if (Test-Path $channelFile) { (Get-Content $channelFile -Raw).Trim() } else { 'main' }
    if (-not $current) { $current = 'main' }
    $target = if ($Argv -and $Argv.Count -gt 0) { $Argv[0] } else { '' }
    if (-not $target) {
        Write-Host "  Channel: $current"
        Write-Host '  Available channels (any branch on origin can be a channel):'
        Write-Host '    main      - stable, what most users run'
        Write-Host '    canary    - features queued for the next main merge; opt-in testing'
        Write-Host '  Switch:'
        Write-Host '    airc channel <name>           # set preference (run airc update after)'
        Write-Host '    airc update --channel <name>  # set + pull in one step'
        return
    }
    Set-Content -Path $channelFile -Value $target -NoNewline
    Write-Host "  Channel preference set: '$target'. Run 'airc update' to actually switch + pull."
}

# -- cmd_logs -----------------------------------------------------------
function Invoke-Logs {
    param([string[]]$Argv)
    Ensure-Init
    $count = 20
    if ($Argv -and $Argv.Count -gt 0 -and $Argv[0] -match '^\d+$') { $count = [int]$Argv[0] }
    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
    if ($hostTarget) {
        $rhome = Get-RemoteHome
        $raw = Invoke-AircSsh $hostTarget "tail -$count $rhome/messages.jsonl 2>/dev/null"
    } else {
        $raw = Get-Content $MESSAGES -Tail $count -ErrorAction SilentlyContinue
    }
    foreach ($line in $raw) {
        if (-not $line) { continue }
        try {
            $m = $line | ConvertFrom-Json
            Write-Host "[$($m.ts)] $($m.from): $($m.msg)"
        } catch { }
    }
}

# -- cmd_reminder -------------------------------------------------------
function Invoke-Reminder {
    param([string[]]$Argv)
    Ensure-Init
    $arg = if ($Argv -and $Argv.Count -gt 0) { $Argv[0] } else { 'status' }
    $reminderFile = Join-Path $AIRC_WRITE_DIR 'reminder'
    switch -Regex ($arg) {
        '^(off|0)$'  {
            Remove-Item $reminderFile -Force -ErrorAction SilentlyContinue
            Write-Host '  Reminders off.'
        }
        '^pause$' {
            Set-Content -Path $reminderFile -Value '0' -NoNewline
            Write-Host "  Reminders paused. 'airc reminder <seconds>' to resume."
        }
        '^status$' {
            if (Test-Path $reminderFile) {
                $val = (Get-Content $reminderFile -Raw).Trim()
                if ($val -eq '0') { Write-Host '  Reminders paused.' }
                else { Write-Host "  Reminder every ${val}s." }
            } else { Write-Host '  Reminders off.' }
        }
        '^\d+$' {
            Set-Content -Path $reminderFile -Value $arg -NoNewline
            Write-Host "  Reminder every ${arg}s if no messages."
        }
        default { Die "Usage: airc reminder [off|pause|<seconds>]" }
    }
}

# -- cmd_status ---------------------------------------------------------
function Invoke-Status {
    param([string[]]$Argv)
    Ensure-Init
    $probe = ($Argv -and $Argv -contains '--probe')
    $myName     = Get-Name
    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
    $hostName   = Get-ConfigVal -Key 'host_name'   -Default ''
    $hostPort   = Get-ConfigVal -Key 'host_port'   -Default '7547'

    Write-Host "  airc status - scope $AIRC_WRITE_DIR"
    if ($hostTarget) {
        Write-Host "  identity:    $myName (joiner of $hostName @ ${hostTarget}:${hostPort})"
    } else {
        $myPort = if ($env:AIRC_PORT) { $env:AIRC_PORT } else { '7547' }
        $portFile = Join-Path $AIRC_WRITE_DIR 'host_port'
        if (Test-Path $portFile) { $myPort = (Get-Content $portFile -Raw).Trim() }
        Write-Host "  identity:    $myName (hosting on port $myPort)"
    }

    # Monitor liveness via PID file
    $pids = Read-AircPidFile
    $alive = $null
    foreach ($p in $pids) {
        if (Get-Process -Id $p -ErrorAction SilentlyContinue) { $alive = $p; break }
    }
    if ($alive) {
        Write-Host "  monitor:     running (PID $alive)"
    } elseif ($pids.Count -gt 0) {
        Write-Host "  monitor:     stale pidfile (PIDs $($pids -join ' ') not alive - run 'airc connect' to self-heal)"
    } else {
        Write-Host '  monitor:     not running'
    }

    # Host reachability (only joiners; opt-in via --probe)
    if ($hostTarget -and $probe) {
        $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
        $statusProbeArgs = @(
            '-i', $sshKey,
            '-o', 'StrictHostKeyChecking=accept-new',
            '-o', 'ConnectTimeout=3',
            '-o', 'BatchMode=yes',
            $hostTarget, 'echo __REACHABLE__'
        )
        $r = & ssh @statusProbeArgs 2>$null
        if ($r -match '__REACHABLE__') {
            Write-Host '  host:        reachable'
        } else {
            Write-Host '  host:        UNREACHABLE (ssh timeout or auth failure)'
        }
    }

    # Pending queue
    $pending = Join-Path $AIRC_WRITE_DIR 'pending.jsonl'
    $pcount = 0
    if (Test-Path $pending) { $pcount = (Get-Content $pending -ErrorAction SilentlyContinue).Count }
    if ($pcount -gt 0) { Write-Host "  queue:       $pcount pending (auto-retries every ~5s)" }
    else { Write-Host '  queue:       empty' }

    # Reminder state
    $rf = Join-Path $AIRC_WRITE_DIR 'reminder'
    if (Test-Path $rf) {
        $rv = (Get-Content $rf -Raw).Trim()
        if ($rv -eq '0') { Write-Host '  reminder:    paused' }
        elseif ($rv -match '^\d+$') { Write-Host "  reminder:    every ${rv}s" }
    } else {
        Write-Host '  reminder:    off'
    }
}

# -- cmd_teardown -------------------------------------------------------
# Bash uses pgrep -P + kill. Windows: taskkill /T /F walks the tree.
function Invoke-Teardown {
    param([string[]]$Argv)
    $flush = ($Argv -contains '--flush')
    $all   = ($Argv -contains '--all')
    $killed = $false

    if ($all) {
        # Nuclear: every airc-related pwsh, ssh, python on this user. Best-
        # effort. Match by command line containing 'airc'.
        $procs = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | Where-Object {
            $_.CommandLine -and $_.CommandLine -match '\bairc\b' -and $_.ProcessId -ne $PID
        }
        foreach ($p in $procs) {
            Write-Host "  --all: killing PID $($p.ProcessId) ($($p.Name))"
            Stop-ProcessTree -ProcId $p.ProcessId
            $killed = $true
        }
        if (-not $killed) { Write-Host '  --all: no machine-wide airc processes to kill.' }
    }

    # Scope-aware via PID file
    $pids = Read-AircPidFile
    if ($pids.Count -gt 0) {
        $alivePids = @()
        foreach ($p in $pids) {
            if (Get-Process -Id $p -ErrorAction SilentlyContinue) { $alivePids += $p }
        }
        if ($alivePids.Count -gt 0) {
            Write-Host "  killing scope $AIRC_WRITE_DIR : $($alivePids -join ' ')"
            foreach ($p in $alivePids) { Stop-ProcessTree -ProcId $p }
            $killed = $true
        }
        Remove-Item (Join-Path $AIRC_WRITE_DIR 'airc.pid') -Force -ErrorAction SilentlyContinue
    }

    # Free stale TCP listeners on the airc port range that look orphaned
    # (no parent process still alive).
    $portCandidates = @(7547)
    if ($env:AIRC_PORT -and $env:AIRC_PORT -ne '7547') { $portCandidates = @([int]$env:AIRC_PORT, 7547) }
    foreach ($port in $portCandidates) {
        $conns = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue
        foreach ($c in $conns) {
            $owner = Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue
            if (-not $owner) { continue }
            # Heuristic: only kill if the owner is pwsh/python/airc-related.
            if ($owner.Name -match '^(pwsh|python|powershell)$') {
                Write-Host "  freeing stale port $port (PID $($c.OwningProcess) - $($owner.Name))"
                Stop-ProcessTree -ProcId $c.OwningProcess
                $killed = $true
            }
        }
    }

    if ($flush) {
        if ($AIRC_WRITE_DIR -and (Test-Path $AIRC_WRITE_DIR)) {
            Write-Host "  flushing state: $AIRC_WRITE_DIR"
            Remove-Item -Recurse -Force $AIRC_WRITE_DIR -ErrorAction SilentlyContinue
        }
    }

    if ($killed) { Write-Host '  Teardown complete.' }
    else { Write-Host '  No airc processes running.' }
}

# -- cmd_disconnect -----------------------------------------------------
function Invoke-Disconnect {
    Invoke-Teardown | Out-Null
    if (Test-Path $CONFIG) {
        try {
            $cfg = Get-Content $CONFIG -Raw | ConvertFrom-Json -AsHashtable
            foreach ($k in @('host_target','host_name','host_airc_home','host_port','host_ssh_pub')) {
                $cfg.Remove($k) | Out-Null
            }
            ($cfg | ConvertTo-Json -Depth 10) | Set-Content -Path $CONFIG -NoNewline
        } catch { }
    }
    Write-Host "  Disconnected. Identity preserved. Next 'airc connect' starts fresh (not a resume)."
}

# -- cmd_rename / cmd_nick ----------------------------------------------
function Invoke-Rename {
    param([string[]]$Argv)
    $newName = if ($Argv -and $Argv.Count -gt 0) { $Argv[0] } else { '' }
    if (-not $newName -or $newName -in @('-h','--help')) {
        Write-Host 'Usage: airc nick <new-name>'
        Write-Host '  Renames this identity and broadcasts [rename] to peers.'
        if (-not $newName) { exit 1 } else { return }
    }
    if ($newName.StartsWith('-')) { Die "Name must not start with '-' (got '$newName')" }
    $newName = ($newName.ToLower() -replace '[^a-z0-9-]','-')
    if ($newName.Length -gt 24) { $newName = $newName.Substring(0, 24) }
    if (-not $newName) { Die 'Invalid name (must be a-z 0-9 -)' }
    if (-not (Test-Path $CONFIG)) { Die "Not initialized - run 'airc connect' first" }
    $oldName = Get-Name
    if ($oldName -eq $newName) { Write-Host "  Already named '$newName'."; return }
    Set-ConfigVal -Updates @{ name = $newName }
    Write-Host "  Renamed: $oldName -> $newName"
    $myHost = "$($env:USERNAME)@$(Get-AircHost)"
    try { Invoke-Send -Argv @("[rename] old=$oldName new=$newName host=$myHost") | Out-Null } catch { }
}

# -- cmd_send / cmd_msg -------------------------------------------------
# Local-mirror-first, then SSH append to host's messages.jsonl.
# Auth-vs-network failure distinction (per fix #17).
function Invoke-Send {
    param([string[]]$Argv)
    if (-not $Argv -or $Argv.Count -eq 0) {
        Die 'Usage: airc msg <message>  or  airc msg @peer <message>'
    }
    $first = $Argv[0]
    $peerName = 'all'
    $msg = ''
    if ($first.StartsWith('@')) {
        # Two valid shapes:
        #   airc msg @peer body words ...    (shell-split: 2+ args)
        #   airc msg "@peer body words ..."  (whole thing one arg, e.g.
        #                                     when called via cmd.exe
        #                                     wrapper or with a quoted
        #                                     argument from PowerShell)
        # Detect the single-arg case by checking whether $first contains
        # whitespace; split on the first run of whitespace if so.
        if ($Argv.Count -ge 2) {
            $peerName = $first.Substring(1)
            $msg = ($Argv[1..($Argv.Count - 1)] -join ' ')
        } elseif ($first -match '^@(\S+)\s+(.+)$') {
            $peerName = $matches[1]
            $msg = $matches[2]
        } else {
            Die 'Usage: airc msg @peer <message>'
        }
    } else {
        $msg = ($Argv -join ' ')
    }
    Ensure-Init

    $myName = Get-Name
    $ts     = Get-Timestamp
    $payloadObj = [ordered]@{ from = $myName; to = $peerName; ts = $ts; msg = $msg }
    $payload    = $payloadObj | ConvertTo-Json -Compress

    $sig = Sign-Message -Payload $payload
    $signedObj = [ordered]@{ from = $myName; to = $peerName; ts = $ts; msg = $msg; sig = $sig }
    $fullMsg   = $signedObj | ConvertTo-Json -Compress

    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''

    if ($hostTarget) {
        $rhome = Get-RemoteHome
        # Mirror locally FIRST so we always have an audit trail.
        Add-Content -Path $MESSAGES -Value $fullMsg

        # Fast-path: if the target is a Tailscale CGNAT IP and tailscale
        # status already reports the peer as offline, skip the 10s SSH
        # ConnectTimeout and queue immediately with a cleaner marker.
        # flush_pending_loop + monitor reconnect handle the drain when
        # the peer wakes. Mirrors bash 64b604d.
        if (Test-PeerOfflineInTailnet -TargetHost $hostTarget) {
            $pending = Join-Path $AIRC_WRITE_DIR 'pending.jsonl'
            Add-Content -Path $pending -Value $fullMsg
            $marker = ([ordered]@{
                from = 'airc'
                ts   = (Get-Timestamp)
                msg  = "[QUEUED to $peerName - peer offline in tailnet, auto-delivers on wake]"
            } | ConvertTo-Json -Compress)
            Add-Content -Path $MESSAGES -Value $marker
            # Reset reminder state (we did send something, just queued)
            $now = [int][double]::Parse(((Get-Date).ToUniversalTime() - [DateTime]'1970-01-01').TotalSeconds)
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'last_sent') -Value $now -NoNewline
            Remove-Item (Join-Path $AIRC_WRITE_DIR 'reminded') -Force -ErrorAction SilentlyContinue
            return
        }

        $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
        $remoteCmd = "cat >> $rhome/messages.jsonl && echo __APPENDED__"
        $errFile = [System.IO.Path]::GetTempFileName()
        try {
            $sendArgs = @(
                '-i', $sshKey,
                '-o', 'StrictHostKeyChecking=accept-new',
                '-o', 'ConnectTimeout=10',
                '-o', 'BatchMode=yes',
                $hostTarget, $remoteCmd
            )
            $out = $fullMsg | & ssh @sendArgs 2>$errFile
            $stderrRaw = if (Test-Path $errFile) { (Get-Content $errFile -Raw -ErrorAction SilentlyContinue) } else { '' }
            if ($out -match '__APPENDED__') {
                # delivered
            } else {
                # Distinguish auth from network failure
                $isAuth = $false
                if ($stderrRaw -match '(?i)permission denied|publickey|host key verification|authentication fail|identification has changed|no supported authentication') {
                    $isAuth = $true
                }
                # Defensive trim: Substring(0, N) with N > string length throws
                # ArgumentOutOfRangeException. Use length of the *replaced* string
                # (newlines collapsed shrinks it) and clamp to 300.
                $stderrFlat = if ($stderrRaw) { ($stderrRaw -replace "`r?`n", ' ').Trim() } else { '' }
                $stderrLine = if ($stderrFlat.Length -gt 300) { $stderrFlat.Substring(0, 300) } else { $stderrFlat }
                if ($isAuth) {
                    $marker = ([ordered]@{ from='airc'; ts=(Get-Timestamp); msg="[AUTH FAILED to $peerName - repair required, NOT queued] $stderrLine" } | ConvertTo-Json -Compress)
                    Add-Content -Path $MESSAGES -Value $marker
                    Write-Error 'SSH auth to host FAILED. Message NOT queued - retries would fail identically.'
                    Write-Error "SSH stderr: $stderrLine"
                    Write-Error 'Fix: airc teardown --flush  then  airc connect <invite>'
                    exit 1
                }
                # Network: queue
                $pending = Join-Path $AIRC_WRITE_DIR 'pending.jsonl'
                Add-Content -Path $pending -Value $fullMsg
                $marker = ([ordered]@{ from='airc'; ts=(Get-Timestamp); msg="[QUEUED to $peerName - network error, will retry] $stderrLine" } | ConvertTo-Json -Compress)
                Add-Content -Path $MESSAGES -Value $marker
                Write-Warning "Network error reaching host - message queued. SSH stderr: $stderrLine"
            }
        } finally {
            Remove-Item $errFile -Force -ErrorAction SilentlyContinue
        }
    } else {
        Add-Content -Path $MESSAGES -Value $fullMsg
    }

    # Reset reminder
    $now = [int][double]::Parse(((Get-Date).ToUniversalTime() - [DateTime]'1970-01-01').TotalSeconds)
    Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'last_sent') -Value $now -NoNewline
    Remove-Item (Join-Path $AIRC_WRITE_DIR 'reminded') -Force -ErrorAction SilentlyContinue
}

# -- cmd_ping -----------------------------------------------------------
function Invoke-Ping {
    param([string[]]$Argv)
    if (-not $Argv -or $Argv.Count -eq 0) { Die 'Usage: airc ping @peer [timeout_secs]' }
    $first = $Argv[0]
    if (-not $first.StartsWith('@')) { Die 'Usage: airc ping @peer (broadcast ping not supported)' }
    $peerName = $first.Substring(1)
    $timeout  = if ($Argv.Count -gt 1 -and $Argv[1] -match '^\d+$') { [int]$Argv[1] } else { 10 }
    Ensure-Init

    $pingId = [guid]::NewGuid().ToString()
    $startTime = Get-Date
    Invoke-Send -Argv @("@$peerName", "[PING:$pingId]") | Out-Null
    Write-Host "ping sent to $peerName (id=$pingId) - waiting up to ${timeout}s for pong..."

    while ($true) {
        $elapsed = ((Get-Date) - $startTime).TotalSeconds
        if (Test-Path $MESSAGES) {
            $hit = Select-String -Path $MESSAGES -Pattern "\[PONG:$pingId\]" -SimpleMatch -Quiet -ErrorAction SilentlyContinue
            if ($hit) {
                Write-Host "PONG received from $peerName after $([int]$elapsed)s - monitor alive + auto-responder working."
                return
            }
        }
        if ($elapsed -ge $timeout) {
            Write-Host "TIMEOUT after ${timeout}s - no pong from $peerName."
            $sawPing = Select-String -Path $MESSAGES -Pattern "\[PING:$pingId\]" -SimpleMatch -Quiet -ErrorAction SilentlyContinue
            if ($sawPing) {
                Write-Host '  Ping IS visible in local log (cmd_send mirrored it). Outbound works.'
                Write-Host '  No pong likely means: (a) peer monitor dead, (b) older airc, or (c) non-airc agent.'
            } else {
                Write-Host '  Ping is NOT in local log - cmd_send mirror may have failed. Check: airc status, airc logs.'
            }
            exit 1
        }
        Start-Sleep -Milliseconds 500
    }
}

# -- cmd_send_file ------------------------------------------------------
function Invoke-SendFile {
    param([string[]]$Argv)
    if (-not $Argv -or $Argv.Count -lt 2) { Die 'Usage: airc send-file <peer> <path>' }
    $peerName = $Argv[0]
    $filepath = $Argv[1]
    if (-not (Test-Path $filepath)) { Die "File not found: $filepath" }
    Ensure-Init
    $hostTarget = Get-ConfigVal -Key 'host_target' -Default ''
    $myName = Get-Name
    $filename = Split-Path -Leaf $filepath
    $targetHost = if ($hostTarget) { $hostTarget } else { 'localhost' }
    $rhome = Get-RemoteHome
    Invoke-AircSsh $targetHost "mkdir -p $rhome/files/$myName" 2>$null
    $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
    $scpArgs = @(
        '-i', $sshKey,
        '-o', 'StrictHostKeyChecking=accept-new',
        '-q',
        $filepath,
        "${targetHost}:${rhome}/files/${myName}/${filename}"
    )
    & scp @scpArgs 2>&1
    if ($LASTEXITCODE -ne 0) { Die "scp failed for $filename" }
    $size = (Get-Item $filepath).Length
    Invoke-Send -Argv @("@$peerName", "Sent file: $filename ($size bytes)") | Out-Null
    Write-Host "Sent $filename ($size bytes)"
}

# -- cmd_update / cmd_canary --------------------------------------------
function Invoke-Update {
    param([string[]]$Argv)
    $dir = if ($env:AIRC_DIR) { $env:AIRC_DIR } else { Join-Path $env:USERPROFILE '.airc-src' }
    $channelFile = Join-Path $dir '.channel'
    $requested = ''
    for ($i = 0; $i -lt $Argv.Count; $i++) {
        switch ($Argv[$i]) {
            '--channel'  { $requested = $Argv[$i + 1]; $i++ }
            '-c'         { $requested = $Argv[$i + 1]; $i++ }
            '--canary'   { $requested = 'canary' }
            '--main'     { $requested = 'main' }
        }
    }
    if (-not (Test-Path (Join-Path $dir '.git'))) {
        Die "No git checkout at $dir. Reinstall via install.ps1."
    }
    $channel = if ($requested) { $requested }
               elseif (Test-Path $channelFile) { (Get-Content $channelFile -Raw).Trim() }
               else { 'main' }

    $before = (& git -C $dir rev-parse --short HEAD).Trim()
    $current = (& git -C $dir rev-parse --abbrev-ref HEAD).Trim()
    if ($current -ne $channel) {
        & git -C $dir fetch --quiet origin $channel
        if ($LASTEXITCODE -ne 0) { Die "Channel '$channel' not found on origin." }
        & git -C $dir checkout -q $channel 2>$null
        if ($LASTEXITCODE -ne 0) { & git -C $dir checkout -q -B $channel "origin/$channel" }
    }
    & git -C $dir pull --ff-only --quiet
    # Re-run install.ps1 to refresh skills + binary
    $installScript = Join-Path $dir 'install.ps1'
    if (Test-Path $installScript) {
        & pwsh -NoLogo -NoProfile -File $installScript
    }
    Set-Content -Path $channelFile -Value $channel -NoNewline
    $after = (& git -C $dir rev-parse --short HEAD).Trim()
    if ($before -eq $after) {
        Write-Host "  Already at $after on channel '$channel'. Skills refreshed."
    } else {
        Write-Host "  Updated: $before -> $after on channel '$channel'. Skills refreshed."
        Write-Host "  Running monitor still uses old code. To pick up:  airc teardown && airc connect"
    }
}

# -- cmd_daemon (Windows: Task Scheduler) -------------------------------
# launchd / systemd analog on Windows is the Task Scheduler. We register
# a per-user task that runs at logon, restarts on failure.
function Invoke-Daemon {
    param([string[]]$Argv)
    $action = if ($Argv -and $Argv.Count -gt 0) { $Argv[0] } else { 'status' }
    $taskName = 'AIRC'
    switch ($action) {
        'install' {
            $aircCmd = Join-Path (Join-Path $env:USERPROFILE 'AppData\Local\Programs\airc') 'airc.cmd'
            if (-not (Test-Path $aircCmd)) {
                Die "airc.cmd not found at $aircCmd. Run install.ps1 first."
            }
            $action = New-ScheduledTaskAction -Execute $aircCmd -Argument 'connect'
            $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
            $settings = New-ScheduledTaskSettingsSet -StartWhenAvailable `
                -DontStopOnIdleEnd -RestartCount 99 -RestartInterval (New-TimeSpan -Minutes 1) `
                -ExecutionTimeLimit ([TimeSpan]::Zero)
            $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive
            Register-ScheduledTask -TaskName $taskName -Action $action -Trigger $trigger `
                -Settings $settings -Principal $principal -Force | Out-Null
            Write-Host "  + Registered Task Scheduler task '$taskName' (runs at logon, restarts on failure)"
            Write-Host "  Status:  airc daemon status"
        }
        { $_ -in @('uninstall','remove','stop') } {
            try {
                Unregister-ScheduledTask -TaskName $taskName -Confirm:$false -ErrorAction Stop
                Write-Host "  + Removed Task Scheduler task '$taskName'"
            } catch {
                Write-Host "  (task '$taskName' not registered)"
            }
        }
        'status' {
            $task = Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue
            if (-not $task) { Write-Host "  No daemon installed. Run: airc daemon install"; return }
            $info = Get-ScheduledTaskInfo -TaskName $taskName
            Write-Host "  Task:        $taskName"
            Write-Host "  State:       $($task.State)"
            Write-Host "  Last run:    $($info.LastRunTime)"
            Write-Host "  Last result: $($info.LastTaskResult)"
            Write-Host "  Next run:    $($info.NextRunTime)"
        }
        default { Die "Usage: airc daemon [install|uninstall|status]" }
    }
}

# -- cmd_doctor's tests-runner cousin -----------------------------------
function Invoke-Tests {
    Write-Host '  Integration test runner not yet ported to Windows.'
    Write-Host '  Bash test/integration.sh is the canonical suite (run on macOS / Linux / WSL).'
    Write-Host '  Windows native suite: tracked as roadmap once cmd_connect host-mode is solid.'
}

# -- cmd_connect --------------------------------------------------------
# The big one. host vs joiner branching, gh discovery, mnemonic resolver,
# pair handshake (TCP), monitor launch.
function Invoke-Connect {
    param([string[]]$Argv)
    # Flag parsing
    $useGist = $true
    $roomName = 'general'
    $useRoom  = $true
    $resolvedRoomName = ''
    $resolvedGistId   = ''
    $positional = @()
    for ($i = 0; $i -lt $Argv.Count; $i++) {
        switch -Regex ($Argv[$i]) {
            '^(--gist|-gist)$' { $useGist = $true }
            '^(--no-gist|-no-gist)$' { $useGist = $false }
            '^(--room|-room)$' { $roomName = $Argv[$i + 1]; $useRoom = $true; $i++ }
            '^(--no-general|-no-general|--no-room|-no-room)$' { $useRoom = $false }
            default { $positional += $Argv[$i] }
        }
    }
    $target = if ($positional.Count -gt 0) { $positional[0] } else { '' }
    $reminderInterval = if ($env:AIRC_REMINDER) { [int]$env:AIRC_REMINDER }
                       elseif ($positional.Count -gt 1) { [int]$positional[1] }
                       else { 300 }

    # Auto-teardown stale processes in this scope before fresh start
    $stalePids = Read-AircPidFile
    if ($stalePids.Count -gt 0) {
        foreach ($p in $stalePids) {
            if (Get-Process -Id $p -ErrorAction SilentlyContinue) {
                Stop-ProcessTree -ProcId $p
            }
        }
        Remove-Item (Join-Path $AIRC_WRITE_DIR 'airc.pid') -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 500
    }

    # -- Resume case (no target, prior config) --
    if (-not $target -and (Test-Path $CONFIG)) {
        $priorHost = Get-ConfigVal -Key 'host_target' -Default ''
        if ($priorHost) {
            $priorName = Get-ConfigVal -Key 'host_name' -Default (Get-Name)
            Write-Host "  Resuming as joiner of '$priorName' ($priorHost)..."
            # Tailscale-down fail-loud: if the saved host is CGNAT and
            # Tailscale is not running locally, SSH would hang 5s on the
            # ConnectTimeout then the monitor retry loop would spin forever
            # with no actionable signal. Advise-TailscaleIfDown prints
            # platform-specific start instructions and returns $true when
            # the caller should abort. Mirrors bash 0f8d8a7.
            if (Advise-TailscaleIfDown -TargetHost $priorHost) {
                Die 'Re-run airc join after starting Tailscale.'
            }
            # Auth probe before committing to monitor loop
            $sshKey = Join-Path $IDENTITY_DIR 'ssh_key'
            $probeErr = [System.IO.Path]::GetTempFileName()
            $authProbeArgs = @(
                '-i', $sshKey,
                '-o', 'StrictHostKeyChecking=accept-new',
                '-o', 'ConnectTimeout=5',
                '-o', 'BatchMode=yes',
                $priorHost, 'echo __AUTH_OK__'
            )
            $probeOut = & ssh @authProbeArgs 2>$probeErr
            $stderrText = (Get-Content $probeErr -Raw -ErrorAction SilentlyContinue)
            Remove-Item $probeErr -Force -ErrorAction SilentlyContinue
            if ($probeOut -notmatch '__AUTH_OK__') {
                if ($stderrText -match '(?i)permission denied|publickey|host key verification|authentication fail|no supported authentication') {
                    Write-Error "SSH auth to host FAILED on resume. Saved pairing is stale."
                    Write-Error "Fix: airc teardown --flush  then  airc connect <invite>"
                    Die 'Resume aborted - re-pair required'
                } else {
                    Write-Warning "Host probe failed (non-auth). Monitor will retry in background."
                    Write-Warning "SSH stderr: $stderrText"
                }
            }
            Write-AircPidFile -Pids @($PID)
            # Same banner the fresh-pair / host paths emit. Without this,
            # the resume path drops straight into the monitor with no
            # console signal that anything is up -- looks indistinguishable
            # from a hung process to anyone watching stdout. Joel
            # 2026-04-24: parity gap noted across all implementations.
            Write-Host '  Monitoring for messages...'
            Start-AircMonitor -MyName (Get-Name)
            return
        }
    }

    # -- Zero-arg discovery: find #general (or named room) on our gh --
    $savedHostTarget = ''
    if (Test-Path $CONFIG) { $savedHostTarget = Get-ConfigVal -Key 'host_target' -Default '' }
    if (-not $target -and -not $savedHostTarget -and ($env:AIRC_NO_DISCOVERY -ne '1') -and (Test-GhAvailable)) {
        if ($useRoom) {
            $rows = Get-GhGistList -Limit 50
            $candidates = $rows | Where-Object { $_.Description -eq "airc room: $roomName" }
            if ($candidates -and $candidates.Count -ge 1) {
                $picked = $candidates[0].Id
                Write-Host "  Found #$roomName on your gh account -> joining ($picked)"
                $target = $picked
            } else {
                Write-Host "  No #$roomName found on your gh account -> becoming the host."
            }
        }
    }

    # -- Mnemonic resolver: humanhash phrase -> gist id (same gh) --
    if ($target -and $target -match '^[a-z]+(-[a-z]+){2,}$') {
        if (-not (Test-GhAvailable)) {
            Die "Mnemonic '$target' lookup needs gh CLI. Install gh + 'gh auth login', or use the gist id directly."
        }
        $matched = ''
        foreach ($r in (Get-GhGistList -Limit 50)) {
            if ($r.Description -notmatch '^airc (room:|invite for)') { continue }
            $hh = Get-Humanhash -HexInput $r.Id
            if ($hh -eq $target) { $matched = $r.Id; break }
        }
        if ($matched) {
            Write-Host "  Resolved mnemonic '$target' -> gist $matched"
            $target = $matched
        } else {
            Die "Mnemonic '$target' didn't match any airc gist on this gh account."
        }
    }

    # -- Gist transport: target without '@' is treated as gist id --
    if ($target -and ($target -notmatch '@')) {
        $gistId = $target -replace '^gist:', ''
        $resolvedGistId = $gistId
        if ($gistId -match '^[a-zA-Z0-9]{6,40}$') {
            Write-Host "  Resolving gist $gistId ..."
            $rawContent = Get-GistContent -GistId $gistId
            if (-not $rawContent) {
                Die "Failed to fetch gist '$gistId'. Check the ID, network, and (if private) 'gh auth login'."
            }
            $resolved = $null
            try {
                $env = $rawContent | ConvertFrom-Json
                if ($env.airc) {
                    switch ($env.kind) {
                        'invite' { $resolved = $env.invite }
                        'room'   { $resolved = $env.invite; $resolvedRoomName = $env.name }
                        default  { Die "Gist uses unknown kind '$($env.kind)' - this airc may need 'airc update'." }
                    }
                }
            } catch { }
            if (-not $resolved) {
                # Legacy raw-string format
                $resolved = ($rawContent -split "`n" | Where-Object { $_ -match '@.*@' } | Select-Object -First 1).Trim()
            }
            if (-not $resolved -or ($resolved -notmatch '@')) {
                Die "Failed to resolve gist '$gistId' to a valid invite."
            }
            Write-Host '  + Resolved invite from gist.'
            $target = $resolved
        }
    }

    if ($target -and ($target -match '@')) {
        # -- JOIN MODE --
        $hostSshPubkeyB64 = ''
        if ($target -match '#') {
            $hostSshPubkeyB64 = ($target -split '#')[-1]
            $target = ($target -split '#')[0]
        }
        # Parse name@user@host[:port]
        $peerName  = ($target -split '@')[0]
        $sshTarget = $target.Substring($peerName.Length + 1)
        $peerPort  = '7547'
        if ($sshTarget -match ':(\d+)$') {
            $peerPort = $matches[1]
            $sshTarget = $sshTarget -replace ':\d+$',''
        }
        if (-not $peerName -or -not $sshTarget) { Die 'Format: airc connect name@user@host' }

        $myName = Resolve-AircName
        Init-Identity -Name $myName

        # Write initial config
        Set-ConfigVal -Updates @{
            name        = $myName
            host        = (Get-AircHost)
            host_target = $sshTarget
            created     = (Get-Timestamp)
        }
        if ($resolvedRoomName) {
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'room_name') -Value $resolvedRoomName -NoNewline
            Write-Host "  Joined #$resolvedRoomName"
        }

        # Pre-authorize host's pubkey if in join string
        if ($hostSshPubkeyB64) {
            try {
                $hostSshPubkey = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($hostSshPubkeyB64))
                if ($hostSshPubkey) { Add-AuthorizedKey -PubKey $hostSshPubkey }
            } catch { }
        }

        # Pair handshake via TCP (.NET native, no embedded Python)
        $peerHostOnly = ($sshTarget -split '@')[-1]

        # Tailscale-down pre-flight on fresh-pair / gist-discovery paths.
        # Resume path (line ~1877) already calls Advise-TailscaleIfDown, but
        # that gate doesn't cover (a) cold-start `airc join <invite>` from a
        # fresh scope or (b) the gist-discovery resolution that lands here
        # with a tailnet host_target. Without this check, a logged-out
        # Tailscale produces a silent unreachable-host + self-heal cascade
        # (issue #78, Memento's case 2026-04-25). Same call shape as resume
        # path: detect-and-instruct, do not auto-tailscale-up.
        if (Advise-TailscaleIfDown -TargetHost $peerHostOnly) {
            Die 'Re-run airc join after starting Tailscale.'
        }

        Write-Host "  Connecting to ${peerHostOnly}:$peerPort ..."
        $mySshPub  = (Get-Content (Join-Path $IDENTITY_DIR 'ssh_key.pub') -Raw -ErrorAction SilentlyContinue).Trim()
        $mySignPub = (Get-Content (Join-Path $IDENTITY_DIR 'public.pem') -Raw -ErrorAction SilentlyContinue)
        $payload   = ([ordered]@{
            name      = $myName
            host      = "$($env:USERNAME)@$(Get-AircHost)"
            ssh_pub   = $mySshPub
            sign_pub  = $mySignPub
            airc_home = $AIRC_WRITE_DIR
        } | ConvertTo-Json -Compress)

        $response = $null
        try {
            $client = [System.Net.Sockets.TcpClient]::new()
            $iar = $client.BeginConnect($peerHostOnly, [int]$peerPort, $null, $null)
            $ok = $iar.AsyncWaitHandle.WaitOne(30000)
            if (-not $ok) { $client.Close(); throw "TCP connect to ${peerHostOnly}:$peerPort timed out" }
            $client.EndConnect($iar)
            $client.SendTimeout = 30000
            $client.ReceiveTimeout = 30000
            $stream = $client.GetStream()
            $bytes = [Text.Encoding]::UTF8.GetBytes($payload + "`n")
            $stream.Write($bytes, 0, $bytes.Length)
            try { $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send) } catch { }
            $sb = [System.Text.StringBuilder]::new()
            $buf = New-Object byte[] 4096
            while ($true) {
                $n = $stream.Read($buf, 0, $buf.Length)
                if ($n -le 0) { break }
                $sb.Append([Text.Encoding]::UTF8.GetString($buf, 0, $n)) | Out-Null
            }
            $client.Close()
            $response = $sb.ToString().Trim()
        } catch {
            $response = $null
            Write-Warning "Pair handshake failed: $_"
        }

        if (-not $response) {
            # Self-heal: if we resolved a kind:room gist, take over as new host
            if ($resolvedRoomName -and $resolvedGistId -and (Test-GhAvailable)) {
                Write-Host ''
                Write-Host "  ! Host of #$resolvedRoomName unreachable - self-healing as new host..."
                Write-Host "     (prior host gist: $resolvedGistId)"
                & gh gist delete $resolvedGistId --yes 2>$null
                $preservedName = Get-ConfigVal -Key 'name' -Default ''
                Remove-Item $CONFIG -Force -ErrorAction SilentlyContinue
                Remove-Item (Join-Path $AIRC_WRITE_DIR 'room_name') -Force -ErrorAction SilentlyContinue
                Write-Host "  Re-launching into host mode for #$resolvedRoomName ..."
                $env:AIRC_NO_DISCOVERY = '1'
                if ($preservedName) { $env:AIRC_NAME = $preservedName }
                Invoke-Connect -Argv @('--room', $resolvedRoomName)
                return
            }
            Die "Can't reach ${peerHostOnly}:$peerPort. Is the host running 'airc connect'?"
        }

        # Parse host's response
        try { $resp = $response | ConvertFrom-Json } catch { Die "Pair handshake: malformed host response: $response" }
        if ($resp.ssh_pub) { Add-AuthorizedKey -PubKey $resp.ssh_pub }

        # Save host as a peer
        if (-not (Test-Path $PEERS_DIR)) { New-Item -ItemType Directory -Force -Path $PEERS_DIR | Out-Null }
        # Drop stale records that share this host
        Get-ChildItem $PEERS_DIR -Filter '*.json' -ErrorAction SilentlyContinue | ForEach-Object {
            if ($_.BaseName -eq $peerName) { return }
            try {
                $d = Get-Content $_.FullName -Raw | ConvertFrom-Json
                if ($d.host -eq $sshTarget) {
                    Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
                    Remove-Item ((Join-Path $PEERS_DIR ($_.BaseName + '.pub'))) -Force -ErrorAction SilentlyContinue
                }
            } catch { }
        }
        $peerRecord = [ordered]@{
            name      = $peerName
            host      = $sshTarget
            airc_home = $resp.airc_home
            paired    = (Get-Timestamp)
        }
        $peerJsonPath = Join-Path $PEERS_DIR "$peerName.json"
        ($peerRecord | ConvertTo-Json -Depth 10) | Set-Content -Path $peerJsonPath -NoNewline
        if ($resp.sign_pub) {
            Set-Content -Path (Join-Path $PEERS_DIR "$peerName.pub") -Value $resp.sign_pub -NoNewline
        }

        # Persist host details
        Set-ConfigVal -Updates @{
            host_airc_home = $resp.airc_home
            host_name      = $peerName
            host_port      = $peerPort
            host_ssh_pub   = $resp.ssh_pub
        }

        # Reminder from host
        $hostReminder = if ($resp.reminder) { [int]$resp.reminder } else { 300 }
        if ($hostReminder -gt 0) {
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'reminder') -Value $hostReminder -NoNewline
            $now = [int][double]::Parse(((Get-Date).ToUniversalTime() - [DateTime]'1970-01-01').TotalSeconds)
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'last_sent') -Value $now -NoNewline
        }

        # Verify SSH works
        $verify = Invoke-AircSsh $sshTarget 'echo ok' 2>$null
        if ($verify -match 'ok') {
            Write-Host "  Connected to '$peerName' (SSH verified, reminder: ${hostReminder}s)"
        } else {
            Write-Host "  Connected to '$peerName' (SSH not verified - messages may need retry)"
        }
        Write-AircPidFile -Pids @($PID)
        Write-Host '  Monitoring for messages...'
        Start-AircMonitor -MyName $myName

    } else {
        # -- HOST MODE --
        $name = if ($target) { $target } else { Resolve-AircName }
        Init-Identity -Name $name
        Set-ConfigVal -Updates @{
            name    = $name
            host    = (Get-AircHost)
            created = (Get-Timestamp)
        }
        $hostA = Get-AircHost
        $user  = $env:USERNAME
        $sshPub = (Get-Content (Join-Path $IDENTITY_DIR 'ssh_key.pub') -Raw).Trim()
        $sshPubB64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($sshPub))

        $hostPort = if ($env:AIRC_PORT) { [int]$env:AIRC_PORT } else { 7547 }
        $originalPort = $hostPort
        $hostPort = Get-FreeAircPort -Start $hostPort
        $portSuffix = if ($hostPort -ne 7547) { ":$hostPort" } else { '' }
        Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'host_port') -Value $hostPort -NoNewline

        if ($reminderInterval -gt 0) {
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'reminder') -Value $reminderInterval -NoNewline
            $now = [int][double]::Parse(((Get-Date).ToUniversalTime() - [DateTime]'1970-01-01').TotalSeconds)
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'last_sent') -Value $now -NoNewline
        }

        Write-Host ''
        if ($hostPort -ne $originalPort) { Write-Host "  Port $originalPort was taken; using $hostPort." }
        Write-Host "  Hosting as '$name' (reminder: ${reminderInterval}s)"
        Write-Host ''
        $inviteLong = "$name@$user@$hostA$portSuffix#$sshPubB64"

        $printedLong = $false
        if (-not $useGist) {
            Write-Host '  On the other machine:'
            Write-Host "    airc connect $inviteLong"
            $printedLong = $true
        }
        if ($useRoom) {
            Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'room_name') -Value $roomName -NoNewline
            Write-Host "  Hosting #$roomName (gh-account substrate)."
        }

        # Gist transport
        if ($useGist) {
            if (-not (Test-GhAvailable)) {
                Write-Host ''
                Write-Host '  ! --gist requested but gh CLI not installed.'
                Write-Host '     winget install --id GitHub.cli  (then: gh auth login -s gist)'
                Write-Host '     Skipping gist push; long invite above is the only handoff.'
            } else {
                $now = Get-Timestamp
                if ($useRoom) {
                    $envelope = [ordered]@{
                        airc    = 1
                        kind    = 'room'
                        name    = $roomName
                        topic   = ''
                        invite  = $inviteLong
                        host    = [ordered]@{ name=$name; user=$user; address=$hostA; port=$hostPort }
                        created = $now
                        updated = $now
                    }
                    $gistDesc = "airc room: $roomName"
                } else {
                    $envelope = [ordered]@{
                        airc    = 1
                        kind    = 'invite'
                        invite  = $inviteLong
                        host    = [ordered]@{ name=$name; user=$user; address=$hostA; port=$hostPort }
                        created = $now
                    }
                    $gistDesc = "airc invite for $name (delete after pair)"
                }
                $gistTmp = [System.IO.Path]::GetTempFileName()
                ($envelope | ConvertTo-Json -Depth 10) | Set-Content -Path $gistTmp -NoNewline
                $gistOutput = & gh gist create -d $gistDesc $gistTmp 2>$null
                Remove-Item $gistTmp -Force -ErrorAction SilentlyContinue
                $gistUrl = if ($gistOutput) { ($gistOutput | Select-Object -Last 1).Trim() } else { '' }
                if ($gistUrl) {
                    $gistId = ($gistUrl -split '/')[-1]
                    $hh = Get-Humanhash -HexInput $gistId
                    if ($useRoom) {
                        Set-Content -Path (Join-Path $AIRC_WRITE_DIR 'room_gist_id') -Value $gistId -NoNewline
                        Write-Host "  Hosting #$roomName (gh-account substrate)."
                        Write-Host "  Other agents on your gh account auto-join via:  airc connect"
                        Write-Host "  Cross-account share:"
                        Write-Host "    airc connect $gistId"
                        if ($hh) { Write-Host "      # mnemonic: $hh" }
                        Write-Host "    airc connect $inviteLong"
                        Write-Host ''
                        Write-Host "  (Room gist: $gistUrl - persistent; deleted on 'airc part'.)"
                    } else {
                        Write-Host '  On the other machine (pick whichever is easiest to share):'
                        Write-Host ''
                        Write-Host "    airc connect $gistId"
                        if ($hh) { Write-Host "      # mnemonic: $hh" }
                        Write-Host "    airc connect $inviteLong"
                        Write-Host ''
                        Write-Host "  (Gist: $gistUrl - secret, single-use; delete after pairing.)"
                    }
                } else {
                    Write-Host ''
                    Write-Host "  ! Gist push failed (gh auth?). Falling back to long invite:"
                    if (-not $printedLong) { Write-Host "    airc connect $inviteLong" }
                }
            }
        }

        Write-Host ''
        Write-Host "  Waiting for peers on port $hostPort..."

        # Background TCP-accept loop. We use Start-ThreadJob so it shares
        # the process (one PID for `airc connect` + the pair acceptor).
        # The job receives via a thread-safe queue from the listener.
        $hostState = @{
            Port         = $hostPort
            Name         = $name
            PeersDir     = $PEERS_DIR
            IdentityDir  = $IDENTITY_DIR
            Messages     = $MESSAGES
            ScopeDir     = $AIRC_WRITE_DIR
            Reminder     = $reminderInterval
            ConfigPath   = $CONFIG
        }
        $acceptorJob = Start-ThreadJob -ScriptBlock {
            param($s)
            $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Any, $s.Port)
            $listener.Start()
            try {
                while ($true) {
                    if (-not $listener.Pending()) { Start-Sleep -Milliseconds 250; continue }
                    $client = $listener.AcceptTcpClient()
                    try {
                        $client.ReceiveTimeout = 30000
                        $stream = $client.GetStream()
                        $sb = [System.Text.StringBuilder]::new()
                        $buf = New-Object byte[] 4096
                        $foundNewline = $false
                        while (-not $foundNewline) {
                            $n = $stream.Read($buf, 0, $buf.Length)
                            if ($n -le 0) { break }
                            $chunk = [Text.Encoding]::UTF8.GetString($buf, 0, $n)
                            $sb.Append($chunk) | Out-Null
                            if ($chunk.Contains("`n")) { $foundNewline = $true }
                        }
                        $joiner = $sb.ToString().Trim() | ConvertFrom-Json

                        # Authorize joiner SSH key
                        $sshDir = Join-Path $env:USERPROFILE '.ssh'
                        if (-not (Test-Path $sshDir)) { New-Item -ItemType Directory -Force -Path $sshDir | Out-Null }
                        $authKeys = Join-Path $sshDir 'authorized_keys'
                        if ($joiner.ssh_pub) {
                            $existing = if (Test-Path $authKeys) { Get-Content $authKeys -Raw -ErrorAction SilentlyContinue } else { '' }
                            $line = $joiner.ssh_pub.Trim()
                            if ($existing -notlike "*$line*") { Add-Content -Path $authKeys -Value $line }
                        }

                        # Save joiner as peer (drop stale records sharing host)
                        if (-not (Test-Path $s.PeersDir)) { New-Item -ItemType Directory -Force -Path $s.PeersDir | Out-Null }
                        $jname = $joiner.name
                        $jhost = $joiner.host
                        Get-ChildItem $s.PeersDir -Filter '*.json' -ErrorAction SilentlyContinue | ForEach-Object {
                            if ($_.BaseName -eq $jname) { return }
                            try {
                                $d = Get-Content $_.FullName -Raw | ConvertFrom-Json
                                if ($d.host -eq $jhost) {
                                    Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
                                    Remove-Item (Join-Path $s.PeersDir ($_.BaseName + '.pub')) -Force -ErrorAction SilentlyContinue
                                }
                            } catch { }
                        }
                        $rec = [ordered]@{
                            name      = $jname
                            host      = $jhost
                            airc_home = $joiner.airc_home
                            paired    = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
                        }
                        ($rec | ConvertTo-Json -Depth 10) | Set-Content -Path (Join-Path $s.PeersDir "$jname.json") -NoNewline
                        if ($joiner.sign_pub) {
                            Set-Content -Path (Join-Path $s.PeersDir "$jname.pub") -Value $joiner.sign_pub -NoNewline
                        }

                        # Send back host info
                        $hostPub = (Get-Content (Join-Path $s.IdentityDir 'ssh_key.pub') -Raw).Trim()
                        $signPub = (Get-Content (Join-Path $s.IdentityDir 'public.pem') -Raw)
                        $resp = ([ordered]@{
                            ssh_pub   = $hostPub
                            sign_pub  = $signPub
                            name      = $s.Name
                            reminder  = $s.Reminder
                            airc_home = $s.ScopeDir
                        } | ConvertTo-Json -Compress)
                        $rb = [Text.Encoding]::UTF8.GetBytes($resp + "`n")
                        $stream.Write($rb, 0, $rb.Length)
                        $stream.Flush()

                        # Surface join as system event in messages.jsonl
                        try {
                            $roomNameFile = Join-Path $s.ScopeDir 'room_name'
                            $rname = if (Test-Path $roomNameFile) { (Get-Content $roomNameFile -Raw).Trim() } else { 'general' }
                            $event = [ordered]@{
                                ts   = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
                                from = 'airc'
                                to   = 'all'
                                msg  = "$jname joined #$rname"
                            }
                            Add-Content -Path $s.Messages -Value (($event | ConvertTo-Json -Compress))
                        } catch { }

                        Write-Host "  Peer joined: $jname"
                    } catch {
                        Write-Warning "Pair acceptor: $_"
                    } finally {
                        try { $client.Close() } catch { }
                    }
                }
            } finally {
                $listener.Stop()
            }
        } -ArgumentList $hostState

        Write-AircPidFile -Pids @($PID)
        Write-Host '  Monitoring for messages...'
        try {
            Start-AircMonitor -MyName $name
        } finally {
            Stop-Job $acceptorJob -ErrorAction SilentlyContinue | Out-Null
            Remove-Job $acceptorJob -Force -ErrorAction SilentlyContinue | Out-Null
        }
    }
}

# ========================================================================
# DISPATCH
# ========================================================================
$cmd = if ($args.Count -gt 0) { $args[0] } else { 'help' }
$rest = if ($args.Count -gt 1) { $args[1..($args.Count - 1)] } else { @() }

try {
    switch ($cmd) {
        # Info
        { $_ -in @('version','--version','-v') }      { Invoke-Version; break }
        { $_ -in @('help','--help','-h') }            { Invoke-Help; break }
        'doctor'                                       { Invoke-Doctor; break }
        { $_ -in @('tests','test') }                   { Invoke-Tests; break }

        # Connection lifecycle
        { $_ -in @('connect','setup','start','join','resume') } { Invoke-Connect -Argv $rest; break }

        # Messaging
        { $_ -in @('send','msg','say','privmsg') }    { Invoke-Send -Argv $rest; break }
        'send-file'                                    { Invoke-SendFile -Argv $rest; break }
        'ping'                                         { Invoke-Ping -Argv $rest; break }

        # Identity / peers
        { $_ -in @('rename','nick') }                 { Invoke-Rename -Argv $rest; break }
        'reminder'                                     { Invoke-Reminder -Argv $rest; break }
        'peers'                                        { Invoke-Peers; break }

        # Rooms / discovery
        { $_ -in @('rooms','list','ls') }             { Invoke-Rooms; break }
        { $_ -in @('invite','share','join-string') }  { Invoke-Invite; break }
        'part'                                         { Invoke-Part; break }

        # Lifecycle / disconnect
        { $_ -in @('teardown','stop','flush') }       { Invoke-Teardown -Argv $rest; break }
        { $_ -in @('disconnect','quit','leave','unbind') } { Invoke-Disconnect; break }

        # Diagnostic
        'logs'                                         { Invoke-Logs -Argv $rest; break }
        'status'                                       { Invoke-Status -Argv $rest; break }

        # Updates / channels
        { $_ -in @('update','upgrade','pull') }       { Invoke-Update -Argv $rest; break }
        'channel'                                      { Invoke-Channel -Argv $rest; break }
        'canary'                                       { Invoke-Update -Argv (@('--channel','canary') + $rest); break }

        # Daemon (Task Scheduler on Windows)
        { $_ -in @('daemon','autostart','service') }  { Invoke-Daemon -Argv $rest; break }

        # Monitor (rare standalone use)
        'monitor'                                      { Start-AircMonitor -MyName (Get-Name); break }

        # Debug
        'debug-scope' { Write-Host $AIRC_WRITE_DIR; break }
        'debug-name'  { Write-Host (Resolve-AircName); break }
        'debug-host'  { Write-Host (Get-AircHost); break }

        default { Die "Unknown command: $cmd. Try: airc help" }
    }
} catch {
    # Surface the real error -- `Write-Error $_` confuses the parser with
    # ambiguous parameter binding when $_ is an ErrorRecord (its
    # properties collide with -OutVariable / -OutBuffer). Use the host's
    # error stream directly with the rendered message + script location.
    $errMsg = "{0}`n  at {1}:{2}" -f $_.Exception.Message,
        $_.InvocationInfo.ScriptName, $_.InvocationInfo.ScriptLineNumber
    [Console]::Error.WriteLine($errMsg)
    exit 1
}
