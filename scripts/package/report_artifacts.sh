#!/usr/bin/env bash
# Say out loud what the release actually contains.
#
# Every packaging step in .github/workflows/release.yml is allowed to fail, and
# for v0.5.0-beta.2 three of them did: the snap could not find Cargo.toml, the
# rpm manifest had two fields of the wrong TOML type, and the AppImage ran
# before linuxdeploy had been downloaded. The job was green, the release
# published, and the only way to notice was to count the files on the releases
# page.
#
# The tolerance is deliberate -- these formats need tools that are not on every
# runner, and a missing .rpm should not withhold the binaries that did build --
# but "allowed to fail" and "nobody is told" are different things. A missing
# required glob fails the step; a missing optional one is reported as a warning
# and lands in the job summary, so the next release states which packagers
# worked.
#
# Promote an optional glob to required once it has produced a file two releases
# running. That is the whole maintenance protocol.

set -uo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
dist_dir="$repo_root/dist"

usage() {
    cat <<'USAGE'
Usage: report_artifacts.sh [--required GLOB]... [--optional GLOB]...

Checks dist/ for each glob. A missing --required glob fails the run; a missing
--optional one is reported and does not. Both are written to the GitHub job
summary when GITHUB_STEP_SUMMARY is set.
USAGE
}

required=()
optional=()

while [ $# -gt 0 ]; do
    case "$1" in
        --required)
            if [ $# -lt 2 ]; then
                echo "error: --required needs a glob" >&2
                exit 2
            fi
            required+=("$2")
            shift 2
            ;;
        --optional)
            if [ $# -lt 2 ]; then
                echo "error: --optional needs a glob" >&2
                exit 2
            fi
            optional+=("$2")
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown arg: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ ! -d "$dist_dir" ]; then
    echo "::error::dist/ does not exist; nothing was packaged"
    exit 1
fi

summary=""
note() {
    echo "$1"
    summary="$summary$1"$'\n'
}

# compgen rather than an unquoted glob: it reports "no match" with a non-zero
# exit instead of handing back the pattern itself, which would count as a hit.
matches_for() {
    local glob="$1"
    (cd "$dist_dir" && compgen -G "$glob") 2>/dev/null || true
}

fail=0

check() {
    local glob="$1" kind="$2"
    local found first="" count=0

    while IFS= read -r found; do
        [ -n "$found" ] || continue
        count=$((count + 1))
        [ -n "$first" ] || first="$found"
    done < <(matches_for "$glob")

    if [ "$count" -gt 1 ]; then
        note "| \`$glob\` | $kind | ok | $first (+$((count - 1))) |"
        return
    fi
    if [ "$count" -eq 1 ]; then
        note "| \`$glob\` | $kind | ok | $first |"
        return
    fi

    if [ "$kind" = required ]; then
        echo "::error::no release artifact matched $glob"
        note "| \`$glob\` | $kind | **MISSING** | - |"
        fail=1
    else
        echo "::warning::no release artifact matched $glob; that packager produced nothing"
        note "| \`$glob\` | $kind | missing | - |"
    fi
}

note "| Pattern | Kind | Result | File |"
note "| --- | --- | --- | --- |"

for glob in ${required[@]+"${required[@]}"}; do check "$glob" required; done
for glob in ${optional[@]+"${optional[@]}"}; do check "$glob" optional; done

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### Release artifacts"
        echo
        printf '%s' "$summary"
        echo
        echo "<details><summary>Everything in dist/</summary>"
        echo
        echo '```'
        (cd "$dist_dir" && ls -1) || true
        echo '```'
        echo
        echo "</details>"
    } >> "$GITHUB_STEP_SUMMARY"
fi

echo
echo "dist/ contents:"
(cd "$dist_dir" && ls -1) || true

if [ "$fail" -ne 0 ]; then
    echo
    echo "FAIL: a required artifact is missing from dist/"
    exit 1
fi
