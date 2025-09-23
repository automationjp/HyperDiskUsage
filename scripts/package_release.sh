#!/usr/bin/env bash
set -euo pipefail

# Package current host binaries (CLI/GUI) + README.md into dist/*.zip
# - Tries to ensure required tools exist (jq, zip)
# - Cross targets are not enforced; this script packages host builds.

usage() {
  cat <<USAGE
Usage: $(basename "$0") \\
  [--targets "linux-gnu,linux-musl,linux-aarch64,windows-gnu,linux-deb,linux-rpm,linux-appimage,linux-snap,linux-flatpak,macos-dmg,homebrew,windows-msi,scoop,winget,linux-all,macos-all,windows-all,all"] \\
  [--cpu-flavors "generic,native"] [--skip-gui] [--verbose] [--deb] [--rpm] [--url-base URL]

Builds release binaries (CLI/GUI) for the current host and packages them into dist/*.zip
with README.md. When --targets is provided, also performs cross builds and packages
them accordingly. Host artifacts are also exported without the cpu-suffix for convenience.

Targets:
  linux-gnu        -> x86_64-unknown-linux-gnu
  linux-musl       -> x86_64-unknown-linux-musl (requires musl-tools)
  linux-aarch64    -> aarch64-unknown-linux-gnu (requires gcc-aarch64-linux-gnu)
  windows-gnu      -> x86_64-pc-windows-gnu (requires mingw-w64)
  windows-msvc     -> x86_64-pc-windows-msvc (unsupported on Linux; will warn)
  macos-x86_64     -> x86_64-apple-darwin (requires Apple SDK; will warn)
  macos-aarch64    -> aarch64-apple-darwin (requires Apple SDK; will warn)

Packaging pseudo-targets (host-dependent):
  linux-deb        -> Build .deb for host via cargo-deb
  linux-rpm        -> Build .rpm for host via cargo-generate-rpm
  linux-appimage   -> Build AppImage for GUI (linuxdeploy/appimagetool required)
  linux-snap       -> Build snap (snapcraft if available)
  linux-flatpak    -> Build flatpak bundle (flatpak-builder if available)
  macos-dmg        -> Build DMG (create-dmg, cargo-bundle)
  homebrew         -> Generate Homebrew formula template under dist/brew/
  windows-msi      -> Build MSI/EXE/ZIP via cargo-wix (Windows host only)
  scoop            -> Generate Scoop manifest under dist/scoop/
  winget           -> Generate winget manifest under dist/winget/

Aggregate targets:
  linux-all        -> linux-gnu,linux-musl,linux-aarch64,windows-gnu,linux-deb,linux-rpm,linux-appimage,linux-snap,linux-flatpak
  macos-all        -> macos-dmg,homebrew
  windows-all      -> windows-msi,scoop,winget
  all              -> linux-all,macos-all,windows-all

CPU flavors:
  generic         -> no special RUSTFLAGS (portable)
  native          -> RUSTFLAGS="-C target-cpu=native" (host-only)

Flags:
  --verbose        Enable verbose cargo logs (dist/build_*.log)
  --deb            Build .deb packages for host (cargo-deb)
  --rpm            Build .rpm packages for host (cargo-generate-rpm)
  --url-base URL   Base URL for release assets (for local manifest URL/SHA insertion)

Examples:
  bash scripts/package_release.sh --cpu-flavors "generic,native"
  bash scripts/package_release.sh --targets "linux-musl,windows-gnu" --skip-gui --verbose
USAGE
}

targets_csv=""
skip_gui=0
cpu_flavors_csv="generic"
verbose=0
build_deb=0
build_rpm=0
url_base=""
raw_only=0
no_zip=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --targets)
      targets_csv="$2"; shift 2 ;;
    --cpu-flavors)
      cpu_flavors_csv="$2"; shift 2 ;;
    --skip-gui)
      skip_gui=1; shift ;;
    --verbose)
      verbose=1; shift ;;
    --deb)
      build_deb=1; shift ;;
    --rpm)
      build_rpm=1; shift ;;
    --url-base)
      url_base="$2"; shift 2 ;;
    --raw-only)
      raw_only=1; shift ;;
    --no-zip)
      no_zip=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *) echo "unknown arg: $1"; usage; exit 1 ;;
  esac
done

root_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")"/.. && pwd)"
cd "$root_dir"

command -v cargo >/dev/null || { echo "error: cargo not found in PATH"; exit 1; }

ensure_tool() {
  local name="$1"
  if command -v "$name" >/dev/null 2>&1; then return 0; fi
  echo "==> Installing $name ..."
  if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update -y || true
    sudo apt-get install -y "$name" || true
  elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y "$name" || true
  elif command -v yum >/dev/null 2>&1; then
    sudo yum install -y "$name" || true
  elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -Sy --noconfirm "$name" || true
  elif command -v zypper >/dev/null 2>&1; then
    sudo zypper install -y "$name" || true
  elif command -v brew >/dev/null 2>&1; then
    # macOS: zip exists by default; install jq via brew
    if [[ "$name" == "jq" ]]; then brew install jq || true; fi
  else
    echo "warning: could not auto-install $name (unknown package manager)"
  fi
}

ensure_tool jq
ensure_tool zip
command -v jq >/dev/null || echo "warning: jq still missing; falling back to sed parser"
command -v zip >/dev/null || { echo "error: zip not found; please install zip"; exit 1; }

# Ensure AppImage tools (download to ~/.local/bin if missing)
ensure_appimage_tools() {
  local bindir="$HOME/.local/bin"
  mkdir -p "$bindir"
  if ! command -v linuxdeploy >/dev/null 2>&1; then
    echo "==> Installing linuxdeploy AppImage"
    curl -L -o "$bindir/linuxdeploy" https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage || true
    chmod +x "$bindir/linuxdeploy" || true
  fi
  if ! command -v appimagetool >/dev/null 2>&1; then
    echo "==> Installing appimagetool AppImage"
    curl -L -o "$bindir/appimagetool" https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage || true
    chmod +x "$bindir/appimagetool" || true
  fi
  export PATH="$bindir:$PATH"
}

# Ensure Snapcraft
ensure_snapcraft() {
  if command -v snapcraft >/dev/null 2>&1; then return 0; fi
  if command -v apt-get >/dev/null 2>&1; then
    echo "==> Installing snapcraft"
    sudo apt-get update -y || true
    sudo apt-get install -y snapcraft || true
  elif command -v snap >/dev/null 2>&1; then
    sudo snap install snapcraft --classic || true
  else
    echo "(info) snapcraft not found and cannot be auto-installed"
  fi
}

# Ensure Flatpak Builder
ensure_flatpak_builder() {
  if command -v flatpak-builder >/dev/null 2>&1; then return 0; fi
  if command -v apt-get >/dev/null 2>&1; then
    echo "==> Installing flatpak-builder"
    sudo apt-get update -y || true
    sudo apt-get install -y flatpak flatpak-builder || true
  else
    echo "(info) flatpak-builder not found and cannot be auto-installed"
  fi
}

# macOS helpers
ensure_brew() { command -v brew >/dev/null 2>&1; }
ensure_cargo_bundle() { cargo install cargo-bundle >/dev/null 2>&1 || true; }
ensure_create_dmg() { if ensure_brew; then brew install create-dmg || true; fi }

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os" in
  linux) os_tag=linux;;
  darwin) os_tag=macos;;
  msys*|mingw*|cygwin*) os_tag=windows;;
  *) os_tag=$os;;
esac

dist_dir="$root_dir/dist"
rm -rf "$dist_dir" && mkdir -p "$dist_dir"

# Use a safe target dir on WSL/Windows mounts to avoid permission errors removing temp archives.
ensure_safe_target_dir() {
  # Respect existing CARGO_TARGET_DIR if set by the user/CI
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then return 0; fi
  local fs_type
  fs_type=$(stat -f -c %T "$root_dir" 2>/dev/null || echo "")
  case "$fs_type" in
    drvfs|fuseblk|9p|9p2000*|v9fs)
      export CARGO_TARGET_DIR="/tmp/hyperdu-target"
      mkdir -p "$CARGO_TARGET_DIR"
      echo "(info) Using CARGO_TARGET_DIR=$CARGO_TARGET_DIR to avoid drvfs temp-file issues" ;;
  esac
}

ensure_safe_target_dir

# Expand aggregate targets if any
expand_targets() {
  local in="$1"
  local out=()
  IFS=',' read -r -a items <<< "$in"
  for t in "${items[@]}"; do
    case "$t" in
      linux-all)
        out+=(linux-gnu linux-musl linux-aarch64 windows-gnu linux-deb linux-rpm linux-appimage linux-snap linux-flatpak);;
      macos-all)
        out+=(macos-dmg homebrew);;
      windows-all)
        out+=(windows-msi scoop winget);;
      all)
        out+=(linux-gnu linux-musl linux-aarch64 windows-gnu linux-deb linux-rpm linux-appimage linux-snap linux-flatpak macos-dmg homebrew windows-msi scoop winget);;
      *) out+=($t);;
    esac
  done
  echo "${out[*]}"
}

build_and_capture() {
  local pkg="$1"
  local rustflags="${2:-}"
  local bin
  local envlog=( )
  if [[ $verbose -eq 1 ]]; then envlog+=("HYPERDU_LOG=1"); fi
  if [[ -n "$rustflags" ]]; then
    bin=$(env "${envlog[@]}" RUSTFLAGS="$rustflags" bash "$root_dir/scripts/build_print.sh" -p "$pkg" --release | tail -n1 || true)
  else
    bin=$(env "${envlog[@]}" bash "$root_dir/scripts/build_print.sh" -p "$pkg" --release | tail -n1 || true)
  fi
  if [[ -z "$bin" || ! -f "$bin" ]]; then
    echo "error: failed to build or locate binary for $pkg"
    return 1
  fi
  echo "$bin"
}

# Cross helpers
ensure_rust_target() {
  local triple="$1"
  rustup target list --installed | grep -q "^$triple$" || rustup target add "$triple"
}

ensure_cross_toolchain() {
  local triple="$1"
  case "$triple" in
    x86_64-unknown-linux-musl)
      command -v musl-gcc >/dev/null 2>&1 || ensure_tool musl-tools ;;
    x86_64-pc-windows-gnu)
      command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1 || ensure_tool mingw-w64 ;;
    aarch64-unknown-linux-gnu)
      command -v aarch64-linux-gnu-gcc >/dev/null 2>&1 || ensure_tool gcc-aarch64-linux-gnu || true ;;
    x86_64-pc-windows-msvc)
      echo "warning: $triple cross-linking not supported on Linux (MSVC toolchain)" ;;
    x86_64-apple-darwin|aarch64-apple-darwin)
      echo "warning: $triple requires Apple SDK/toolchain; skipping" ;;
  esac
}

triple_from_tag() {
  case "$1" in
    linux-gnu) echo x86_64-unknown-linux-gnu ;;
    linux-musl) echo x86_64-unknown-linux-musl ;;
    linux-aarch64) echo aarch64-unknown-linux-gnu ;;
    windows-gnu) echo x86_64-pc-windows-gnu ;;
    windows-msvc) echo x86_64-pc-windows-msvc ;;
    macos-x86_64) echo x86_64-apple-darwin ;;
    macos-aarch64) echo aarch64-apple-darwin ;;
    *) echo "" ;;
  esac
}

name_from_triple() {
  local t="$1"
  case "$t" in
    x86_64-unknown-linux-gnu) echo linux-x86_64 ;;
    x86_64-unknown-linux-musl) echo linux-x86_64-musl ;;
    aarch64-unknown-linux-gnu) echo linux-aarch64 ;;
    x86_64-pc-windows-gnu) echo windows-x86_64 ;;
    x86_64-pc-windows-msvc) echo windows-x86_64-msvc ;;
    x86_64-apple-darwin) echo macos-x86_64 ;;
    aarch64-apple-darwin) echo macos-aarch64 ;;
    *) echo unknown ;;
  esac
}

build_and_capture_cross() {
  local pkg="$1"; local triple="$2"
  local rustflags="${3:-}"
  local bin
  # Per-target linker hints to ensure cross-linking uses correct toolchain
  local env_cmd=( )
  if [[ $verbose -eq 1 ]]; then env_cmd+=("HYPERDU_LOG=1"); fi
  local cargo_args=( -p "$pkg" --release --target "$triple" )
  case "$triple" in
    aarch64-unknown-linux-gnu)
      env_cmd+=("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc") ;;
    x86_64-pc-windows-gnu)
      env_cmd+=("CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc") ;;
    x86_64-unknown-linux-musl)
      # musl-tools provides musl-gcc wrapper
      env_cmd+=("CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc") ;;
  esac
  # Reduce feature surface for harder cross targets (musl/aarch64): disable optional CLI features
  case "$triple" in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-gnu)
      cargo_args+=( --no-default-features ) ;;
  esac
  if [[ -n "$rustflags" ]]; then
    bin=$(env RUSTFLAGS="$rustflags" "${env_cmd[@]}" bash "$root_dir/scripts/build_print.sh" "${cargo_args[@]}" | tail -n1 || true)
  else
    bin=$(env "${env_cmd[@]}" bash "$root_dir/scripts/build_print.sh" "${cargo_args[@]}" | tail -n1 || true)
  fi
  if [[ -z "$bin" || ! -f "$bin" ]]; then
    echo "warn: failed to build or locate binary for $pkg ($triple)"
    return 1
  fi
  echo "$bin"
}

IFS=',' read -r -a cpu_flavors <<< "$cpu_flavors_csv"

# Initialize to avoid unbound variable errors when no host flavors are provided
cli_bin=""
gui_bin=""

for flavor in "${cpu_flavors[@]}"; do
  rustflags=""; suffix="generic"
  if [[ "$flavor" == "native" ]]; then rustflags="-C target-cpu=native"; suffix="native"; fi
  echo "==> Building hyperdu-cli (release) [$suffix]"
  cli_bin="$(build_and_capture hyperdu-cli "$rustflags")"
  echo "  cli: $cli_bin"
  # Also copy raw CLI binary to dist for direct run
  raw_cli_name="hyperdu-cli-${os_tag}-${arch}-${suffix}"
  if [[ "$os_tag" == "windows" ]]; then raw_cli_name+=".exe"; fi
  cp "$cli_bin" "$dist_dir/$raw_cli_name" && chmod +x "$dist_dir/$raw_cli_name" || true

  if [[ $skip_gui -eq 0 ]]; then
    echo "==> Building hyperdu-gui (release) [$suffix]"
    gui_bin="$(build_and_capture hyperdu-gui "$rustflags" || true)"
    if [[ -n "$gui_bin" && -f "$gui_bin" ]]; then
      echo "  gui: $gui_bin"
    else
      echo "  gui: not built (skipping GUI package)"
    fi
  fi

  # Normalize names
  cli_name="hyperdu-cli-${os_tag}-${arch}-${suffix}.zip"
  gui_name="hyperdu-gui-${os_tag}-${arch}-${suffix}.zip"

  if [[ $raw_only -eq 0 && $no_zip -eq 0 ]]; then
    echo "==> Packaging CLI -> $cli_name"
    tmpdir_cli="$(mktemp -d)"
    cp "$cli_bin" "$tmpdir_cli/"
    cp "$root_dir/README.md" "$tmpdir_cli/"
    (cd "$tmpdir_cli" && zip -9 -q "$dist_dir/$cli_name" "$(basename "$cli_bin")" README.md)
    rm -rf "$tmpdir_cli"
  fi
  # On Windows hosts, also drop a plain .exe for easy run
  if [[ "$os_tag" == "windows" ]]; then
    cp "$cli_bin" "$dist_dir/hyperdu-cli-${os_tag}-${arch}-${suffix}.exe"
  fi

  # Optional: host .deb / .rpm
  if [[ $build_deb -eq 1 ]]; then
    echo "==> Building .deb package (cargo-deb)"
    cargo install cargo-deb >/dev/null 2>&1 || true
    cargo deb -p hyperdu-cli --no-build --target-dir target >/dev/null 2>&1 || cargo deb -p hyperdu-cli
    deb_path=$(ls -1 target/debian/*hyperdu-cli*.deb 2>/dev/null | tail -n1 || true)
    if [[ -n "$deb_path" && -f "$deb_path" ]]; then
      cp "$deb_path" "$dist_dir/" || true
    fi
    if [[ $skip_gui -eq 0 ]]; then
      cargo deb -p hyperdu-gui --no-build --target-dir target >/dev/null 2>&1 || cargo deb -p hyperdu-gui
      deb_path_gui=$(ls -1 target/debian/*hyperdu-gui*.deb 2>/dev/null | tail -n1 || true)
      if [[ -n "$deb_path_gui" && -f "$deb_path_gui" ]]; then
        cp "$deb_path_gui" "$dist_dir/" || true
      fi
    fi
  fi

  if [[ $build_rpm -eq 1 ]]; then
    echo "==> Building .rpm package (cargo-generate-rpm)"
    cargo install cargo-generate-rpm >/dev/null 2>&1 || true
    # rpm tool is required for packaging
    if ! command -v rpmbuild >/dev/null 2>&1; then
      if command -v sudo >/dev/null 2>&1; then sudo apt-get update -y || true; sudo apt-get install -y rpm || true; fi
    fi
    cargo generate-rpm -p hyperdu-cli || true
    rpm_path=$(ls -1 target/generate-rpm/*hyperdu-cli*.rpm 2>/dev/null | tail -n1 || true)
    if [[ -n "$rpm_path" && -f "$rpm_path" ]]; then
      cp "$rpm_path" "$dist_dir/" || true
    fi
    if [[ $skip_gui -eq 0 ]]; then
      cargo generate-rpm -p hyperdu-gui || true
      rpm_path_gui=$(ls -1 target/generate-rpm/*hyperdu-gui*.rpm 2>/dev/null | tail -n1 || true)
      if [[ -n "$rpm_path_gui" && -f "$rpm_path_gui" ]]; then
        cp "$rpm_path_gui" "$dist_dir/" || true
      fi
    fi
  fi

  if [[ $skip_gui -eq 0 && -n "$gui_bin" && -f "$gui_bin" ]]; then
    echo "==> Packaging GUI -> $gui_name"
    tmpdir_gui="$(mktemp -d)"
    cp "$gui_bin" "$tmpdir_gui/"
    cp "$root_dir/README.md" "$tmpdir_gui/"
    (cd "$tmpdir_gui" && zip -9 -q "$dist_dir/$gui_name" "$(basename "$gui_bin")" README.md)
    rm -rf "$tmpdir_gui"
    if [[ "$os_tag" == "windows" ]]; then
      cp "$gui_bin" "$dist_dir/hyperdu-gui-${os_tag}-${arch}-${suffix}.exe"
    fi
  fi
done

# Normalize names
cli_name="hyperdu-cli-${os_tag}-${arch}.zip"
gui_name="hyperdu-gui-${os_tag}-${arch}.zip"

# Only package host artifacts without suffix if a host build actually ran
if [[ -n "$cli_bin" && -f "$cli_bin" ]]; then
  echo "==> Packaging CLI -> $cli_name"
  tmpdir_cli="$(mktemp -d)"
  cp "$cli_bin" "$tmpdir_cli/"
  cp "$root_dir/README.md" "$tmpdir_cli/"
  (cd "$tmpdir_cli" && zip -9 -q "$dist_dir/$cli_name" "$(basename "$cli_bin")" README.md)
  rm -rf "$tmpdir_cli"
  if [[ "$os_tag" == "windows" ]]; then
    cp "$cli_bin" "$dist_dir/hyperdu-cli-${os_tag}-${arch}.exe"
  fi
fi

if [[ $skip_gui -eq 0 && -n "$gui_bin" && -f "$gui_bin" ]]; then
  # Copy raw GUI binary
  raw_gui_name="hyperdu-gui-${os_tag}-${arch}-${suffix}"
  if [[ "$os_tag" == "windows" ]]; then raw_gui_name+=".exe"; fi
  cp "$gui_bin" "$dist_dir/$raw_gui_name" && chmod +x "$dist_dir/$raw_gui_name" || true
  if [[ $raw_only -eq 0 && $no_zip -eq 0 ]]; then
    echo "==> Packaging GUI -> $gui_name"
    tmpdir_gui="$(mktemp -d)"
    cp "$gui_bin" "$tmpdir_gui/"
    cp "$root_dir/README.md" "$tmpdir_gui/"
    (cd "$tmpdir_gui" && zip -9 -q "$dist_dir/$gui_name" "$(basename "$gui_bin")" README.md)
    rm -rf "$tmpdir_gui"
  fi
fi

echo "OK -> $dist_dir"

# Print installers summary for convenience
echo "==> Installers Summary"
for f in "$dist_dir"/hyperdu-cli-*; do
  [[ -f "$f" ]] && echo "  CLI: $(basename "$f")" || true
done
for f in "$dist_dir"/hyperdu-gui-*; do
  [[ -f "$f" ]] && echo "  GUI: $(basename "$f")" || true
done

# Cross packaging
if [[ -n "$targets_csv" ]]; then
  expanded="$(expand_targets "$targets_csv")"
  IFS=' ' read -r -a targets <<< "$expanded"
  for tag in "${targets[@]}"; do
    case "$tag" in
      linux-deb) build_deb=1 ;;
      linux-rpm) build_rpm=1 ;;
      linux-appimage)
        if [[ "$os_tag" == linux ]]; then
          ensure_appimage_tools
          bash "$root_dir/scripts/package_appimage.sh" >> "$dist_dir/appimage-pack.log" 2>&1 || echo "warn: AppImage step failed (see dist/appimage-pack.log)"
        else
          echo "warn: linux-appimage requested on non-linux host; skipping"
        fi ;;
      linux-snap)
        if [[ "$os_tag" == linux ]]; then
          ensure_snapcraft
          bash "$root_dir/scripts/package_snap.sh" >> "$dist_dir/snapcraft-pack.log" 2>&1 || echo "warn: snapcraft step failed (see dist/snapcraft-pack.log)"
        else
          echo "warn: linux-snap requested on non-linux host; skipping"
        fi ;;
      linux-flatpak)
        if [[ "$os_tag" == linux ]]; then
          ensure_flatpak_builder
          bash "$root_dir/scripts/package_flatpak.sh" >> "$dist_dir/flatpak-pack.log" 2>&1 || echo "warn: flatpak step failed (see dist/flatpak-pack.log)"
        else
          echo "warn: linux-flatpak requested on non-linux host; skipping"
        fi ;;
      macos-dmg)
        if [[ "$os_tag" == macos ]]; then
          ensure_cargo_bundle
          ensure_create_dmg
          bash "$root_dir/scripts/package_macos_dmg.sh" --release || true
        else
          echo "warn: macos-dmg requested on non-macos host; skipping"
        fi ;;
      homebrew)
        bash "$root_dir/scripts/package_brew.sh" || true ;;
      windows-msi)
        if [[ "$os_tag" == windows ]]; then pwsh -File "$root_dir/scripts/package_release.ps1" || true; else echo "warn: windows-msi requested on non-windows host; skipping"; fi ;;
      scoop)
        if [[ "$os_tag" == windows ]]; then
          ver=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root_dir/hyperdu-cli/Cargo.toml" | head -n1)
          pwsh -File "$root_dir/scripts/package_scoop.ps1" -Version "$ver" || true
        else
          echo "warn: scoop requested on non-windows host; skipping"
        fi ;;
      winget)
        if [[ "$os_tag" == windows ]]; then
          ver=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root_dir/hyperdu-cli/Cargo.toml" | head -n1)
          pwsh -File "$root_dir/scripts/package_winget.ps1" -Version "$ver" || true
        else
          echo "warn: winget requested on non-windows host; skipping"
        fi ;;
      *)
        triple="$(triple_from_tag "$tag")"
        if [[ -z "$triple" ]]; then echo "warn: unknown target tag: $tag"; continue; fi
        ensure_rust_target "$triple" || true
        ensure_cross_toolchain "$triple" || true
        case "$triple" in
          x86_64-apple-darwin|aarch64-apple-darwin|x86_64-pc-windows-msvc)
            echo "warn: skipping unsupported cross target on this host: $triple"; continue ;;
        esac

        echo "==> Cross-building ($triple) hyperdu-cli [generic]"
        cli_cross="$(build_and_capture_cross hyperdu-cli "$triple" "" || true)"
        if [[ -n "$cli_cross" && -f "$cli_cross" ]]; then
          base="$(name_from_triple "$triple")"
          # Raw cross cli binary
          raw_cli_cross="hyperdu-cli-${base}-generic"; [[ "$triple" == x86_64-pc-windows-gnu ]] && raw_cli_cross+=".exe"
          cp "$cli_cross" "$dist_dir/$raw_cli_cross" && chmod +x "$dist_dir/$raw_cli_cross" || true
          if [[ $raw_only -eq 0 && $no_zip -eq 0 ]]; then
            zipname="hyperdu-cli-${base}-generic.zip"
            td="$(mktemp -d)"; cp "$cli_cross" "$td/"
            cp "$root_dir/README.md" "$td/"
            (cd "$td" && zip -9 -q "$dist_dir/$zipname" "$(basename "$cli_cross")" README.md)
            rm -rf "$td"
          fi
        else
          echo "warn: CLI build failed for $triple (skipping)"
        fi

        if [[ $skip_gui -eq 0 ]]; then
          echo "==> Cross-building ($triple) hyperdu-gui [generic]"
          gui_cross="$(build_and_capture_cross hyperdu-gui "$triple" "" || true)"
          if [[ -n "$gui_cross" && -f "$gui_cross" ]]; then
            base="$(name_from_triple "$triple")"
            raw_gui_cross="hyperdu-gui-${base}-generic"; [[ "$triple" == x86_64-pc-windows-gnu ]] && raw_gui_cross+=".exe"
            cp "$gui_cross" "$dist_dir/$raw_gui_cross" && chmod +x "$dist_dir/$raw_gui_cross" || true
            if [[ $raw_only -eq 0 && $no_zip -eq 0 ]]; then
              zipname="hyperdu-gui-${base}-generic.zip"
              td="$(mktemp -d)"; cp "$gui_cross" "$td/"
              cp "$root_dir/README.md" "$td/"
              (cd "$td" && zip -9 -q "$dist_dir/$zipname" "$(basename "$gui_cross")" README.md)
              rm -rf "$td"
            fi
          else
            echo "warn: GUI build failed for $triple (skipping)"
          fi
        fi
        ;;
    esac
  done
fi

# Manifest URL/SHA insertion (local release scenario)
if [[ -n "$url_base" ]]; then
  echo "==> Updating manifests with URL/SHA256 (base: $url_base)"
  sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
    elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}';
    else echo ""; fi
  }
  # Homebrew formula (DMG)
  if [[ -f "$dist_dir/hyperdu-gui.dmg" && -f "$dist_dir/brew/hyperdu.rb" ]]; then
    dmg="$dist_dir/hyperdu-gui.dmg"
    sum=$(sha256_of "$dmg")
    url="$url_base/$(basename "$dmg")"
    sed -i.bak -e "s#__URL_TARBALL__#${url}#" -e "s#__SHA256__#${sum}#" "$dist_dir/brew/hyperdu.rb" && rm -f "$dist_dir/brew/hyperdu.rb.bak"
  fi
  # Scoop manifest (EXE/MSI)
  if [[ -f "$dist_dir/scoop/hyperdu.json" ]]; then
    exe=$(ls -1 "$dist_dir"/hyperdu-cli-*.exe 2>/dev/null | head -n1 || true)
    msi=$(ls -1 "$dist_dir"/hyperdu-cli-*.msi 2>/dev/null | head -n1 || true)
    asset="${exe:-$msi}"
    if [[ -n "$asset" && -f "$asset" ]]; then
      sum=$(sha256_of "$asset"); url="$url_base/$(basename "$asset")"
      sed -i.bak -e "s#__URL__#${url}#" -e "s#__SHA256__#${sum}#" "$dist_dir/scoop/hyperdu.json" && rm -f "$dist_dir/scoop/hyperdu.json.bak"
    fi
  fi
  # winget manifest
  if [[ -f "$dist_dir/winget/manifest.yaml" ]]; then
    exe=$(ls -1 "$dist_dir"/hyperdu-cli-*.exe 2>/dev/null | head -n1 || true)
    msi=$(ls -1 "$dist_dir"/hyperdu-cli-*.msi 2>/dev/null | head -n1 || true)
    asset="${exe:-$msi}"
    if [[ -n "$asset" && -f "$asset" ]]; then
      sum=$(sha256_of "$asset"); url="$url_base/$(basename "$asset")"
      sed -i.bak -e "s#__URL__#${url}#" -e "s#__SHA256__#${sum}#" "$dist_dir/winget/manifest.yaml" && rm -f "$dist_dir/winget/manifest.yaml.bak"
    fi
  fi
fi
