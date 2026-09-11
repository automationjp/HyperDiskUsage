# HyperDU

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

> **何がディスクを埋めているかを、速く見つける。**  
> Rust 製の高速・クロスプラットフォームなディスク使用量アナライザー。CLI、GUI、GNU `du` 互換モード、AI エージェント向け MCP / Skill を提供します。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-black?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20Linux-tested-blue)](#platform-status)

[Setup](docs/setup.md) · [Performance](docs/performance.md) · [Benchmark plan](docs/benchmarks.md) · [Documentation](docs/README.md) · [Web site](https://automationjp.github.io/HyperDiskUsage/) · [English](https://automationjp.github.io/HyperDiskUsage/en/)

---

## Quick start

CLI引数の既定値・出力条件・対応OSは [CLIパラメータリファレンス](docs/cli-reference.md) を参照してください。`--time` 系引数は既定で有効な `time-format` featureが必要です。

> **0.5.0-beta.3 は公開準備中です。** このブランチでは `cargo install --locked --path hyperdu` で導入できます。下記の crates.io コマンドと新しい名前の配布物は、リリース公開後に利用できます。

```bash
cargo install hyperdu --version 0.5.0-beta.3
hyperdu . --top 20
```

クレート名も実行コマンドも **`hyperdu`**。CLI と MCP サーバを一度に導入できます。

```bash
# カレントディレクトリを解析
hyperdu .

# 大きい項目を上位 20 件表示
hyperdu /path/to/data --top 20

# JSON へ出力
hyperdu /path/to/data --json result.json

# GNU du 互換モード
hyperdu --compat gnu -k /var/log
```

全オプションは `hyperdu --help` で確認できます。

## Performance

HyperDU の主題は **高速なディスク使用量解析**です。

単に `du` を Rust で書き直すのではなく、OS ごとの directory enumeration、metadata 取得、並列走査、physical-size accounting まで含めて hot path を最適化しています。

### HyperDU と du の比較

Linux と Windows の実測結果を、各環境の条件とともに掲載します。成功したディレクトリ行はすべて独立した oracle と一致しました。速度比は GNU `du` の時間 ÷ HyperDU の時間です。測定ソースは `2645689515ab2e608a78b7492637e55179e4739a`。 [GitHub Actions の実行](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) · [benchmarks.json](https://automationjp.github.io/HyperDiskUsage/benchmarks.json) · [サイトの性能結果](https://automationjp.github.io/HyperDiskUsage/#performance) · [Details](docs/benchmarks.md)

#### Linux — 構成ごとに100万ファイル

GitHub-hosted Ubuntu 24.04 / ext4、AMD EPYC 9V74（4 vCPU・15.6 GiB RAM）で、各構成に256 Bの通常ファイルを1,000,000個作成しました。warm cache、ディスク上の割当バイト、各ツール8回の交互実行の中央値です。Linux の見出しは **最大3.25倍高速**（wideの実測値は3.256倍）であり、Linuxに限る表現です。

| 構成 | HyperDU | GNU `du` | du / HyperDU |
|---|---:|---:|---:|
| flat | 2327.49 ms | 2763.52 ms | 1.187倍 |
| wide | 745.96 ms | 2428.80 ms | 3.256倍 |
| deep | 764.67 ms | 2434.28 ms | 3.183倍 |

#### Windows — 100万ファイルと1万ファイル診断

Windows 11 / NTFS / NVMe、AMD Ryzen 9 3900X（12 cores / 24 threads・128 GiB RAM）、warm cache、apparent-size の論理バイトで測定しました。100万ファイルの flat は HyperDU 単独を8回測定した中央値が589.91 msです。GNU `du` 8.32（Git for Windows / MSYS）はwarmupで600秒に達して測定前にタイムアウトしました。受理済みの100万ファイルGNU `du`基準値・速度比はなく、wide/deepの100万ファイルは未実行です。

1万ファイルの比較診断は各ツール2回の中央値です。以下の倍率はこの小規模診断に限り、100万ファイル全体の速度主張には使えません。

| 構成 | HyperDU | GNU `du` 8.32 (MSYS) | du / HyperDU |
|---|---:|---:|---:|
| flat | 32.98 ms | 704.17 ms | 21.35倍 |
| wide | 27.46 ms | 712.33 ms | 25.94倍 |
| deep | 32.32 ms | 836.73 ms | 25.89倍 |

### Why it is fast

| Platform | Main path | Optimization |
|---|---|---|
| Linux | `getdents64` + `statx` | directory entry をまとめて取得し、metadata を効率よく並列集計 |
| Windows | `NtQueryDirectoryFile` / `FileIdFullDirectoryInformation` | name・size・allocation size・file ID を batch 取得 |
| macOS | `getattrlistbulk` | metadata を bulk 取得 |

加えて `hyperdu-core` は worker ごとの LIFO deque と work stealing を使い、directory tree の偏りに応じて work を再分配します。

Windows では `--mft` を指定し、NTFS volume root・権限などの条件を満たす場合に `$MFT` 直接読み取り経路も利用できます。安全に解析できない場合は directory enumeration へ fallback します。

高速化の設計詳細は [Performance design](docs/performance.md)、component 間の関係は [Architecture](docs/architecture.md) を参照してください。

## What HyperDU provides

### Fast disk analysis

- logical size / physical allocation size の集計
- hardlink の重複排除
- multithread scan + work stealing
- filesystem ごとの scan strategy
- exclude / max depth / minimum file size
- JSON / CSV output
- basic / deep classification
- progress 表示

### GNU `du` compatibility mode

```bash
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -k /home --max-depth=2
hyperdu --compat gnu -b --time /usr/share
```

現状は POSIX `du` 完全互換ではありません。`--compat posix-strict` は既定の512-byte単位などを選択しますが、POSIX必須の `-a`、`-s`、`-H`、`-L` は未対応です。`du` の置換やエイリアスとして使用しないでください。

互換性は継続的にテストしていますが、GNU coreutils の全挙動を無条件に完全再現することを保証するものではありません。

### Structured output

```bash
hyperdu /srv/data --json usage.json
hyperdu /srv/data --csv usage.csv
```

## AI agents: MCP / Skill / Plugin

HyperDU は Claude や Codex などの AI エージェントからも利用できます。

| Interface | Purpose | MCP required? |
|---|---|---|
| MCP server | typed arguments / structured results | Yes |
| Agent Skill | `hyperdu` CLI を使った triage workflow | No |
| Agent Plugin | MCP + Skill をまとめて配布 | Optional |

MCP server は次の 3 ツールを公開します。

| Tool | Question it answers |
|---|---|
| `list_volumes` | どの volume が逼迫しているか |
| `scan_path` | その中で何が大きいか |
| `find_reclaimable` | 再生成可能な候補は何か |

**削除ツールは意図的に提供していません。** エージェントが人間の確認なしにデータを破壊する経路を作らないためです。

```bash
cargo install hyperdu --version 0.5.0-beta.3
claude mcp add --transport stdio hyperdu -- hyperdu mcp
# Codex:
codex mcp add hyperdu -- hyperdu mcp
```

Agent Skill / Plugin は [plugin/README.md](plugin/README.md) を参照してください。

## GUI

`hyperdu-gui` は `egui` / `eframe` ベースの desktop UI です。

- インタラクティブモード: 子フォルダの結果を順次表示し、走査中にも閲覧
- 一括モード: 全体走査後に結果を表示
- ツリー・パンくず・並べ替え可能な一覧でディレクトリを掘り下げ
- 除外・最小サイズ・深さ・リンク・スレッド数・I/O設定を画面から指定
- 進捗・エラー・キャンセル状態と論理/物理サイズを表示
- JSON / CSVへの結果出力

両モードの走査・集計は `hyperdu-core` が担当します。詳細は[GUIのREADME](hyperdu-gui/README.md)を参照してください。

```bash
cargo install hyperdu-gui --version 0.5.0-beta.3
hyperdu-gui
```

## Installation

**実行だけなら prebuilt binary / Scoop / `.deb` で Rust は不要です。** ソースから build する場合の Rust version、Windows MSVC / Windows SDK、Linux native build tools、GUI依存、MCP環境は [Setup and build environment](docs/setup.md) にまとめています。

### crates.io

```bash
# CLI + MCP (Rust 1.88+)
cargo install hyperdu --version 0.5.0-beta.3

# GUI (Rust 1.75+)
cargo install hyperdu-gui --version 0.5.0-beta.3

# MCP を使うときだけ起動
hyperdu mcp
```

### Prebuilt binaries

公開後は [Releases](https://github.com/automationjp/HyperDiskUsage/releases) から取得できます。以下は次期リリースの予定ファイル名です。

| Platform | CLI | GUI |
|---|---|---|
| Windows x86_64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.zip) / [exe](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.exe) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-windows-x86_64-generic.zip) |
| Linux x86_64 (glibc) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-generic.zip) |
| Linux x86_64 (musl) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-musl-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-musl-generic.zip) |
| Linux aarch64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-aarch64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-aarch64-generic.zip) |

Debian / Ubuntu 向け `.deb` も同じ release にあります。

### Scoop (Windows)

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

### winget (Windows) — 申請中

```powershell
winget install automationjp.HyperDU
```

manifest は `winget validate` 済みですが、winget-pkgs への登録完了までは利用できません。

### From source

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo install --path hyperdu
```

詳しい build environment は [docs/setup.md](docs/setup.md) を参照してください。

## Platform status

| Platform | Status | Notes |
|---|---|---|
| Windows | **Tested** | NTFS、CI、native enumeration、optional MFT path |
| Linux | **Tested** | XFS / ext4、CI |
| macOS | CLI: **未検証** / GUI: **ビルド不可** | `getattrlistbulk` implementationあり。release workflow は現在対象外 |

Minimum Rust versions:

- `hyperdu-core`, `hyperdu-gui`: **Rust 1.75+**
- `hyperdu` (CLI + MCP): **Rust 1.88+**
- workspace 全体の build/test: **Rust 1.88+**

## Experimental: persisted Linux snapshots

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

これは watcher ではありません。`show` は保存値を返し、freshness は常に `stale` と明示します。

詳細は [Linux directory snapshots](docs/index-snapshots.md) を参照してください。

## Documentation

- [Setup and build environment](docs/setup.md)
- [Documentation index](docs/README.md)
- [Performance design](docs/performance.md)
- [Benchmark plan / remeasurement checklist](docs/benchmarks.md)
- [Architecture](docs/architecture.md)
- [Linux persisted snapshots](docs/index-snapshots.md)
- [Historical / old documents](docs/old/README.md)
- [Agent Plugin / Skill](plugin/README.md)

## Development

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

workspace 全体の開発では Rust 1.88+ を使用します。platform-specific prerequisites と optional feature の確認方法は [Setup and build environment](docs/setup.md) を参照してください。

performance path を変更する PR では、通常の test に加えて [Benchmark plan](docs/benchmarks.md) の correctness gate と再計測条件を確認してください。

## Known limitations

- ベータ版です。CLI option、MCP tool schema、output format は変更される可能性があります。
- 公開用 benchmark は上記のLinux実測値とWindows実測値を掲載しています。OS、集計方法、ファイル数、試行回数が異なるため、Linuxの見出しやWindowsの診断倍率を未測定条件へ広げません。
- macOS の性能・互換性検証は完了していません。
- network filesystem や HDD では I/O latency が支配的になり、並列化による差が小さくなる場合があります。
- symbolic link は既定では追跡しません。`--follow-links` 利用時は cycle に注意してください。

## License

[MIT License](LICENSE)

## Acknowledgements

- [ripgrep](https://github.com/BurntSushi/ripgrep) — high-performance filesystem/search implementation
- [fd](https://github.com/sharkdp/fd) — parallel filesystem traversal
- [dust](https://github.com/bootandy/dust) — disk usage UX

---

**HyperDU — fast disk analysis for humans and agents.**

[POSIX du compatibility audit](docs/posix-compatibility.md)
