#!/usr/bin/env bash
set -euo pipefail

# Configure Git to use repository hooks and optionally install Python pre-commit

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
cd "$repo_root"

echo "==> Setting core.hooksPath to .githooks"
git config core.hooksPath .githooks

chmod +x .githooks/pre-commit || true
chmod +x .githooks/pre-push || true

# If Python pre-commit is available, optionally wire it via .githooks wrapper.
# Note: 'pre-commit install' refuses to run when core.hooksPath is set.
if command -v pre-commit >/dev/null 2>&1; then
  echo "(info) Detected pre-commit CLI. Skipping 'pre-commit install' because core.hooksPath is set."
  echo "(info) You can run pre-commit manually via: pre-commit run --all-files"
  echo "(info) Or switch to pre-commit managed hooks with: git config --unset-all core.hooksPath && pre-commit install"
fi

echo "OK: Git hooks configured under .githooks."
echo "- pre-commit: runs clippy auto-fix, fmt+clippy, and cross clippy"
echo "- pre-push  : runs cross clippy (if configured)"
