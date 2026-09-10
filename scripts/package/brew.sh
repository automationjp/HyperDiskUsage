#!/usr/bin/env bash
set -euo pipefail

# Generate a Homebrew formula template for hyperdu.
# The formula will be written under dist/brew/ with placeholders for URL/SHA256.

usage() {
  cat <<USAGE
Usage: $(basename "$0") [--version X.Y.Z]

Generates Homebrew formulae in dist/brew/ with placeholders. Fill in URL/SHA256
based on your release assets and push to your tap repository.
USAGE
}

VER=""
if [[ ${1:-} == "--version" ]]; then VER=${2:-}; fi
if [[ -z "$VER" ]]; then
  # The crate inherits its version from [workspace.package], so the literal is
  # not in hyperdu/Cargo.toml. pkgid prints `<url>#<version>`.
  VER=$(cargo pkgid -p hyperdu | sed 's/.*[#@]//')
fi
if [[ -z "$VER" ]]; then echo "error: could not determine hyperdu version" >&2; exit 1; fi

outdir="dist/brew"
mkdir -p "$outdir"

cat >"$outdir/hyperdu.rb" <<'RUBY'
class Hyperdu < Formula
  desc "Hyper-fast disk usage analyzer CLI"
  homepage "https://github.com/automationjp/HyperDiskUsage"
  url "__URL_TARBALL__"
  sha256 "__SHA256__"
  version "__VERSION__"

  def install
    bin.install "hyperdu"
    man1.install "hyperdu.1"
  end
end
RUBY

sed -i.bak -e "s/__VERSION__/$VER/" "$outdir/hyperdu.rb"
rm -f "$outdir/hyperdu.rb.bak"

echo "Wrote formula template: $outdir/hyperdu.rb"
