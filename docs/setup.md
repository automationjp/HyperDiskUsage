# Setup and build environment

この文書では、HyperDU を**実行するだけの場合**と、**Rust からコンパイル・開発する場合**を分けて説明します。

## 1. 実行するだけの場合

prebuilt binary / Scoop / `.deb` を利用する場合、**Rust toolchain は不要です**。

### Windows

GitHub Release の Windows x86_64 binary、または Scoop を利用できます。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

直接 binary を取得する場合も、Rust や Visual Studio のインストールは不要です。

### Linux

配布物には glibc / musl / aarch64 の build があります。使用している環境に合うものを選択してください。

- x86_64 + glibc: 通常の Ubuntu / Debian / Fedora 系で利用する候補
- x86_64 + musl: static build を使いたい場合
- aarch64 + glibc: ARM64 Linux

`.deb` を使う場合も Rust は不要です。

### GUI runtime

`hyperdu-gui` は desktop graphics stack を必要とします。

- Windows: 通常の desktop session
- Linux: X11 または Wayland desktop environment
- macOS: 現在 GUI は release 対象外

## 2. Rust から CLI をビルドする

### Rust version

crate ごとの minimum Rust version は次のとおりです。

| Crate | Minimum Rust |
|---|---:|
| `hyperdu-core` | 1.75+ |
| `hyperdu-cli` | 1.75+ |
| `hyperdu-gui` | 1.75+ |
| `hyperdu-mcp` | 1.88+ |

**workspace 全体を一度に build/test する場合は `hyperdu-mcp` を含むため Rust 1.88+ が必要です。**

開発環境では stable toolchain を推奨します。

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
cargo build --release -p hyperdu-cli
```

実行:

```bash
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

Windows PowerShell:

```powershell
.\target\release\hyperdu.exe --version
.\target\release\hyperdu.exe . --top 20
```

## 3. OS ごとのコンパイル環境

### Windows — recommended development path

推奨は **MSVC toolchain** です。

必要なもの:

1. Rust stable (`rustup`)
2. Visual Studio 2022 または Visual Studio Build Tools
3. C++ build tools / MSVC linker
4. Windows SDK

Rust target を確認します。

```powershell
rustup show
rustc -vV
```

通常の rustup Windows installer で MSVC target を選択した場合、target は次の形になります。

```text
x86_64-pc-windows-msvc
```

build:

```powershell
cargo build --release -p hyperdu-cli
cargo test -p hyperdu-core -p hyperdu-cli
```

`--mft` の実 volume 検証は通常 build とは別です。NTFS volume root と管理者権限が必要で、条件を満たさない場合 HyperDU は通常の directory enumeration に fallback します。

### Linux — CLI / core

Rust に加えて、native dependency をコンパイルできる基本的な C build environment が必要です。

Ubuntu / Debian 系の例:

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

その後:

```bash
cargo build --release -p hyperdu-cli
cargo test -p hyperdu-core -p hyperdu-cli
```

release packaging や cross build を行う場合は追加 toolchain が必要です。CI の release workflow では、用途に応じて `musl-tools`、`gcc-aarch64-linux-gnu`、`mingw-w64`、`zip`、`jq`、`rpm` なども使用しています。

### Linux — GUI

CLI/core の build environment に加えて、`egui` / `eframe` / `winit` が利用する **X11 または Wayland の development libraries** が必要です。

ディストリビューションによって package 名が異なるため、GUI build で native library error が出た場合は、その distribution の X11 / Wayland development package を追加してください。

```bash
cargo build --release -p hyperdu-gui
cargo run --release -p hyperdu-gui
```

headless server では GUI より CLI を推奨します。

### macOS

CLI core には `getattrlistbulk` fast path がありますが、現在 macOS CLI は実機 performance / compatibility 未検証です。

GUI は現在 release target に含めていません。

## 4. MCP server をビルドする

`hyperdu-mcp` は `rmcp` の要件により **Rust 1.88+** が必要です。

```bash
rustup toolchain install stable
cargo build --release -p hyperdu-mcp
cargo install --path hyperdu-mcp
```

`hyperdu-mcp` は通常の対話型 CLI ではなく、**stdio 上で MCP client から接続される server** です。直接起動すると protocol input を待機します。

登録例:

```bash
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
codex mcp add hyperdu -- hyperdu-mcp
```

動作確認は利用する MCP client 側から server が起動・接続できることを確認してください。

## 5. 開発環境の確認

workspace を変更する場合の基本確認です。

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

workspace 全体では Rust 1.88+ を使用してください。

### Optional feature configurations

`profiling` backend は Tracy と Puffin を同時には有効化しません。`--all-features` は有効な検証方法ではありません。

必要な feature は個別に確認します。

```bash
cargo check --workspace --features hyperdu-core/prof-tracy
cargo check --workspace --features hyperdu-core/prof-puffin
```

parallel configuration を確認する場合:

```bash
cargo test --workspace \
  --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

## 6. Performance build

通常の比較では、必ず release build を使います。

```bash
cargo build --release -p hyperdu-cli
```

ローカル CPU 専用の比較を行う場合のみ、条件を記録したうえで native optimization を使用します。

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release -p hyperdu-cli
```

`target-cpu=native` の結果を generic prebuilt binary の性能値と混同しないでください。

性能値を公開する場合は [Benchmark plan](benchmarks.md) の環境記録・correctness gate を通します。

## 7. Installation check

セットアップ後は最低限次を確認します。

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
```

開発 checkout では:

```bash
cargo run -p hyperdu-cli -- --help
```

問題が performance path に関係する場合は、OS、filesystem、Rust version、build profile、CPU、storage、実行 command をセットで記録してください。

## Related docs

- [Performance design](performance.md)
- [Benchmark plan](benchmarks.md)
- [Architecture](architecture.md)
- [Documentation index](README.md)
