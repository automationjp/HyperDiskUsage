#!/usr/bin/env bash
# Every version literal outside Cargo.toml must be accounted for.
#
# This exists because snap/snapcraft.yaml, the generator that writes it, and the
# agent plugin manifest sat at 0.4.0 while the workspace moved to 0.5.0-beta.2.
# Nothing pointed at them. The bump is manual, the set of files carrying a
# version was rediscovered by grep each time, and AGENTS.md claimed a release
# bumps Cargo.toml "and nowhere else" -- which was never true of the packaging
# tree. A literal that no tool reads is a literal nobody updates.
#
# Two sources of truth, because two different questions are being answered:
#
#   workspace  what this tree is right now      cargo pkgid -p hyperdu-cli
#   released   what the public can install      the download URL in
#                                               bucket/hyperdu.json, which sits
#                                               next to a SHA256 of a real asset
#
# Each file belongs to exactly one group. snap/snapcraft.yaml labels the source
# being built, so it follows the workspace. README install commands and the
# scoop bucket point at published artefacts, so they follow the release and must
# NOT be bumped ahead of one: doing that advertises downloads that 404. The two
# agree except between a bump and its release, so --fix rewrites the workspace
# group only; the release group is reconciled once the assets exist.
#
# Scope is files that carry a version into something a user installs or reads.
# An incident note in a script that happens to name an old tag is not that, and
# is deliberately out.
#
# Escape hatches, both with precedent in scripts/lint/paths.sh:
#   * a version belonging to somebody else goes in THIRD_PARTY below
#   * a line naming a version as prose opts out with `version-sync:ignore`

set -uo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
cd "$repo_root" || exit 1

usage() {
    cat <<'USAGE'
Usage: versions.sh [--fix]

Checks that every version literal in the packaging and documentation tree reads
either the workspace version or the last published release.

  --fix   Rewrite the workspace-group files to the workspace version. The
          release group is never rewritten: it names published assets, so it is
          only correct once those assets exist. Exits non-zero anyway if the run
          also found something --fix cannot touch.
USAGE
}

fix=0
case "${1:-}" in
    --fix) fix=1 ;;
    -h | --help)
        usage
        exit 0
        ;;
    "") ;;
    *)
        echo "unknown arg: $1" >&2
        usage >&2
        exit 2
        ;;
esac

# The crates inherit `version.workspace = true`, so the literal is in no crate
# manifest to grep for. pkgid prints `<url>#<version>`.
workspace_version="$(cargo pkgid -p hyperdu-cli --manifest-path Cargo.toml 2>/dev/null | sed 's/.*[#@]//')"
if [ -z "$workspace_version" ]; then
    echo "error: could not determine the workspace version from cargo pkgid" >&2
    exit 1
fi

# pkgid answers from Cargo.lock, so it reports the last resolved version rather
# than what the manifest now asks for. A bump that forgets the lock therefore
# looks consistent to every check below -- and this repo has already shipped
# that bug once, which is why Cargo.toml carries a comment about it. Cross-check
# the two literals the manifest does hold before trusting the answer.
manifest_version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}' Cargo.toml | head -n1)"
core_pin="$(sed -n 's/^hyperdu-core *=.*version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
if [ "$manifest_version" != "$workspace_version" ]; then
    echo "error: Cargo.toml says $manifest_version but Cargo.lock resolves $workspace_version." >&2
    echo "       Run 'cargo update -w' and commit Cargo.lock." >&2
    exit 1
fi
if [ "$core_pin" != "$manifest_version" ]; then
    echo "error: [workspace.package] version is $manifest_version but the hyperdu-core" >&2
    echo "       dependency pin is $core_pin. cargo publish rewrites the path dependency" >&2
    echo "       into a registry one and needs those to agree." >&2
    exit 1
fi

# The autoupdate block holds a literal `$version` placeholder rather than a
# version, so it is dropped before the first concrete URL is taken. The single
# quotes are the point: scoop expands that placeholder, this script must not.
# shellcheck disable=SC2016
released_version="$(grep -o 'releases/download/v[^/"]*/' bucket/hyperdu.json |
    grep -v '\$version' | head -n1 | sed -e 's#releases/download/v##' -e 's#/$##')"
if [ -z "$released_version" ]; then
    echo "error: could not determine the released version from bucket/hyperdu.json" >&2
    exit 1
fi

# Versions that are not HyperDU's, as FILE|CONTEXT|LITERAL. The literal is
# excused only on a line of that file which also contains CONTEXT.
#
# The scoping is the point. A bare list of numbers would have excused
# plugin/plugin.json's own "version" field the day it regressed to 1.0.0,
# because that file already carries 1.0.0 in the agent-plugins.org schema URL --
# and an allowlist that hides real drift is worse than no allowlist. Anchoring
# each entry to the text that makes it somebody else's version keeps it honest.
THIRD_PARTY=(
    # uutils coreutils, the du in the benchmark tables
    "README.md|uutils|0.8.0"
    "hyperdu-cli/README.md|uutils|0.8.0"
    # the schema the manifest validates against, not the plugin's own version
    "plugin/plugin.json|agent-plugins.org|1.0.0"
    # winget's manifest schema version and its minimum Windows build
    "scripts/package/winget.ps1|ManifestVersion|1.12.0"
    "scripts/package/winget.ps1|MinimumOSVersion|10.0.17763.0"
)

# Describes the tree as it is now.
WORKSPACE_FILES=(
    packaging/man/hyperdu.1
    plugin/plugin.json
    scripts/package/snap.sh
    scripts/package/winget.ps1
    snap/snapcraft.yaml
)

# Names artefacts that are already published.
RELEASE_FILES=(
    README.md
    bucket/hyperdu.json
    hyperdu-cli/README.md
    hyperdu-gui/README.md
    hyperdu-mcp/README.md
    site/_config.yml
)

# '~' is in the class so an RPM spelling reads as one token: without it,
# 0.5.0~beta.2 would be truncated to 0.5.0 and then flagged against itself.
pattern='[0-9]+\.[0-9]+\.[0-9]+([-.~][0-9A-Za-z.~]+)?'

fail=0
# Failures --fix cannot resolve: a release-group file, a missing file, a tag
# that disagrees with the tree. Tracked apart so --fix cannot rewrite the
# workspace group, exit 0, and bury them.
unfixable=0
checked=0
fixups=()

is_third_party() {
    local file="$1" text="$2" literal="$3"
    local entry entry_file entry_ctx entry_lit
    for entry in "${THIRD_PARTY[@]}"; do
        entry_file="${entry%%|*}"
        entry_lit="${entry##*|}"
        entry_ctx="${entry#*|}"
        entry_ctx="${entry_ctx%|*}"
        [ "$file" = "$entry_file" ] || continue
        [ "$literal" = "$entry_lit" ] || continue
        case "$text" in *"$entry_ctx"*) return 0 ;; esac
    done
    return 1
}

# cargo-deb cannot put a '-' in a Debian version and RPM rejects one outright,
# so the same release is spelled three ways: 0.5.0-beta.2 for cargo,
# 0.5.0.beta.2 in a .deb filename, 0.5.0~beta.2 in an .rpm one. Only a line that
# actually names such a file may use the packaged spellings. Accepting them
# everywhere would have let a dot-for-hyphen slip pass in snapcraft.yaml or
# bucket/hyperdu.json, where the exact hyphenated form is what the tool consumes
# and a near miss is a broken value rather than a cosmetic one.
names_a_package_file() {
    case "$1" in
        *.deb* | *.rpm*) return 0 ;;
    esac
    return 1
}

check_file() {
    local file="$1" expected="$2" group="$3"
    local deb_form="${expected//-/.}"
    local rpm_form="${expected//-/\~}"
    local hit num text found

    if [ ! -f "$file" ]; then
        echo "  missing: $file is listed in versions.sh but does not exist"
        fail=1
        unfixable=1
        return
    fi

    while IFS= read -r hit; do
        num="${hit%%:*}"
        text="${hit#*:}"

        # Prose about a version, not a claim about this one.
        case "$text" in *version-sync:ignore*) continue ;; esac

        while IFS= read -r found; do
            [ -n "$found" ] || continue
            checked=$((checked + 1))
            [ "$found" = "$expected" ] && continue
            is_third_party "$file" "$text" "$found" && continue
            if names_a_package_file "$text"; then
                [ "$found" = "$deb_form" ] && continue
                [ "$found" = "$rpm_form" ] && continue
            fi
            printf '  %s:%s: %s -- expected the %s version %s\n' \
                "$file" "$num" "$found" "$group" "$expected"
            fail=1
            if [ "$group" = workspace ]; then
                fixups+=("$file:$num:$found:$expected")
            else
                unfixable=1
            fi
        done < <(printf '%s\n' "$text" | grep -oE "$pattern")
    done < <(grep -nE "$pattern" "$file")
}

echo "  workspace $workspace_version (cargo pkgid -p hyperdu-cli)"
echo "  released  $released_version (bucket/hyperdu.json)"

for target in "${WORKSPACE_FILES[@]}"; do
    check_file "$target" "$workspace_version" workspace
done
for target in "${RELEASE_FILES[@]}"; do
    check_file "$target" "$released_version" released
done

# On a tag build the tag names the release being cut, so it has to agree with
# what the tree carries. Both variables are set only by GitHub Actions.
if [ "${GITHUB_REF_TYPE:-}" = tag ]; then
    if [ "${GITHUB_REF_NAME:-}" != "v$workspace_version" ]; then
        echo "  tag ${GITHUB_REF_NAME:-<unset>} does not name the workspace version $workspace_version"
        fail=1
        unfixable=1
    fi
fi

if [ "$fix" -eq 1 ] && [ "${#fixups[@]}" -gt 0 ]; then
    echo
    echo "==> rewriting the workspace group"
    for entry in "${fixups[@]}"; do
        file="${entry%%:*}"
        rest="${entry#*:}"
        num="${rest%%:*}"
        rest="${rest#*:}"
        found="${rest%%:*}"
        want="${rest#*:}"
        # Whole tokens only. `sed s///` is a substring match, so a stale 0.4.0
        # would also rewrite the 0.4.0 sitting inside an unrelated 10.4.0.1 --
        # and since fixups holds one entry per match rather than per line, the
        # neighbouring-character test has to be exact rather than incidental.
        if awk -v n="$num" -v old="$found" -v new="$want" '
            NR != n { print; next }
            {
                line = $0; out = ""
                while ((p = index(line, old)) > 0) {
                    before = (p > 1) ? substr(line, p - 1, 1) : ""
                    after_at = p + length(old)
                    after = (after_at <= length(line)) ? substr(line, after_at, 1) : ""
                    whole = (before !~ /[0-9.~-]/) && (after !~ /[0-9.~-]/)
                    out = out substr(line, 1, p - 1) (whole ? new : old)
                    line = substr(line, after_at)
                }
                print out line
            }
        ' "$file" > "$file.tmp"; then
            mv -f "$file.tmp" "$file"
            echo "  $file:$num  $found -> $want"
        else
            rm -f "$file.tmp"
            echo "  FAILED to rewrite $file:$num" >&2
            unfixable=1
        fi
    done
    echo
    if [ "$unfixable" -ne 0 ]; then
        echo "but this run also found problems --fix cannot touch; see above"
        exit 1
    fi
    echo "re-run without --fix to confirm"
    exit 0
fi

if [ "$released_version" != "$workspace_version" ]; then
    echo
    echo "note: the workspace is at $workspace_version, the published release at $released_version."
    echo "      That is expected between a bump and its release. Once v$workspace_version is"
    echo "      published, point the release group at it:"
    printf '        %s\n' "${RELEASE_FILES[@]}"
    echo "      bucket/hyperdu.json also needs the new asset's SHA256."
fi

if [ "$fail" -ne 0 ]; then
    echo
    echo "FAIL: version drift ($checked literals checked)"
    echo "      workspace group: bash scripts/lint/versions.sh --fix"
    echo "      release group:   update by hand, after the release exists"
    echo "      somebody else's version: add a FILE|CONTEXT|LITERAL line to"
    echo "                               THIRD_PARTY in this script"
    exit 1
fi

echo "  OK ($checked literals checked)"
