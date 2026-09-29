#!/usr/bin/env bash
# Build only: no Snap Store credentials, login or publication.
# LXD must be initialized by canonical/setup-lxd before invoking this helper.
set -euo pipefail

if [[ "${GITHUB_ACTIONS:-}" != true || "${RUNNER_OS:-}" != Linux ||
      "${RUNNER_ENVIRONMENT:-}" != github-hosted ]]; then
    echo 'error: this provisioning helper is restricted to disposable GitHub-hosted Linux runners' >&2
    exit 2
fi
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
cd "$repo_root"
[[ -f Cargo.toml && -f snap/snapcraft.yaml ]]

# Match the former build action's stable Snapcraft selection and build metadata.
if snap list snapcraft >/dev/null 2>&1; then
    sudo snap refresh snapcraft --channel=latest/stable
else
    sudo snap install snapcraft --classic --channel=latest/stable
fi
export SNAPCRAFT_BUILD_INFO=1
export SNAPCRAFT_BUILD_ENVIRONMENT=lxd
snapcraft pack --use-lxd

# A green tool exit without an output is not a successful package build.
shopt -s nullglob
snaps=(./*.snap)
if (( ${#snaps[@]} == 0 )); then
    echo 'error: Snapcraft completed without a .snap artifact' >&2
    exit 1
fi
printf 'Snap artifact: %s\n' "${snaps[@]}"
