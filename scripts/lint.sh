#!/usr/bin/env bash
set -euo pipefail

# Ensure cargo/rustup available (basic check)
if ! command -v cargo >/dev/null 2>&1; then
  # If invoked via sudo and cargo is not in root's PATH, drop to invoking user once.
  if [[ ${EUID:-$(id -u)} -eq 0 && -n "${SUDO_USER:-}" && "${SUDO_USER}" != root ]]; then
    echo "info: cargo not found under sudo; re-running as ${SUDO_USER}" >&2
    # Unset SUDO_USER to avoid recursive handoff loops in the child process
    exec sudo -u "${SUDO_USER}" -H env -u SUDO_USER bash "$0" "$@"
  fi
  echo "error: cargo not found in PATH. Please install Rust (rustup) or run without sudo." >&2
  exit 1
fi

# Helper: install a package using the system package manager
install_pkg() {
  local pkg="$1"
  if command -v apt-get >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1 && [[ ${EUID:-$(id -u)} -ne 0 ]]; then sudo apt-get update -y || true; sudo apt-get install -y "$pkg" || true; else apt-get update -y || true; apt-get install -y "$pkg" || true; fi
  elif command -v dnf >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1 && [[ ${EUID:-$(id -u)} -ne 0 ]]; then sudo dnf install -y "$pkg" || true; else dnf install -y "$pkg" || true; fi
  elif command -v yum >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1 && [[ ${EUID:-$(id -u)} -ne 0 ]]; then sudo yum install -y "$pkg" || true; else yum install -y "$pkg" || true; fi
  elif command -v pacman >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1 && [[ ${EUID:-$(id -u)} -ne 0 ]]; then sudo pacman -Sy --noconfirm "$pkg" || true; else pacman -Sy --noconfirm "$pkg" || true; fi
  elif command -v zypper >/dev/null 2>&1; then
    if command -v sudo >/dev/null 2>&1 && [[ ${EUID:-$(id -u)} -ne 0 ]]; then sudo zypper install -y "$pkg" || true; else zypper install -y "$pkg" || true; fi
  elif command -v brew >/dev/null 2>&1; then
    brew install "$pkg" || true
  elif command -v choco >/dev/null 2>&1; then
    choco install -y "$pkg" || true
  elif command -v scoop >/dev/null 2>&1; then
    if ! scoop bucket list | rg -q "extras"; then scoop bucket add extras || true; fi
    scoop install "$pkg" || true
  else
    echo "(info) no supported package manager found to install $pkg" >&2
  fi
}

usage() {
  cat <<USAGE
Usage: $(basename "$0")

Runs formatting and lint checks for the workspace:
  - cargo fmt --all -- --check
  - cargo clippy --workspace -- -D warnings
  - cargo deny check   (if cargo-deny is installed)

Options:
  -h, --help   Show this help
USAGE
}

for a in "$@"; do case "$a" in -h|--help) usage; exit 0;; esac; done

echo "==> rustfmt (check)"
cargo fmt --all -- --check

echo "==> clippy (workspace, deny warnings)"
cargo clippy --workspace -- -D warnings

# Optional: strict import sort if nightly rustfmt is present
# Install nightly rustfmt on demand if missing
if ! rustup toolchain list 2>/dev/null | rg -q '^nightly'; then
  echo "(info) installing nightly rustfmt for strict import order..."
  rustup toolchain install nightly -c rustfmt || echo "(warn) failed to install nightly rustfmt"
fi
if rustup toolchain list 2>/dev/null | rg -q '^nightly'; then
  echo "==> rustfmt (nightly strict import order, check)"
  bash scripts/fmt_strict.sh --check
else
  echo "(info) nightly rustfmt not found; skipping strict import order check"
fi

# Optional: shellcheck for POSIX scripts if available
# Install shellcheck on demand if missing
if ! command -v shellcheck >/dev/null 2>&1; then
  echo "(info) installing shellcheck..."
  install_pkg shellcheck
fi
if command -v shellcheck >/dev/null 2>&1; then
  echo "==> shellcheck (scripts)"
  shellcheck -S error -x scripts/*.sh
else
  echo "(info) shellcheck not found; skipping shell lint"
fi

if command -v cargo-deny >/dev/null 2>&1; then
  echo "==> cargo-deny"
  cargo deny check
else
  echo "(info) cargo-deny not found; skipping dependency audit"
fi

echo "OK"
