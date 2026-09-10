#!/usr/bin/env bash
# Exercise the required artifact policy read from the actual release workflow.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
tmp_base=$(cd -- "${TMPDIR:-/tmp}" && pwd)
fixture=$(mktemp -d "$tmp_base/hyperdu-artifact-test.XXXXXXXX")
case "$fixture" in "$tmp_base"/hyperdu-artifact-test.*) ;; *) exit 1 ;; esac
trap 'rm -rf -- "$fixture"' EXIT
mkdir -p "$fixture/scripts/package" "$fixture/dist"
cp "$root/scripts/package/report_artifacts.sh" "$fixture/scripts/package/"
required=()
while IFS= read -r glob; do
    required+=(--required "$glob")
done < <(sed -n "s/^[[:space:]]*--required '\([^']*\)'.*/\1/p" "$root/.github/workflows/release.yml")
[[ ${#required[@]} -gt 0 ]] || { echo 'No required artifact policy found' >&2; exit 1; }
cli_deb=hyperdu_0.5.0-beta.2_amd64.deb
gui_deb=hyperdu-gui_0.5.0-beta.2_amd64.deb
touch "$fixture/dist/hyperdu-linux-x86_64-generic.zip" \
      "$fixture/dist/hyperdu-gui-linux-x86_64-generic.zip" \
      "$fixture/dist/$cli_deb" "$fixture/dist/$gui_deb"
run_report() {
    GITHUB_STEP_SUMMARY= bash "$fixture/scripts/package/report_artifacts.sh" \
        "${required[@]}" > "$fixture/result.log" 2>&1
}
run_report || { cat "$fixture/result.log"; exit 1; }
for missing in "$cli_deb" "$gui_deb"; do
    rm -- "$fixture/dist/$missing"
    if run_report; then
        echo "Artifact policy accepted missing $missing" >&2
        exit 1
    fi
    touch "$fixture/dist/$missing"
done
run_report || { cat "$fixture/result.log"; exit 1; }
echo 'PASS: both DEB packages are independently required'

# Run the actual release case arm with a failing local packager. A verification
# rejection must remain visible in logs without failing the other formats.
appimage_case=$(sed -n '/^      linux-appimage)/,/^      linux-snap)/p' \
    "$root/scripts/package/release.sh" | sed '$d')
[[ -n "$appimage_case" ]] || { echo 'No AppImage release policy found' >&2; exit 1; }
printf '#!/bin/sh\necho "fixture: local AppImage verification rejected" >&2\nexit 1\n' \
    > "$fixture/scripts/package/appimage.sh"
(
    root_dir="$fixture"
    dist_dir="$fixture/dist"
    os_tag=linux
    tag=linux-appimage
    eval "case \"\$tag\" in $appimage_case esac"
) > "$fixture/appimage-result.log" 2>&1 || {
    cat "$fixture/appimage-result.log"
    exit 1
}
grep -q 'warn: optional AppImage skipped' "$fixture/appimage-result.log"
grep -q 'fixture: local AppImage verification rejected' "$fixture/dist/appimage-pack.log"
echo 'PASS: optional AppImage failure warns and preserves its diagnostic log'
