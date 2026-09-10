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
cli_deb=hyperdu-cli_0.5.0-beta.2_amd64.deb
gui_deb=hyperdu-gui_0.5.0-beta.2_amd64.deb
touch "$fixture/dist/hyperdu-cli-linux-x86_64-generic.zip" \
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
