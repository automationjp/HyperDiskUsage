#!/bin/sh
# Install the HyperDU binaries this skill needs, and register the MCP server.
#
# This builds from source with `cargo`, so a Rust toolchain is required. That is
# worth stating plainly rather than leaving someone to discover it after a failed
# `apt install`.
#
# Prebuilt archives and published crates both exist -- the repository README
# lists them -- but this script installs two binaries on whichever platform it
# lands on, and choosing the right archive for each, then verifying it, is more
# than a setup script should decide on its owner's behalf.
#
# Registering an MCP server rewrites an agent's configuration, so this prints
# the command by default and only runs it when asked with --register. Installing
# a binary is reversible with `cargo uninstall`; silently editing someone's
# agent config is the kind of surprise that makes a setup script untrustworthy.
#
#   ./setup-hyperdu.sh              install what is missing, print how to register
#   ./setup-hyperdu.sh --check      report status, change nothing
#   ./setup-hyperdu.sh --register   install, then register with detected clients

set -eu

REPO_URL="https://github.com/automationjp/HyperDiskUsage"
# The crate is `hyperdu-cli`; the command it installs is `hyperdu`, declared
# by a [[bin]] section. That matches what the deb and rpm packages install.
CLI_BIN="hyperdu"
MCP_BIN="hyperdu-mcp"
MCP_NAME="hyperdu"

MODE="install"

for arg in "$@"; do
    case "$arg" in
        --check) MODE="check" ;;
        --register) MODE="register" ;;
        -h | --help)
            # The whole header block, however long it happens to be. The fixed
            # line range this replaces dropped the three usage lines silently
            # the moment the comment above it grew by a paragraph.
            awk 'NR > 1 { if (!/^#/) exit; sub(/^# ?/, ""); print }' "$0"
            exit 0
            ;;
        *)
            echo "unknown option: $arg" >&2
            echo "try --help" >&2
            exit 2
            ;;
    esac
done

have() { command -v "$1" >/dev/null 2>&1; }

# Resolve the repository root when this script is run from inside a checkout, so
# a contributor testing local changes installs those rather than whatever is on
# the default branch. Layout: <root>/plugin/skills/<skill>/scripts/<this file>.
repo_root() {
    script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
    candidate=$(CDPATH= cd -- "$script_dir/../../../.." 2>/dev/null && pwd) || return 1
    [ -f "$candidate/Cargo.toml" ] || return 1
    grep -q 'hyperdu-core' "$candidate/Cargo.toml" || return 1
    printf '%s' "$candidate"
}

report() {
    for bin in "$CLI_BIN" "$MCP_BIN"; do
        if have "$bin"; then
            echo "  present: $bin ($(command -v "$bin"))"
        else
            echo "  MISSING: $bin"
        fi
    done
}

echo "HyperDU setup"
echo
echo "Binaries:"
report
echo

if [ "$MODE" = "check" ]; then
    if have "$CLI_BIN" && have "$MCP_BIN"; then
        exit 0
    fi
    echo "Run this script without --check to install what is missing."
    exit 1
fi

missing=""
have "$CLI_BIN" || missing="$missing $CLI_BIN"
have "$MCP_BIN" || missing="$missing $MCP_BIN"

if [ -n "$missing" ]; then
    if ! have cargo; then
        cat >&2 <<EOF
error: cargo not found, and this script installs by building from source.

Install a Rust toolchain first:
  https://rustup.rs

Then run this script again.

Or install the two binaries yourself and re-run with --check: prebuilt archives
are attached to each release, and the crates are published.
  $REPO_URL/releases
EOF
        exit 1
    fi

    if root=$(repo_root); then
        echo "Installing from this checkout: $root"
        from_checkout=1
    else
        echo "Installing from $REPO_URL"
        from_checkout=0
    fi

    for bin in $missing; do
        echo
        if [ "$from_checkout" -eq 1 ]; then
            echo "==> cargo install --path $root/$bin"
            cargo install --path "$root/$bin"
        else
            echo "==> cargo install --git $REPO_URL $bin"
            cargo install --git "$REPO_URL" "$bin"
        fi
    done

    echo
    echo "Binaries after install:"
    report
    echo

    # cargo installs into ~/.cargo/bin, which a shell started before the
    # toolchain existed may not have on PATH. Without this note the next step
    # fails for a reason that looks like the install itself did not work.
    if ! have "$CLI_BIN" || ! have "$MCP_BIN"; then
        cat >&2 <<'EOF'
warning: cargo finished but the binaries are not on PATH.
They are in ~/.cargo/bin. Add it to PATH, for example:

  export PATH="$HOME/.cargo/bin:$PATH"

EOF
    fi
fi

# --- MCP registration -------------------------------------------------------
#
# Installing the server binary is not enough: a client only sees it once it has
# been registered.

echo "MCP server registration:"

found_client=0

register_with() {
    client=$1
    shift
    have "$client" || return 0
    found_client=1
    if [ "$MODE" = "register" ]; then
        echo "  ==> $*"
        if "$@"; then
            echo "  registered with $client"
        else
            echo "  WARNING: $client registration failed; run the command above by hand" >&2
        fi
    else
        echo "  $client detected. To register:"
        echo "    $*"
    fi
}

register_with claude claude mcp add --transport stdio "$MCP_NAME" -- "$MCP_BIN"
register_with codex codex mcp add "$MCP_NAME" -- "$MCP_BIN"

if [ "$found_client" -eq 0 ]; then
    cat <<EOF
  No claude or codex CLI found on PATH.

  For any other MCP client, add a stdio server that runs:
    $MCP_BIN

  The bundled plugin/mcp.json already declares exactly that.
EOF
elif [ "$MODE" != "register" ]; then
    echo
    echo "Re-run with --register to have this script issue those commands."
fi

echo
echo "Done."
