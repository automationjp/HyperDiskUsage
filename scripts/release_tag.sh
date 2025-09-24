#!/usr/bin/env bash
set -euo pipefail

# HyperDiskUsage release helper
# - Tags current HEAD (vX.Y.Y) and pushes to origin to trigger GitHub Actions
# - Optional: build local artifacts and publish a GitHub Release via gh (no CI)

usage() {
  cat <<USAGE
Usage: $(basename "$0") vMAJOR.MINOR.PATCH [options]

Options:
  --no-lint           Skip running scripts/lint.sh before tagging
  --force-tag         Delete remote tag if it already exists, then re-create
  --push-branch BR    Push this branch (default: current)
  --publish-local     Build artifacts locally and publish with gh (skip CI)
  --targets LIST      Targets for local packaging (default: linux-gnu,linux-musl,linux-aarch64)
  --cpu-flavors LIST  CPU flavors for local packaging (default: generic)
  --notes FILE        Release notes file for gh release (local publish)
  -h, --help          Show this help

Examples:
  $(basename "$0") v0.0.1                   # tag + push -> CI builds + release
  $(basename "$0") v0.0.1 --force-tag       # re-tag and re-trigger CI
  $(basename "$0") v0.0.1 --publish-local   # build + gh release (no CI)
USAGE
}

if [[ $# -lt 1 ]]; then usage; exit 1; fi

TAG="$1"; shift || true
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "error: tag must be like vX.Y.Z" >&2; exit 1; }

RUN_LINT=1
FORCE_TAG=0
ALLOW_DIRTY=0
VERBOSE=0
NO_HOOKS=0
PUSH_BRANCH=""
PUBLISH_LOCAL=0
TARGETS="linux-gnu,linux-musl,linux-aarch64"
CPU_FLAVORS="generic"
NOTES_FILE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-lint) RUN_LINT=0; shift;;
    --force-tag) FORCE_TAG=1; shift;;
    --allow-dirty) ALLOW_DIRTY=1; shift;;
    --push-branch) PUSH_BRANCH="$2"; shift 2;;
    --no-hooks) NO_HOOKS=1; shift;;
    --verbose) VERBOSE=1; shift;;
    --publish-local) PUBLISH_LOCAL=1; shift;;
    --targets) TARGETS="$2"; shift 2;;
    --cpu-flavors) CPU_FLAVORS="$2"; shift 2;;
    --notes) NOTES_FILE="$2"; shift 2;;
    -h|--help) usage; exit 0;;
    *) echo "unknown arg: $1" >&2; usage; exit 1;;
  esac
done

# Move to repo root robustly (handles broken CWD in WSL)
move_to_repo_root() {
  # Prefer Git’s notion of the repo root if available
  if rr=$(git rev-parse --show-toplevel 2>/dev/null); then
    cd "$rr" || {
      echo "error: failed to cd to git toplevel $rr" >&2; exit 1; }
    return
  fi
  # Fallback: derive from this script path relative to current PWD
  local sp="${BASH_SOURCE[0]}"
  case "$sp" in
    /*) ;;                             # already absolute
    *) sp="$PWD/$sp" ;;                # make absolute from current dir
  esac
  local rr
  rr=$(cd "$(dirname "$sp")/.." && pwd) || {
    echo "error: failed to resolve repository root; run from repo root and try again" >&2
    exit 1
  }
  cd "$rr" || { echo "error: cannot cd to $rr" >&2; exit 1; }
}

move_to_repo_root

# Ensure git is clean (unless allowed)
git update-index -q --refresh || true
if [[ $ALLOW_DIRTY -eq 0 ]]; then
  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: working tree has changes. Commit or stash first, or pass --allow-dirty" >&2
    exit 1
  fi
fi

BRANCH="${PUSH_BRANCH:-$(git rev-parse --abbrev-ref HEAD)}"

if [[ $VERBOSE -eq 1 ]]; then
  set -x
fi

if [[ $RUN_LINT -eq 1 ]]; then
  if command -v cargo >/dev/null 2>&1; then
    echo "==> Lint (fmt + clippy)"
    bash scripts/lint.sh
  else
    echo "(info) cargo not found; skipping lint. Use --no-lint to silence."
  fi
fi

if [[ $PUBLISH_LOCAL -eq 1 ]]; then
  command -v gh >/dev/null 2>&1 || { echo "error: gh CLI not found" >&2; exit 1; }
  echo "==> Local packaging"
  bash scripts/package_release.sh --targets "$TARGETS" --cpu-flavors "$CPU_FLAVORS" --deb --rpm --verbose
  echo "==> Creating GitHub Release: $TAG"
  if gh release view "$TAG" >/dev/null 2>&1; then
    echo "(info) release $TAG exists; updating assets"
  else
    gh release create "$TAG" ${NOTES_FILE:+-F "$NOTES_FILE"} -t "$TAG" || true
  fi
  gh release upload "$TAG" dist/* --clobber
  echo "OK: released $TAG"
  exit 0
fi

echo "==> Tag + push (CI-triggered release)"
git fetch --tags origin
if git ls-remote --tags origin | grep -q "refs/tags/$TAG$"; then
  if [[ $FORCE_TAG -eq 1 ]]; then
    echo "(info) deleting remote tag $TAG"
    git push origin ":refs/tags/$TAG"
  else
    echo "error: remote tag $TAG already exists (use --force-tag to replace)" >&2
    exit 1
  fi
fi

if git rev-parse -q --verify "$TAG" >/dev/null; then
  if [[ $FORCE_TAG -eq 1 ]]; then
    git tag -d "$TAG"
  else
    echo "error: local tag $TAG exists (use --force-tag)" >&2
    exit 1
  fi
fi

git tag -a "$TAG" -m "$TAG"
if [[ $NO_HOOKS -eq 1 ]]; then
  git push --no-verify origin "$BRANCH"
  git push --no-verify origin "$TAG"
else
  git push origin "$BRANCH"
  git push origin "$TAG"
fi
echo "OK: pushed $TAG (branch=$BRANCH). GitHub Actions will build and publish assets."
