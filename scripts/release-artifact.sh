#!/usr/bin/env bash
# Shared installer preparation: fetch exact published bytes before maintenance.
# The caller owns the unique destination and verifies the executable build SHA.
airc_release_artifact() (
  set -euo pipefail
  local destination="$1" revision="$2" checkout="$3" target suffix binary tag asset base digest listed
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64) target=x86_64-unknown-linux-gnu; suffix=tar.gz; binary=airc ;;
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin; suffix=tar.gz; binary=airc ;;
    Darwin:x86_64) target=x86_64-apple-darwin; suffix=tar.gz; binary=airc ;;
    MINGW*:x86_64|MSYS*:x86_64|CYGWIN*:x86_64) target=x86_64-pc-windows-msvc; suffix=zip; binary=airc.exe ;;
    *) echo 'No published AIRC artifact for this platform.' >&2; exit 1 ;;
  esac
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || { echo 'Expected full artifact revision.' >&2; exit 1; }
  tag="$(git -C "$checkout" describe --tags --exact-match "$revision" 2>/dev/null || true)"
  [[ "$tag" =~ ^v[0-9][A-Za-z0-9._-]*$ ]] || tag="canary-${revision}"
  asset="airc-${tag}-${target}.${suffix}"
  base="https://github.com/CambrianTech/airc/releases/download/$tag"
  local temporary; temporary="$(mktemp -d)"
  trap 'rm -rf "$temporary"' EXIT
  curl --fail --location --silent --show-error --max-time 60 --max-filesize 4096 "$base/$asset.sha256" -o "$temporary/sum" || {
    echo "AIRC artifact $tag is not available; no source build will be attempted. Contributors may select --developer-build." >&2; exit 1;
  }
  read -r digest listed < "$temporary/sum"
  listed="${listed#\*}"
  [[ "$digest" =~ ^[0-9a-fA-F]{64}$ ]] && [ "$listed" = "$asset" ] && [ "$(wc -l < "$temporary/sum")" -eq 1 ] || { echo 'Invalid artifact checksum receipt.' >&2; exit 1; }
  curl --fail --location --silent --show-error --max-time 600 --max-filesize 268435456 "$base/$asset" -o "$temporary/archive" || exit 1
  local actual
  if command -v sha256sum >/dev/null; then actual="$(sha256sum "$temporary/archive")"; else actual="$(shasum -a 256 "$temporary/archive")"; fi
  [ "${actual%% *}" = "$digest" ] || { echo 'AIRC archive checksum mismatch.' >&2; exit 1; }
  # Read exactly the sole regular member; never extract archive-controlled paths.
  if [ "$suffix" = zip ]; then
    [ "$(unzip -Z1 "$temporary/archive")" = "$binary" ] || { echo 'Unexpected AIRC archive members.' >&2; exit 1; }
    unzip -p "$temporary/archive" "$binary" > "$destination" || exit 1
  else
    [ "$(tar -tzf "$temporary/archive")" = "$binary" ] && [[ "$(tar -tvzf "$temporary/archive")" == -* ]] || { echo 'Unexpected AIRC archive members.' >&2; exit 1; }
    tar -xOzf "$temporary/archive" "$binary" > "$destination" || exit 1
  fi
  [ -s "$destination" ] || { echo 'Empty AIRC artifact.' >&2; exit 1; }
  chmod +x "$destination"
)
