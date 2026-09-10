#!/usr/bin/env bash
set -euo pipefail

# Explicit reviewed local inputs only: no PATH fallback or mutable downloads.
# Usage: verified-appimage-tools.sh LINUXDEPLOY SHA256 APPIMAGETOOL SHA256
# Prints a private directory on success. Caller owns its cleanup. Never executes tools.
[[ $# -eq 4 ]] || { echo 'error: provide both reviewed tool paths and SHA256 values' >&2; exit 1; }
for path in "$1" "$3"; do
  [[ "$path" = /* && -f "$path" && ! -L "$path" ]] || {
    echo 'error: AppImage tools must be explicit absolute regular local files' >&2; exit 1;
  }
done
for hash in "$2" "$4"; do
  [[ "$hash" =~ ^[a-fA-F0-9]{64}$ ]] || {
    echo 'error: each reviewed AppImage tool requires its SHA256' >&2; exit 1;
  }
done
command -v sha256sum >/dev/null || { echo 'error: sha256sum is required' >&2; exit 1; }
umask 077
staged=$(mktemp -d "${TMPDIR:-/tmp}/hyperdu-appimage.XXXXXXXX")
cleanup() { rm -rf -- "$staged"; }
trap cleanup EXIT
trap 'exit 1' HUP INT TERM
# Verify the private copies, not the sources, so replacement of a source after
# verification cannot change the bytes subsequently selected for execution.
cp -- "$1" "$staged/linuxdeploy"
cp -- "$3" "$staged/appimagetool"
chmod 600 "$staged/linuxdeploy" "$staged/appimagetool"
(cd "$staged" && printf '%s  linuxdeploy\n%s  appimagetool\n' "$2" "$4" | sha256sum --check --status) || {
  echo 'error: AppImage tool SHA256 mismatch; nothing installed or executed' >&2; exit 1;
}
chmod 700 "$staged/linuxdeploy" "$staged/appimagetool"
printf '%s\n' "$staged"
trap - EXIT
