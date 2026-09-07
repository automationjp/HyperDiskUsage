#!/usr/bin/env bash
# Every script path mentioned anywhere in the tree must resolve.
#
# This exists because reorganising the helper scripts into subdirectories broke
# CI on both platforms, and the ad-hoc check meant to catch it did not. That
# check only looked for forward-slash literals, so two forms went past it: a
# glob, which silently matches nothing once files move, and a backslash spelling
# used by the PowerShell call sites. The backslash one only runs on a tagged
# release, so a broken reference there could have survived until publish day.
#
# Deliberately depends on nothing but bash and git. Every other step in the lint
# pipeline skips when its tool is missing, which is how the broken glob passed a
# local run: shellcheck was not installed, so nothing evaluated it.
#
# A line that mentions a path in prose rather than calling it can opt out with
# the marker `lint-paths:ignore`.

set -uo pipefail

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/../.. && pwd)"
cd "$repo_root" || exit 1

# Leading path components matter: matching only from the directory name onwards
# turned the skill's own bundled script into a path that resolves nowhere and
# reported a working reference as broken. lint-paths:ignore
pattern='[A-Za-z0-9_./\\-]*scripts[\\/][A-Za-z0-9_.*/\\-]+\.(sh|ps1)'

fail=0
checked=0

while IFS= read -r hit; do
    file="${hit%%:*}"
    rest="${hit#*:}"
    content="${rest#*:}"

    # Prose, not a call site.
    case "$content" in *lint-paths:ignore*) continue ;; esac

    while IFS= read -r ref; do
        [ -n "$ref" ] || continue

        # Backslashes are how the PowerShell call sites spell the same path; a
        # leading ./ or .\ carries no meaning here.
        norm="${ref//\\//}"
        norm="${norm#./}"

        # Call sites in the shell scripts write "$root_dir/scripts/...", and the
        # capture keeps the variable name because the `$` is not part of a path.
        # Trying the tail from the last directory segment covers that without
        # having to know which variables hold the repo root.
        tail_ref="scripts/${norm##*scripts/}"

        # A reference resolves against the repo root, against the directory of
        # the file that mentions it, or -- for the variable case above -- from
        # the last path segment onwards.
        ok=0
        for cand in "$norm" "$(dirname -- "$file")/$norm" "$tail_ref"; do
            case "$cand" in
                *'*'*)
                    # A glob matching nothing is the failure that broke CI, so
                    # expansion has to be checked, not just the spelling.
                    compgen -G "$cand" >/dev/null 2>&1 && ok=1
                    ;;
                *)
                    [ -e "$cand" ] && ok=1
                    ;;
            esac
            [ "$ok" -eq 1 ] && break
        done

        checked=$((checked + 1))
        if [ "$ok" -eq 0 ]; then
            echo "  BROKEN  $ref" >&2
            echo "          referenced by $file" >&2
            fail=1
        fi
    done < <(printf '%s\n' "$content" | grep -oE "$pattern" | sort -u)
done < <(git grep -nIE "$pattern" -- ':!target' 2>/dev/null)

if [ "$fail" -ne 0 ]; then
    echo "script path references do not resolve" >&2
    exit 1
fi

echo "(info) $checked script path references resolve"
