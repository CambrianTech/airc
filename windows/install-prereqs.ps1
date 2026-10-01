# Windows package, volume and environment adapter for install.sh.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$SourceDirectory,
    [Parameter(Mandatory=$true)][string]$EnvironmentFile
)
$ErrorActionPreference = 'Stop'

# Paths. AIRC_DIR controls where the source lives; BIN_TARGET is where
# airc.exe lands (added to user PATH); SKILLS_TARGET is where
# Claude Code looks for slash-command skills. All three honor env-var
# overrides for tests + isolated installs (parity with install.sh).
$DEFAULT_AIRC_ROOT = Join-Path $env:USERPROFILE '.airc'
$DEFAULT_CLONE_DIR = Join-Path $DEFAULT_AIRC_ROOT 'src'
if ($SourceDirectory -and (Test-Path (Join-Path $SourceDirectory 'Cargo.toml')) -and
    (Test-Path (Join-Path $SourceDirectory 'crates\airc-cli'))) {
    $DEFAULT_CLONE_DIR = $SourceDirectory
}
$CLONE_DIR     = if ($env:AIRC_DIR)      { $env:AIRC_DIR }      else { $DEFAULT_CLONE_DIR }
$BIN_TARGET    = if ($env:BIN_TARGET)    { $env:BIN_TARGET }    else { Join-Path $env:USERPROFILE 'AppData\Local\Programs\airc' }
$SKILLS_TARGET = if ($env:SKILLS_TARGET) { $env:SKILLS_TARGET } else { Join-Path $env:USERPROFILE '.claude\skills' }
$REPO_URL      = 'https://github.com/CambrianTech/airc.git'

# Channel = the git branch of the install checkout (see "Clone or update"
# below). git is the state manager; no separate channel file.

function Write-Step($msg)  { Write-Host "  -> $msg" }
function Write-Ok($msg)    { Write-Host "  + $msg" -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "  ! $msg" -ForegroundColor Yellow }
function Write-Fail($msg)  { Write-Host "  x $msg" -ForegroundColor Red }

# -- Refresh PATH from registry ------------------------------------------
# winget updates the User PATH in the registry but the current session
# inherits the old PATH from when this script started. Without a refresh,
# any tool we just installed won't be found by Get-Command in the same
# session. Pulling Machine + User PATH and re-merging mirrors what a
# brand-new shell would inherit.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable('PATH', 'Machine')
    $user    = [Environment]::GetEnvironmentVariable('PATH', 'User')
    $env:PATH = "$user;$machine;$env:PATH"
}

# Match install.sh: a checkout is a valid install source and Cargo owns the
# target-directory decision. Do not require a junction or a second checkout.
function Get-AircTargetDirectory {
    param([string]$SourceDirectory)
    Push-Location -LiteralPath $SourceDirectory
    try {
        $metadata = & cargo metadata --format-version 1 --no-deps
        if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve Cargo target directory.' }
    } finally { Pop-Location }
    $target = ($metadata | ConvertFrom-Json).target_directory
    if (-not $target) { throw 'Cargo metadata did not report a target directory.' }
    return $target
}

function Test-MsvcToolchain {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { return $false }
    $vsPath = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    return [bool]$vsPath
}

function Get-AircDriveInfo([string]$Root) { return New-Object IO.DriveInfo($Root) }
function Get-AircFixedVolumes { return [IO.DriveInfo]::GetDrives() }

# Source builds must check storage BEFORE installing a toolchain. Prefer the
# existing choices; on a small system disk, use a roomy fixed secondary volume.
# This is install state, not machine-specific advice or a hardcoded drive letter.
function Initialize-AircBuildStorage {
    $systemDrive = Get-AircDriveInfo ([IO.Path]::GetPathRoot($env:SystemRoot))
    Write-Step ('System volume {0}: {1:N1} GB available' -f $systemDrive.Name, ($systemDrive.AvailableFreeSpace / 1GB))
    # VS retains installer/system components here even with relocated packages.
    # This is a conservative preflight floor, not an exact VS package estimate;
    # the vendor installer remains authoritative and its exit is reported.
    $systemReserve = if (Test-MsvcToolchain) { 256MB } else { 2GB }
    if ($systemDrive.AvailableFreeSpace -lt $systemReserve) {
        throw "The Windows system volume has insufficient space for remaining setup work ($([math]::Round($systemReserve / 1MB)) MB reserve). Setup stopped before installing packages."
    }
    $stateFile = Join-Path $DEFAULT_AIRC_ROOT 'build-storage.json'
    $storageRoot = $null
    if (Test-Path -LiteralPath $stateFile) {
        $storageRoot = (Get-Content -LiteralPath $stateFile -Raw | ConvertFrom-Json).root
        if (-not $storageRoot -or -not (Test-Path -LiteralPath ([IO.Path]::GetPathRoot($storageRoot)))) {
            throw 'The configured AIRC build-storage drive is unavailable. Reconnect it and rerun setup.'
        }
    } else {
        $systemRoot = [IO.Path]::GetPathRoot($env:SystemRoot)
        $systemDrive = Get-AircDriveInfo $systemRoot
        if ($systemDrive.AvailableFreeSpace -lt 20GB) {
            $candidate = Get-AircFixedVolumes | Where-Object {
                $_.IsReady -and $_.DriveType -eq 'Fixed' -and
                $_.RootDirectory.FullName -ne $systemRoot -and $_.AvailableFreeSpace -ge 20GB
            } | Sort-Object AvailableFreeSpace -Descending | Select-Object -First 1
            if (-not $candidate) { throw 'Source installation needs build space (20 GB recommended); no suitable fixed drive is available. Free space and rerun this setup.' }
            $storageRoot = Join-Path $candidate.RootDirectory.FullName ('airc-build\' + $env:USERNAME)
            New-Item -ItemType Directory -Force -Path $DEFAULT_AIRC_ROOT | Out-Null
            @{ root = $storageRoot } | ConvertTo-Json | Set-Content -LiteralPath $stateFile -Encoding UTF8
        }
    }
    if (-not $storageRoot) { return }
    $buildDrive = Get-AircDriveInfo ([IO.Path]::GetPathRoot($storageRoot))
    # Completed source builds need incremental headroom, not the full initial
    # download/toolchain reserve again. Check the selected cache, not merely an
    # installed binary that may have come from another machine or checkout.
    $target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $storageRoot 'target' }
    $buildReserve = if ((Test-MsvcToolchain) -and (Test-Path (Join-Path $target 'release\airc.exe'))) { 2GB } else { 20GB }
    if ($buildDrive.AvailableFreeSpace -lt $buildReserve) { throw "Build-storage volume $($buildDrive.Name) lacks the $($buildReserve / 1GB) GB reserve for remaining source-build work." }
    New-Item -ItemType Directory -Force -Path $storageRoot | Out-Null
    Write-Step "Build storage: $storageRoot (automatically selected; existing toolchains are preserved)"
    foreach ($setting in @(@('RUSTUP_HOME','rustup','.rustup'), @('CARGO_HOME','cargo','.cargo'))) {
        $existing = [Environment]::GetEnvironmentVariable($setting[0])
        if (-not $existing) { $existing = [Environment]::GetEnvironmentVariable($setting[0], 'User') }
        if ($existing) { Set-Item ('Env:' + $setting[0]) $existing; continue }
        if (Test-Path (Join-Path $env:USERPROFILE $setting[2])) { continue }
        $location = Join-Path $storageRoot $setting[1]
        [Environment]::SetEnvironmentVariable($setting[0], $location, 'User')
        Set-Item ('Env:' + $setting[0]) $location
    }
    # Cargo configuration may already select a shared cache. Query it once
    # Cargo exists; only relocate the unconfigured, checkout-local default.
    $script:AircBuildStorage = $storageRoot
    $scratch = Join-Path $storageRoot 'temp'
    New-Item -ItemType Directory -Force -Path $scratch | Out-Null
    $env:TEMP = $scratch
    $env:TMP = $scratch
}

# -- winget bootstrap ----------------------------------------------------
# winget ships with Windows 10 (1809+) and Windows 11 by default via the
# App Installer package. If a user is on a much older Windows OR has
# stripped App Installer, we can't auto-install -- flag it loud with the
# exact Microsoft Store / GitHub Releases URL to recover.
function Test-WingetAvailable {
    if (Get-Command winget -ErrorAction SilentlyContinue) { return }

    # Issue #95: detect Windows Server — Microsoft Store path is a
    # dead-end there (no Store, no App Installer). Surface chocolatey
    # / scoop fallbacks instead.
    $isServer = $false
    try {
        $os = Get-CimInstance Win32_OperatingSystem -ErrorAction SilentlyContinue
        # ProductType: 1=Workstation, 2=Domain Controller, 3=Server
        if ($os -and ($os.ProductType -eq 2 -or $os.ProductType -eq 3)) {
            $isServer = $true
        }
    } catch { }

    Write-Fail 'winget not found.'
    Write-Host ''
    if ($isServer) {
        Write-Host '  This is Windows Server. The Microsoft Store path does not apply here.'
        Write-Host '  Use chocolatey OR scoop, then re-run this installer:'
        Write-Host ''
        Write-Host '    # chocolatey (recommended for Server):'
        Write-Host '    Set-ExecutionPolicy Bypass -Scope Process -Force'
        Write-Host "    iex ((New-Object System.Net.WebClient).DownloadString('https://chocolatey.org/install.ps1'))"
        Write-Host '    choco install -y git gh rust'
        Write-Host ''
        Write-Host '    # OR scoop (user-scope, no admin needed):'
        Write-Host "    iwr -useb https://get.scoop.sh | iex"
        Write-Host '    scoop install git gh rust'
        Write-Host ''
        Write-Host '  After installing git, gh, rust manually, re-run this script;'
        Write-Host '  it will detect them and skip winget.'
    } else {
        Write-Host '  winget ships with App Installer (Microsoft Store). Install or update it:'
        Write-Host '    1. Open the Microsoft Store, search "App Installer", click Install/Update'
        Write-Host '       (or: https://www.microsoft.com/store/productId/9NBLGGH4NNS1)'
        Write-Host '    2. Reopen PowerShell and run this installer again.'
        Write-Host ''
        Write-Host '  If the Store is unavailable, install manually from'
        Write-Host '  https://github.com/microsoft/winget-cli/releases (latest .msixbundle).'
    }
    exit 1
}

# -- Install one winget package, idempotent ------------------------------
# Test-Cmd: a callable that returns $true when the package is already
# usable (e.g. {Get-Command cargo} or a custom probe). Skips winget on
# hits -- saves the 30s+ download/install round trip.
function Install-IfMissing {
    param(
        [string]$Name,        # human label for messages
        [string]$WingetId,    # winget package id (e.g. Rustlang.Rustup)
        [scriptblock]$TestCmd # returns truthy when already installed
    )
    if (& $TestCmd) {
        Write-Ok "$Name already installed"
        return
    }
    Write-Step "Installing $Name (winget: $WingetId) ..."
    Test-WingetAvailable
    # --silent: no UI prompts. --accept-*: prevents one-time first-run
    # interactive accepts that would block CI / first-time install.
    # --disable-interactivity: belt-and-suspenders against any winget
    # prompt that would hang a non-interactive bootstrap.
    $wingetArgs = @(
        'install', '--id', $WingetId, '--source', 'winget',
        '--exact',
        '--silent',
        '--accept-package-agreements',
        '--accept-source-agreements',
        '--disable-interactivity'
    )
    & winget @wingetArgs
    if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
        # -1978335189 (0x8A15002B) = APPINSTALLER_CLI_ERROR_UPDATE_NOT_APPLICABLE
        # = "already installed, no update needed". Treat as success.
        Write-Warn2 "winget exit $LASTEXITCODE for $Name -- continuing; the post-install probe below decides if we recover."
    }
    Update-SessionPath
    if (& $TestCmd) {
        Write-Ok "$Name installed"
    } else {
        throw "$Name is unavailable after installation. Setup stopped; rerun the same setup to resume."
    }
}

# gh offers an official portable distribution. Use it per-user so GitHub
# approval never depends on an otherwise unnecessary machine-wide MSI/UAC.
function Install-GitHubCli {
    param([Parameter(Mandatory=$true)][string]$SourceDirectory)
    $existing = Get-Command gh -ErrorAction SilentlyContinue
    if ($existing) {
        & $existing.Source --version | Out-Null
        if ($LASTEXITCODE -eq 0) { Write-Ok 'GitHub CLI already installed'; return }
    }
    Write-Step 'Installing official GitHub CLI for the current user'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $release = Invoke-RestMethod 'https://api.github.com/repos/cli/cli/releases/latest'
    $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'amd64' }
    $asset = $release.assets | Where-Object { $_.name -match ("_windows_" + $arch + '\.zip$') } | Select-Object -First 1
    $checksumAsset = $release.assets | Where-Object { $_.name -match '_checksums\.txt$' } | Select-Object -First 1
    if (-not $asset -or -not $checksumAsset) { throw 'Official GitHub CLI release is missing the Windows archive/checksum.' }
    $downloadDir = Join-Path $env:TEMP ('airc-gh-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $downloadDir | Out-Null
    $archive = Join-Path $downloadDir $asset.name
    $checksums = Join-Path $downloadDir 'checksums.txt'
    Invoke-WebRequest $asset.browser_download_url -UseBasicParsing -OutFile $archive
    Invoke-WebRequest $checksumAsset.browser_download_url -UseBasicParsing -OutFile $checksums
    $checksumLine = Get-Content -LiteralPath $checksums | Where-Object { $_ -match ('\s+\*?' + [regex]::Escape($asset.name) + '$') } | Select-Object -First 1
    if (-not $checksumLine) { throw 'GitHub CLI archive has no published checksum.' }
    $expected = ($checksumLine -split '\s+')[0]
    if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'GitHub CLI archive checksum mismatch.' }
    $destination = Join-Path $env:LOCALAPPDATA 'Programs\GitHub CLI'
    Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force
    & (Join-Path $SourceDirectory 'windows\register-bin-path.ps1') -BinDirectory (Join-Path $destination 'bin')
    Update-SessionPath
    & (Join-Path $destination 'bin\gh.exe') --version
    if ($LASTEXITCODE -ne 0) { throw 'GitHub CLI installation did not produce a working command.' }
    Remove-Item -LiteralPath $archive,$checksums -Force
}

# -- Banner --------------------------------------------------------------
Write-Host ''
Write-Host '  AIRC installer (Windows native)'
Write-Host '  --------------------------------'
Write-Host ''

Update-SessionPath
foreach ($setting in @('RUSTUP_HOME','CARGO_HOME','CARGO_TARGET_DIR')) {
    if (-not [Environment]::GetEnvironmentVariable($setting)) {
        $persisted = [Environment]::GetEnvironmentVariable($setting,'User')
        if ($persisted) { [Environment]::SetEnvironmentVariable($setting,$persisted,'Process') }
    }
}
Initialize-AircBuildStorage

# -- Install prereqs -----------------------------------------------------
# Order matters lightly: git first so we can clone, then gh for the
# substrate (gist transport), then the Rust toolchain to build airc.
# pwsh (PowerShell 7+) is intentionally NOT installed -- airc.ps1 is a
# thin bash shim that runs fine on the built-in PS 5.1, and dropping it
# removes a 30+ second prereq install (and the visible UAC prompt).

Install-IfMissing -Name 'Git for Windows'    -WingetId 'Git.Git'             -TestCmd { Get-Command git -ErrorAction SilentlyContinue }
Install-GitHubCli -SourceDirectory $SourceDirectory
# git + gh are the only non-Rust prereqs (plus the Rust toolchain below).
# Identity, signing, hooks, config, and JSON handling are all Rust-owned.

# -- Rust toolchain ------------------------------------------------------
# airc IS a Rust binary; cargo is a hard prereq. install.sh auto-installs
# Rustlang.Rustup on the winget path -- mirror that here instead of the
# old hard-exit "go install Rust yourself". winget's Rustup package runs
# rustup-init, which installs the stable-msvc toolchain and adds
# %USERPROFILE%\.cargo\bin to the User PATH; Update-SessionPath inside
# Install-IfMissing makes it visible to THIS session.
Install-IfMissing -Name 'Rust (rustup)'      -WingetId 'Rustlang.Rustup'     -TestCmd { Get-Command cargo -ErrorAction SilentlyContinue }
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    # rustup installed but cargo not resolving: a fresh rustup-init may
    # not have set a default toolchain (non-interactive install). Fix it
    # directly rather than telling the user to open a new shell and guess.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $rustupExe = Join-Path $cargoHome 'bin\rustup.exe'
    if (Test-Path $rustupExe) {
        & $rustupExe default stable
        Update-SessionPath
    }
}

# -- MSVC C++ build tools ------------------------------------------------
# Validated live on a fresh Windows 11 box (2026-06-10): rustup's default
# x86_64-pc-windows-msvc target CANNOT LINK without the Visual Studio C++
# build tools -- `cargo build` dies with a wall of per-crate
# `error: linking with link.exe failed: exit code: 1` and no guidance.
# The windows-gnu toolchain is NOT a viable fallback for airc: windows-sys
# raw-dylib import libs trip the upstream bundled-dlltool bug
# (rust-lang/rust#103939) and `ring` needs a real C compiler regardless.
# So: probe for the VC.Tools component via vswhere and auto-install the
# (license-free) VS 2022 Build Tools with the C++ workload when absent.
if (Test-MsvcToolchain) {
    Write-Ok 'MSVC C++ build tools already installed'
} else {
    Test-WingetAvailable
    Write-Step 'Installing Visual Studio 2022 Build Tools + C++ workload (required to link Rust on Windows; ~2 GB, several minutes) ...'
    $btArgs = @(
        'install', '--id', 'Microsoft.VisualStudio.2022.BuildTools', '--source', 'winget',
        '--exact', '--silent',
        '--accept-package-agreements', '--accept-source-agreements',
        '--disable-interactivity',
        '--override', '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    )
    if ($script:AircBuildStorage) {
        $vsRoot = Join-Path $script:AircBuildStorage 'visual-studio'
        $btArgs[-1] += ' --path install="' + (Join-Path $vsRoot 'BuildTools') + '"'
        $btArgs[-1] += ' --path cache="' + (Join-Path $vsRoot 'Cache') + '"'
        $btArgs[-1] += ' --path shared="' + (Join-Path $vsRoot 'Shared') + '"'
    }
    Write-Host '  Windows may request administrator consent for the C++ tools. Setup waits for that operation to finish.'
    & winget @btArgs
    $buildToolsExit = $LASTEXITCODE
    if (Test-MsvcToolchain) {
        Write-Ok 'MSVC C++ build tools installed'
    } else {
        Write-Fail "MSVC C++ tools are unavailable (installer exit $buildToolsExit)."
        if ($buildToolsExit -eq -2147024784 -or $buildToolsExit -eq 2147942512) {
            Write-Host '  Windows reported insufficient disk space (0x80070070). Setup did not reach the build.'
        }
        Write-Host '  Re-run this setup after completing Windows consent or resolving the reported installer failure.'
        exit 1
    }
}


Write-Host ''


if ($script:AircBuildStorage -and -not $env:CARGO_TARGET_DIR) {
    $target = Get-AircTargetDirectory -SourceDirectory $SourceDirectory
    if ([IO.Path]::GetFullPath($target) -eq [IO.Path]::GetFullPath((Join-Path $SourceDirectory 'target'))) {
        $env:CARGO_TARGET_DIR = Join-Path $script:AircBuildStorage 'target'
    }
}
Update-SessionPath
$values = New-Object System.Collections.Generic.List[string]
foreach ($name in @('PATH','CARGO_HOME','RUSTUP_HOME','CARGO_TARGET_DIR','TEMP','TMP')) {
    $value = [Environment]::GetEnvironmentVariable($name)
    if ($value) { $values.Add($name); $values.Add($value) }
}
[IO.File]::WriteAllText($EnvironmentFile, (($values -join [char]0) + [char]0), (New-Object Text.UTF8Encoding($false)))
