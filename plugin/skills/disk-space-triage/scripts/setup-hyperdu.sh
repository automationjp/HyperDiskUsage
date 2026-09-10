#!/bin/sh
# Install unified HyperDU (CLI + MCP). --check is read-only; --register opts in
# to changing detected MCP client configuration. Requires Rust 1.88+ to build.
set -eu
# Audited source revision; update together with the PowerShell installer.
source_rev="5f36fca6f955c727e4ffa2dbd7d4784568f2690c"
mode=install
for arg in "$@"; do
    case "$arg" in
        --check) mode=check ;;
        --register) mode=register ;;
        -h|--help) echo 'Usage: setup-hyperdu.sh [--check|--register]'; exit 0 ;;
        *) echo "Unknown option: $arg" >&2; exit 2 ;;
    esac
done
have_unified() {
    command -v hyperdu >/dev/null 2>&1 || return 1
    help_text=$(hyperdu mcp --help 2>/dev/null) || return 1
    printf '%s\n' "$help_text" | grep -Eq 'Usage: hyperdu(\.exe)? mcp'
}
if [ "$mode" = check ]; then
    if have_unified; then echo 'Ready: hyperdu (CLI + MCP)'; exit 0; fi
    echo 'Missing unified hyperdu. Run this script without --check to install.'
    exit 1
fi
if ! have_unified; then
    command -v cargo >/dev/null 2>&1 || { echo 'Install Rust 1.88+ from https://rustup.rs' >&2; exit 1; }
    script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    source_root=$(CDPATH= cd -- "$script_dir/../../../.." && pwd)
    if [ -f "$source_root/hyperdu/Cargo.toml" ]; then
        cargo install --locked --force --path "$source_root/hyperdu"
    else
        cargo install --locked --force --git https://github.com/automationjp/HyperDiskUsage --rev "$source_rev" hyperdu
    fi
    PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    export PATH
    have_unified || { echo 'Installed command unavailable or missing MCP; check PATH.' >&2; exit 1; }
fi
echo 'Ready: hyperdu (CLI + MCP)'
echo 'claude mcp add --transport stdio hyperdu -- hyperdu mcp'
echo 'codex mcp add hyperdu -- hyperdu mcp'
if [ "$mode" = register ]; then
    if command -v claude >/dev/null 2>&1; then claude mcp add --transport stdio hyperdu -- hyperdu mcp; fi
    if command -v codex >/dev/null 2>&1; then codex mcp add hyperdu -- hyperdu mcp; fi
fi
echo 'Other MCP clients: command="hyperdu", args=["mcp"].'
