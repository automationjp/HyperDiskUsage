# Setup and build environment

[日本語](../setup.md) | [English](../en/setup.md) | [简体中文](../zh-CN/setup.md)

This document separates the requirements for **running HyperDU only** from those for compiling and developing it from Rust.

## 1. Running only

When using a prebuilt binary, Scoop, or a `.deb` package, **you do not need a Rust toolchain**.

### Windows

You can use the Windows x86_64 binary from a GitHub Release or install HyperDU with Scoop.

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

Downloading the binary directly also does not require Rust or Visual Studio.

### Linux

The distribution provides glibc, musl, and aarch64 builds. Choose the build that matches your environment.

- x86_64 + glibc: a candidate for ordinary Ubuntu / Debian / Fedora systems
- x86_64 + musl: when you want a static build
- aarch64 + glibc: ARM64 Linux

Rust is also unnecessary when using a `.deb` package.

### GUI runtime

`hyperdu-gui` requires a desktop graphics stack.

- Windows: a normal desktop session
- Linux: an X11 or Wayland desktop environment
- macOS: the GUI is currently not a release target

## 2. Building the CLI from Rust

### Rust version

The minimum Rust version for each crate is as follows.

| Crate | Minimum Rust |
|---|---:|
| `hyperdu-core` | 1.75+ |
| `hyperdu` (CLI + MCP) | 1.88+ |
| `hyperdu-gui` | 1.75+ |

**Building or testing the entire workspace at once requires Rust 1.88+ because it includes the CLI's MCP implementation.**

A stable toolchain is recommended for development.

```bash
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustc --version
cargo --version
```

### Clone

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
```

### CLI only

```bash
cargo build --release -p hyperdu
```

Run it:

```bash
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

Windows PowerShell:

```powershell
.\target\release\hyperdu.exe --version
.\target\release\hyperdu.exe . --top 20
```

## 3. Compilation environment by operating system

### Windows — recommended development path

The recommended choice is the **MSVC toolchain**.

You need:

1. Rust stable (`rustup`)
2. Visual Studio 2022 or Visual Studio Build Tools
3. C++ build tools / MSVC linker
4. Windows SDK

Check the Rust target:

```powershell
rustup show
rustc -vV
```

When you select an MSVC target in the usual rustup Windows installer, the target has the following form:

```text
x86_64-pc-windows-msvc
```

Build:

```powershell
cargo build --release -p hyperdu
cargo test -p hyperdu-core -p hyperdu
```

Validation of a real volume with `--mft` is separate from a normal build. It requires an NTFS volume root and administrator privileges. If those conditions are not met, HyperDU falls back to ordinary directory enumeration.

### Linux — CLI / core

In addition to Rust, you need a basic C build environment capable of compiling native dependencies.

Example for Ubuntu / Debian:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

Then:

```bash
cargo build --release -p hyperdu
cargo test -p hyperdu-core -p hyperdu
```

Release packaging and cross builds require additional toolchains. The CI release workflow also uses `musl-tools`, `gcc-aarch64-linux-gnu`, `mingw-w64`, `zip`, `jq`, `rpm`, and other tools depending on the use case.

### Linux — GUI

In addition to the CLI/core build environment, you need the **X11 or Wayland development libraries** used by `egui` / `eframe` / `winit`.

Package names vary by distribution. If a GUI build reports a native library error, add that distribution's X11 / Wayland development package.

```bash
cargo build --release -p hyperdu-gui
cargo run --release -p hyperdu-gui
```

The CLI is recommended over the GUI on headless servers.

### macOS

The CLI core has a `getattrlistbulk` fast path, but real-machine performance and compatibility of the macOS CLI have not been verified.

The GUI is not currently included in the release targets.

## 4. Building the MCP server

`hyperdu mcp` requires **Rust 1.88+** because of the `rmcp` requirement.

```bash
rustup toolchain install stable
cargo build --release -p hyperdu
cargo install --path hyperdu-cli
hyperdu mcp
```

`hyperdu mcp` is not a normal interactive CLI command. It is a **server that accepts connections from an MCP client over stdio**. When started directly, it waits for protocol input.

Example registrations:

```bash
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

To verify operation, confirm from the MCP client you use that the server can start and connect.

## 5. Checking the development environment

These are the basic checks when changing the workspace.

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Use Rust 1.88+ for the entire workspace.

### Optional feature configurations

The `profiling` backends Tracy and Puffin are not enabled at the same time. `--all-features` is not a valid verification method.

Check the required features separately:

```bash
cargo check --workspace --features hyperdu-core/prof-tracy
cargo check --workspace --features hyperdu-core/prof-puffin
```

To check the parallel configuration:

```bash
cargo test --workspace \
  --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

## 6. Performance build

Always use a release build for ordinary comparisons.

```bash
cargo build --release -p hyperdu
```

Use native optimization only for comparisons dedicated to the local CPU, and record the conditions.

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release -p hyperdu
```

Do not confuse results from `target-cpu=native` with performance values from a generic prebuilt binary.

Before publishing performance values, follow the environment-recording and correctness gate in the [Benchmark plan](benchmarks.md).

## 7. Installation check

After setup, perform at least these checks:

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
```

In a development checkout:

```bash
cargo run -p hyperdu -- --help
```

If a problem may involve the performance path, record the OS, filesystem, Rust version, build profile, CPU, storage, and execution command together.

## Related docs

- [Performance design](performance.md)
- [Benchmark plan](benchmarks.md)
- [Architecture](architecture.md)
- [Documentation index](README.md)
