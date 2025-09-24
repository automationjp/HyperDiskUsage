#!/usr/bin/env bash
set -euo pipefail

# Configure Git to use repository hooks and optionally install Python pre-commit

repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
cd "$repo_root"

echo "==> Setting core.hooksPath to .githooks"
git config core.hooksPath .githooks

chmod +x .githooks/pre-commit || true

if command -v pre-commit >/dev/null 2>&1; then
  echo "==> Installing Python pre-commit hooks"
  pre-commit install --hook-type pre-commit -c .pre-commit-config.yaml
else
  echo "(info) 'pre-commit' command not found; skipping Python-hook install"
  echo "       You can install with: pip install pre-commit && pre-commit install"
fi

echo "OK: Git hooks configured. Commits will run scripts/lint.sh automatically."

