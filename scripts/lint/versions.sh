#!/usr/bin/env bash
# Keep all user-facing HyperDU version literals synchronized with either the
# workspace version or the latest published release.
set -uo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"

usage() {
    cat <<'USAGE'
Usage: versions.sh [--fix|--self-test]

  --fix        Rewrite workspace-group version literals.
  --self-test  Run parser/normalization regression tests only.
USAGE
}

fix=0
self_test_only=0
case "${1:-}" in
    --fix) fix=1 ;;
    --self-test) self_test_only=1 ;;
    -h | --help) usage; exit 0 ;;
    "") ;;
    *) echo "unknown arg: $1" >&2; usage >&2; exit 2 ;;
esac

# Match the complete version token, including repeated '-' components and '+'
# build metadata. '.' and '~' are also accepted because Debian/RPM package
# filenames use those spellings for the same prerelease.
pattern='[0-9]+\.[0-9]+\.[0-9]+([-~.+][0-9A-Za-z-]+([.~+][0-9A-Za-z-]+)*)?'

normalize_package_tokens() {
    local text="$1" expected="$2"
    local deb_form="${expected//-/.}" rpm_form="${expected//-/\~}"
    local IFS=$' \t/'
    local -a parts=()
    local token out=""

    read -r -a parts <<< "$text"
    for token in "${parts[@]}"; do
        case "$token" in
            *.deb*) token="${token//$deb_form/$expected}" ;;
        esac
        case "$token" in
            *.rpm*) token="${token//$rpm_form/$expected}" ;;
        esac
        out+="$token "
    done
    printf '%s\n' "$out"
}

self_test() {
    local expected='0.6.0-alpha--beta-+build.1'
    local got
    got="$(printf '%s\n' "$expected" | grep -oE "$pattern")"
    if [ "$got" != "$expected" ]; then
        echo "self-test: SemVer token truncated: '$got'" >&2
        return 1
    fi

    expected='0.6.0'
    got="$(printf '%s\n' "$expected" | grep -oE "$pattern")"
    if [ "$got" != "$expected" ]; then
        echo "self-test: stable version changed: '$got'" >&2
        return 1
    fi
    expected='0.6.0+build.1'
    got="$(printf '%s\n' "$expected" | grep -oE "$pattern")"
    if [ "$got" != "$expected" ]; then
        echo "self-test: release build metadata truncated: '$got'" >&2
        return 1
    fi
    expected='0.6.0-beta.3'
    got="$(printf '%s.\n' "$expected" | grep -oE "$pattern")"
    if [ "$got" != "$expected" ]; then
        echo "self-test: sentence punctuation became part of version: '$got'" >&2
        return 1
    fi
    expected='0.6.0-alpha-beta'
    local deb='0.6.0.alpha.beta'
    local line="https://example.invalid/releases/download/v$deb/hyperdu_${deb}_amd64.deb"
    local normalized
    normalized="$(normalize_package_tokens "$line" "$expected")"
    case "$normalized" in
        *"v$deb"*) ;;
        *) echo "self-test: URL tag was incorrectly normalized" >&2; return 1 ;;
    esac
    case "$normalized" in
        *"hyperdu_${expected}_amd64.deb"*) ;;
        *) echo "self-test: package filename was not normalized" >&2; return 1 ;;
    esac

    echo "  version parser self-test OK"
}

if [ "$self_test_only" -eq 1 ]; then
    self_test
    exit $?
fi

cd "$repo_root" || exit 1

# This must run before any resolving Cargo command (notably clippy), otherwise
# Cargo may silently rewrite a stale lockfile and hide the release mistake.
if ! cargo metadata --locked --no-deps --format-version 1 >/dev/null 2>&1; then
    echo "error: Cargo.lock is not synchronized with Cargo.toml." >&2
    echo "       Run 'cargo update -w' and commit Cargo.lock before continuing." >&2
    exit 1
fi

workspace_version="$(cargo pkgid -p hyperdu --manifest-path Cargo.toml 2>/dev/null | sed 's/.*[#@]//')"
if [ -z "$workspace_version" ]; then
    echo "error: could not determine the workspace version from cargo pkgid" >&2
    exit 1
fi

manifest_version="$(sed -n '/^\[workspace\.package\]/,/^\[/{s/^version *= *"\([^"]*\)".*/\1/p;}' Cargo.toml | head -n1)"
core_pin="$(sed -n 's/^hyperdu-core *=.*version *= *"\([^"]*\)".*/\1/p' Cargo.toml | head -n1)"
if [ "$manifest_version" != "$workspace_version" ]; then
    echo "error: Cargo.toml says $manifest_version but Cargo.lock resolves $workspace_version." >&2
    echo "       Run 'cargo update -w' and commit Cargo.lock." >&2
    exit 1
fi
if [ "$core_pin" != "$manifest_version" ]; then
    echo "error: [workspace.package] version is $manifest_version but the hyperdu-core" >&2
    echo "       dependency pin is $core_pin. Keep both root Cargo.toml versions equal." >&2
    exit 1
fi

# shellcheck disable=SC2016
released_version="$(grep -o 'releases/download/v[^/\"]*/' bucket/hyperdu.json |
    grep -v '\$version' | head -n1 | sed -e 's#releases/download/v##' -e 's#/$##')"
if [ -z "$released_version" ]; then
    echo "error: could not determine the released version from bucket/hyperdu.json" >&2
    exit 1
fi

THIRD_PARTY=(
    "README.md|uutils|0.8.0"
    "hyperdu/README.md|uutils|0.8.0"
    "plugin/plugin.json|agent-plugins.org|1.0.0"
    "scripts/package/winget.ps1|ManifestVersion|1.12.0"
    "scripts/package/winget.ps1|MinimumOSVersion|10.0.17763.0"
)

WORKSPACE_FILES=(
    packaging/man/hyperdu.1
    plugin/plugin.json
    scripts/package/snap.sh
    scripts/package/winget.ps1
    snap/snapcraft.yaml
    README.md README.en.md README.zh-CN.md
    hyperdu/README.md hyperdu/README.en.md hyperdu/README.zh-CN.md
    hyperdu-gui/README.md hyperdu-gui/README.en.md hyperdu-gui/README.zh-CN.md
    plugin/README.md plugin/README.en.md plugin/README.zh-CN.md
    site/_config.yml
)

# Current documentation explicitly describes the pending workspace release.
# The bucket instead advertises assets already published and must not be bumped
# until those assets exist.
RELEASE_FILES=(bucket/hyperdu.json)

fail=0
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

check_file() {
    local file="$1" expected="$2" group="$3"
    local hit num text scan_text found

    if [ ! -f "$file" ]; then
        echo "  missing: $file is listed in versions.sh but does not exist"
        fail=1
        unfixable=1
        return
    fi

    while IFS= read -r hit; do
        num="${hit%%:*}"
        text="${hit#*:}"
        case "$text" in *version-sync:ignore*) continue ;; esac

        # Debian/RPM spellings are valid only inside the package filename/path
        # component that carries .deb/.rpm. URL tag components and prose on the
        # same line remain untouched and are therefore still checked exactly.
        scan_text="$(normalize_package_tokens "$text" "$expected")"
        while IFS= read -r found; do
            [ -n "$found" ] || continue
            checked=$((checked + 1))
            [ "$found" = "$expected" ] && continue
            is_third_party "$file" "$text" "$found" && continue
            printf '  %s:%s: %s -- expected the %s version %s\n' \
                "$file" "$num" "$found" "$group" "$expected"
            fail=1
            if [ "$group" = workspace ]; then
                fixups+=("$file:$num:$found:$expected")
            else
                unfixable=1
            fi
        done < <(printf '%s\n' "$scan_text" | grep -oE "$pattern")
    done < <(grep -nE "$pattern" "$file")
}

self_test || exit 1

echo "  workspace $workspace_version (cargo pkgid -p hyperdu)"
echo "  released  $released_version (bucket/hyperdu.json)"
for target in "${WORKSPACE_FILES[@]}"; do
    check_file "$target" "$workspace_version" workspace
done
for target in "${RELEASE_FILES[@]}"; do
    check_file "$target" "$released_version" released
done

if [ "${GITHUB_REF_TYPE:-}" = tag ] && [ "${GITHUB_REF_NAME:-}" != "v$workspace_version" ]; then
    echo "  tag ${GITHUB_REF_NAME:-<unset>} does not name the workspace version $workspace_version"
    fail=1
    unfixable=1
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
    echo "note: workspace=$workspace_version, published=$released_version (valid between bump and release)."
fi

if [ "$fail" -ne 0 ]; then
    echo
    echo "FAIL: version drift ($checked literals checked)"
    echo "      workspace group: bash scripts/lint/versions.sh --fix"
    echo "      release group:   update by hand after the release exists"
    exit 1
fi

echo "  OK ($checked literals checked)"
