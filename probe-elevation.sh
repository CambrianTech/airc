#!/usr/bin/env bash
# probe-elevation.sh — diagnostic for "where does elevated PS write?" on Windows Git Bash.
# Writes a probe.ps1 to MSYS /tmp (= Windows %LOCALAPPDATA%\Temp typically),
# elevates via Start-Process -Verb RunAs, has the elevated PS dump where IT
# thinks temp is + which user it ran as.

set -u

# Stage probe.ps1 in the bash /tmp (which is Windows %TEMP% on Git Bash).
PROBE_DIR="${TMP:-/tmp}"
cat > "$PROBE_DIR/airc-elev-probe.ps1" <<'PSEOF'
$lines = @(
  "----- airc elevation probe -----"
  "ran-as-user      : $env:USERNAME"
  "ran-as-domain    : $env:USERDOMAIN"
  "is-admin         : $((New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"
  "[IO.Path]::GetTempPath() : $([System.IO.Path]::GetTempPath())"
  "env:TEMP         : $env:TEMP"
  "env:TMP          : $env:TMP"
  "env:LOCALAPPDATA : $env:LOCALAPPDATA"
  "env:USERPROFILE  : $env:USERPROFILE"
  "where-am-i       : $PSScriptRoot"
  "---"
)
$out = $lines -join [Environment]::NewLine
# Write to multiple candidate locations so we can find SOMETHING.
foreach ($d in @([System.IO.Path]::GetTempPath(), $env:TEMP, $env:USERPROFILE, "C:\Windows\Temp")) {
  if (-not $d) { continue }
  if (-not (Test-Path $d)) { continue }
  try {
    $out | Out-File -FilePath (Join-Path $d "airc-elev-probe-OUT.txt") -Force -Encoding utf8
  } catch {}
}
PSEOF

# Convert /tmp/airc-elev-probe.ps1 to Windows path for Start-Process -File.
PROBE_WIN=""
if command -v cygpath >/dev/null 2>&1; then
  PROBE_WIN=$(cygpath -w "$PROBE_DIR/airc-elev-probe.ps1")
else
  PROBE_WIN=$(printf '%s' "$PROBE_DIR/airc-elev-probe.ps1" | sed 's|^/\([a-z]\)/|\U\1:\\\\|; s|/|\\\\|g')
fi

echo "=== probe.ps1 staged at: $PROBE_DIR/airc-elev-probe.ps1"
echo "===     (Windows form: $PROBE_WIN)"
echo "=== launching elevated PS — click YES on UAC prompt ==="
powershell.exe -NoProfile -Command "Start-Process powershell -Verb RunAs -Wait -ArgumentList @('-NoProfile','-File','$PROBE_WIN')" 2>&1
echo "=== elevated PS exited; searching for output... ==="

for cand in \
  "/c/Users/$USERNAME/AppData/Local/Temp/airc-elev-probe-OUT.txt" \
  "/c/Users/$USERNAME/airc-elev-probe-OUT.txt" \
  "/c/Windows/Temp/airc-elev-probe-OUT.txt" \
  "/tmp/airc-elev-probe-OUT.txt"; do
  if [ -f "$cand" ]; then
    echo ""
    echo "=== FOUND: $cand ==="
    cat "$cand"
  fi
done

echo ""
echo "=== where.exe scan for airc-elev-probe-OUT.txt ==="
where.exe /R "C:\\" airc-elev-probe-OUT.txt 2>&1 | head
