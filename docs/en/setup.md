# Setup and build environment

[日本語](../setup.md) · **English** · [简体中文](../zh-CN/setup.md)

The [README](../../README.en.md) covers published installation; the [developer guide (Japanese)](../developer-guide.md) explains the code. This page separates runtime requirements from source builds.

## 1. Running does not require Rust

Windows / Linux binaries and `.deb` packages from [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases), or Scoop installations, do not require a Rust toolchain. Windows binaries also do not require Visual Studio to run.

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

For Linux, select x86_64 glibc, x86_64 musl, or aarch64 glibc to match the environment. The GUI needs a Windows desktop session or Linux X11 / Wayland environment; use the CLI on headless servers. macOS is outside current release and CI targets, including GUI distribution.

## 2. Declared Rust versions are not verified build guarantees

| Crate | Current manifest `rust-version` | Caveat |
|---|---:|---|
| `hyperdu-core` | 1.75 | A declaration, not proof of the entire dependency graph's minimum |
| `hyperdu` (CLI + MCP) | 1.88 | Includes MCP; do not build the workspace below this version |
| `hyperdu-gui` | 1.75 | **Conflicts with dependencies and is not a support guarantee** |

The egui family used by the GUI declares Rust 1.85 in its [0.32.0 manifest](https://github.com/emilk/egui/blob/0.32.0/Cargo.toml). Consequently, Rust 1.75 cannot be advertised as a supported GUI compiler. This does not prove that 1.85 or 1.88 suffices for every resolved dependency. Reconciling minimum-version declarations with dedicated CI remains open.

**Use a recent stable Rust for development.** Current CI also tests stable.

```bash
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustc --version
cargo --version
```

## 3. Source-build prerequisites by OS

### Windows

Use the MSVC toolchain. Besides Rust, install Visual Studio / Build Tools with C++ build tools, the MSVC linker, and Windows SDK. The usual target is `x86_64-pc-windows-msvc`.

```powershell
rustup show
rustc -vV
```

Testing real volumes through MFT is separate from building. It needs an NTFS volume root, administrator privileges, and supported options; ineligible requests fall back to enumeration. A general build does not validate real-volume behavior.

### Linux

On Ubuntu / Debian, install a basic C build environment:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

GUI builds also need X11 / Wayland development libraries used by `egui` / `eframe` / `winit`. Package names vary by distribution; add the corresponding development packages when native-library errors occur.

Packaging and cross compilation are separate from ordinary CLI builds. The release workflow additionally uses tools such as `musl-tools`, `gcc-aarch64-linux-gnu`, `mingw-w64`, `zip`, `jq`, and `rpm`.

### macOS

The `getattrlistbulk` implementation exists, but is not tested by current CI. Native CLI performance and compatibility remain unverified; the GUI is not a release target.

## 4. Build, run, and install a development checkout

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo build --release --locked -p hyperdu
cargo test --locked -p hyperdu-core -p hyperdu
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

In Windows PowerShell, run `.\target\release\hyperdu.exe` instead.

```bash
cargo build --release --locked -p hyperdu-gui
cargo run --release -p hyperdu-gui
cargo install --locked --path hyperdu
```

`--path` installs the development checkout. For the published package, use the README's explicit `--version` command.

## 5. MCP

The `hyperdu` build includes MCP; no additional server crate is required.

```bash
hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

`hyperdu mcp` is a stdio protocol server, not an interactive command prompt. Verify startup and connection through the MCP client. [Plugin / Skill / MCP](../../plugin/README.en.md)

## 6. Development checks

```bash
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --locked --features hyperdu-core/prof-tracy
cargo check --workspace --locked --features hyperdu-core/prof-puffin
cargo test --workspace --locked --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

Tracy and Puffin cannot be enabled simultaneously; `--all-features` is not a valid combined test. For documentation or distribution changes, install PyYAML and also run:

```bash
python3 scripts/lint/test_docs.py
python3 scripts/package/test_supply_chain.py
python3 scripts/package/test_registry.py
```

## 7. Performance and installation checks

Use release builds for performance comparisons. Record conditions when optimizing for the local CPU:

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release --locked -p hyperdu
```

Do not treat native-CPU results as generic prebuilt-binary measurements. First pass the [benchmark correctness gate](benchmarks.md), and record OS, filesystem, Rust version, profile, CPU, storage, and command.

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
# Check help from a development checkout:
cargo run -p hyperdu -- --help
```

[Performance](performance.md) · [Benchmarks](benchmarks.md) · [Architecture](architecture.md) · [Documentation index](README.md)
