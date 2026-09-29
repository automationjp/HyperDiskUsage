# セットアップとビルド環境

**日本語** · [English](en/setup.md) · [简体中文](zh-CN/setup.md)

[README](../README.md)は公開版の導入、[開発者ガイド](developer-guide.md)は実装の読み方を説明します。この文書では、実行だけの場合とソースビルドの前提を分けます。

## 1. 実行だけならRustは不要

[GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases)のWindows / Linux向けバイナリや `.deb`、Scoopを使う場合、Rust toolchainは不要です。Windowsでバイナリを実行するためにVisual Studioを入れる必要もありません。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
hyperdu --version
```

Linuxはx86_64 glibc、x86_64 musl、aarch64 glibcから環境に合う配布物を選びます。GUIにはWindowsのデスクトップセッション、またはLinuxのX11 / Wayland環境が必要です。headless serverではCLIを使います。macOSは現行Release・CIの対象ではなく、GUIも提供対象外です。

## 2. Rustの宣言値と、ビルドできる環境を区別する

| クレート | 現行ソースの最低Rust版 |
|---|---:|
| `hyperdu-core` | 1.82 |
| `hyperdu`（CLI + MCP） | 1.88 |
| `hyperdu-gui` | 1.85 |

Linux / Windowsの6条件で、各コンパイラを明示して `cargo check --locked --all-targets` を実行します。対象はコミット済みlockfileのdefault構成です。workspace全体はCLIを含むため1.88以上が必要です。公開済みベータ版の古いmanifestは変更されません。公開版の導入・通常開発には新しいstable Rustを使用してください。

```bash
rustup toolchain install stable
rustup default stable
rustup component add rustfmt clippy
rustc --version
cargo --version
```

## 3. OSごとのソースビルド準備

### Windows

MSVC toolchainを推奨します。Rustに加えてVisual Studio / Build ToolsのC++ビルドツール、MSVC linker、Windows SDKが必要です。通常の対象は `x86_64-pc-windows-msvc` です。

```powershell
rustup show
rustc -vV
```

MFTの実ボリューム検証は通常のビルドとは別です。NTFSボリュームルート、管理者権限、対応オプションが必要で、条件を満たさない場合は通常列挙に戻ります。実ボリュームのテストを一般のビルド成功と混同しないでください。

### Linux

Ubuntu / Debianでは基本のCビルド環境を準備します。

```bash
sudo apt-get update
sudo apt-get install -y build-essential pkg-config
```

GUIは `egui` / `eframe` / `winit` が使うX11 / Waylandの開発ライブラリも必要です。パッケージ名はディストリビューションによって異なり、native library errorにはその環境の開発パッケージを追加します。

配布パッケージやcross buildは通常のCLIビルドとは別で、release workflowでは `musl-tools`、`gcc-aarch64-linux-gnu`、`mingw-w64`、`zip`、`jq`、`rpm` なども使います。

### macOS

`getattrlistbulk` の実装はありますが、現行CIでは検証していません。CLIの実機性能・互換性は未検証で、GUIはRelease対象外です。

## 4. ビルド・起動・開発版のインストール

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo build --release --locked -p hyperdu
cargo test --locked -p hyperdu-core -p hyperdu
./target/release/hyperdu --version
./target/release/hyperdu . --top 20
```

Windows PowerShellでは実行ファイルを `.\target\release\hyperdu.exe` として起動します。

```bash
cargo build --release --locked -p hyperdu-gui
cargo run --release -p hyperdu-gui
cargo install --locked --path hyperdu
```

`--path` はcheckoutからの開発版のインストールです。公開済みCargo版はREADMEの `--version` 指定を使います。

## 5. MCP

`hyperdu` のビルドにMCPは含まれます。追加のサーバークレートは不要です。

```bash
hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

`hyperdu mcp` は対話型のコマンドプロンプトではなく、stdioでMCPクライアントのprotocol入力を待つサーバです。接続確認はクライアントから行ってください。[Plugin / Skill / MCP](../plugin/README.md)

## 6. 開発時の検証

```bash
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo check --workspace --locked --features hyperdu-core/prof-tracy
cargo check --workspace --locked --features hyperdu-core/prof-puffin
cargo test --workspace --locked --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
```

TracyとPuffinは同時に有効化できません。`--all-features` は正しい一括検証方法ではありません。ドキュメントや配布を変更した場合はPyYAMLを導入し、次も確認します。

```bash
python3 scripts/lint/test_docs.py
python3 scripts/package/test_supply_chain.py
python3 scripts/package/test_registry.py
```

## 7. 性能とインストール結果の確認

性能比較はrelease buildで行います。ローカルCPU専用の最適化を使う場合は条件を記録します。

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release --locked -p hyperdu
```

この結果をgeneric配布バイナリの性能と混同しないでください。測定前に[ベンチマークの正確性ゲート](benchmarks.md)を通し、OS、ファイルシステム、Rust版、profile、CPU、storage、実行コマンドを記録します。

```bash
hyperdu --version
hyperdu --help
hyperdu . --top 10
# 開発checkoutからhelpを確認
cargo run -p hyperdu -- --help
```

[性能設計](performance.md) · [ベンチマーク](benchmarks.md) · [アーキテクチャ](architecture.md) · [文書一覧](README.md)
