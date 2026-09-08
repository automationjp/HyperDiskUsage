# HyperDU

> **何がディスクを埋めているかを、すぐに。**  
> Rust 製の高速・クロスプラットフォームなディスク使用量アナライザー。CLI、GUI、GNU `du` 互換モード、AI エージェント向け MCP / Skill を提供します。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu-cli.svg)](https://crates.io/crates/hyperdu-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-black?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20Linux-tested-blue)](#platform-status)

[Web site](https://automationjp.github.io/HyperDiskUsage/) · [English](https://automationjp.github.io/HyperDiskUsage/en/) · [Benchmarks](docs/benchmarks.md) · [Agent Plugin](plugin/README.md)

---

## Quick start

CLI は crates.io から導入できます。

```bash
cargo install hyperdu-cli --version 0.5.0-beta.2
hyperdu . --top 20
```

クレート名は `hyperdu-cli` ですが、インストールされるコマンド名は **`hyperdu`** です。

よく使う例:

```bash
# カレントディレクトリを解析
hyperdu .

# 大きい項目を上位 20 件表示
hyperdu /path/to/data --top 20

# JSON へ出力
hyperdu /path/to/data --json result.json

# GNU du 互換モード
hyperdu --compat gnu -sh /var/log
```

全オプションは `hyperdu --help` で確認できます。

## Performance

HyperDU は、単に `du` を Rust で書き直したものではありません。OS ごとのディレクトリ列挙 API、並列走査、物理サイズ取得方法まで含めて走査経路を最適化しています。

同一マシン（Ryzen 9 3900X / NVMe SSD）で 2026-09-07 に測定した代表値です。

| Platform | Dataset | HyperDU | Comparison | Result |
|---|---:|---:|---:|---:|
| Windows | 101,368 files / 2.96 GB | **251 ms** | robocopy `/L /S`: 14,362 ms | **57×** |
| Windows | 101,368 files / 2.96 GB | **251 ms** | GNU du 8.32 (MSYS2): 12,871 ms | **51×** |
| Linux (WSL2/ext4) | 712,374 files | **191 ms** | du (uutils 0.8.0): 2,900 ms | **15×** |

ここでの倍率は「比較対象の実行時間 ÷ HyperDU」です。

走査量が一致していることを確認して測定しています。Windows の比較では HyperDU と robocopy のファイル数 101,368、バイト数 2,956,477,017 が一致しています。

ただし、**57× や 15× がすべての環境で出るわけではありません**。ツリー形状、ページキャッシュ、物理コア数、ファイルシステム、HDD/NVMe、ネットワークストレージなどで差は変わります。より差が小さい結果を含む測定条件・warm/cold benchmark・再現手順は [docs/benchmarks.md](docs/benchmarks.md) にまとめています。

### Why it is fast

| Platform | Main path | Optimization |
|---|---|---|
| Linux | `getdents64` + `statx` | ディレクトリエントリをまとめて取得し、並列に metadata を集計 |
| Windows | `NtQueryDirectoryFile` / `FileIdFullDirectoryInformation` | 名前・サイズ・allocation size・file ID をバッチで取得 |
| macOS | `getattrlistbulk` | metadata をバルク取得 |

加えて、コアスキャナではワーカーごとの LIFO deque と work stealing を使って動的に負荷分散します。巨大ディレクトリの処理継続ジョブも優先キューへ回し、ワーカーが一時的な queue empty を終了と誤認しないよう in-flight job を追跡します。

Windows では `--mft` を指定し、管理者権限で NTFS volume root を走査できる場合、`$MFT` 直接読み取り経路も利用できます。必要な DATA extent を安全に解決できない場合は directory enumeration へ fallback します。

## What HyperDU provides

### Fast disk analysis

- 論理サイズ / 物理 allocation size の集計
- hardlink の重複排除
- マルチスレッド走査
- filesystem ごとの自動戦略
- 除外パターン、深さ、最小ファイルサイズ指定
- JSON / CSV 出力
- basic / deep classification
- progress / runtime tuning

### GNU `du` compatibility mode

既存スクリプトから移行しやすいよう、GNU `du` 互換モードを提供しています。

```bash
hyperdu --compat gnu -sh /var/log
hyperdu --compat gnu -ak /home --max-depth=2
hyperdu --compat gnu -b --time /usr/share
```

必要であれば alias で段階的に置き換えられます。

```bash
alias du='hyperdu --compat gnu'
```

互換性や出力差分は継続的にテストしていますが、GNU coreutils の全挙動を無条件に完全再現することを保証するものではありません。

### Structured output

人が読む CLI 出力だけでなく、後段処理しやすい JSON / CSV を生成できます。

```bash
hyperdu /srv/data --json usage.json
hyperdu /srv/data --csv usage.csv
```

分類結果も別レポートとして出力できます。詳細は `hyperdu --help` を参照してください。

## AI agents: MCP / Skill / Plugin

HyperDU は、人間が端末で使うだけでなく、Claude や Codex などの AI エージェントがディスク逼迫を調査できるように設計しています。

提供する面は独立しています。

| Interface | Purpose | MCP required? |
|---|---|---|
| MCP server | typed arguments / structured results | Yes |
| Agent Skill | `hyperdu` CLI を使った triage workflow | No |
| Agent Plugin | MCP + Skill をまとめて配布 | Optional |

MCP server は次の 3 ツールを公開します。

| Tool | Question it answers |
|---|---|
| `list_volumes` | どのドライブ / volume が逼迫しているか |
| `scan_path` | その中で何が大きいか |
| `find_reclaimable` | そのうち再生成可能な候補は何か |

**削除ツールは意図的に提供していません。** 誤った容量報告はやり直せますが、エージェントによる誤削除は取り返せないためです。

`find_reclaimable` も名前だけで `target/` などを削除候補と判定しません。たとえば Rust の `target/` と判断するには、周辺の `Cargo.toml` など再生成可能性を裏付ける context を確認します。

### MCP setup

MCP server は crates.io から導入できます。`rmcp` の要件により Rust 1.88 以上が必要です。

```bash
cargo install hyperdu-mcp --version 0.5.0-beta.2

claude mcp add --transport stdio hyperdu -- hyperdu-mcp
# Codex:
codex mcp add hyperdu -- hyperdu-mcp
```

Agent Skill / Plugin のセットアップは [plugin/README.md](plugin/README.md) を参照してください。

## GUI

`hyperdu-gui` は `egui` / `eframe` ベースのデスクトップ UI です。

主な機能:

- リアルタイムスキャン表示
- インタラクティブな tree view
- ディレクトリの drill-down
- files/s など throughput の表示
- 結果 export

```bash
cargo install hyperdu-gui --version 0.5.0-beta.2
hyperdu-gui
```

## Installation

### crates.io — recommended

現在利用できる正式な配布経路は crates.io です。0.5.0-beta.2 は CLI / Core / GUI / MCP の各 crate が公開済みです。

```bash
# CLI (Rust 1.75+)
cargo install hyperdu-cli --version 0.5.0-beta.2

# GUI (Rust 1.75+)
cargo install hyperdu-gui --version 0.5.0-beta.2

# MCP server (Rust 1.88+)
cargo install hyperdu-mcp --version 0.5.0-beta.2
```

プレリリースのため、現時点ではバージョンを明示するのが確実です。

### From source

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage

cargo install --path hyperdu-cli
cargo install --path hyperdu-gui
cargo install --path hyperdu-mcp
```

最高性能を確認したい場合は、ローカル CPU 向けに release build できます。

```bash
RUSTFLAGS="-C target-cpu=native" cargo build --release -p hyperdu-cli
```

### Other distribution channels — preparing

以下は **まだ利用できません**。README から存在しない download URL へ誘導しないため、公開前の release asset 直リンクは掲載していません。

- GitHub Releases: public release はまだありません
- winget: manifest submission 前
- scoop: bucket registration 前
- deb / rpm / prebuilt assets: release workflow で生成し、public release 後に利用可能になる予定です

現時点では上の crates.io 経路を利用してください。

## Platform status

| Platform | Status | Notes |
|---|---|---|
| Windows | **Tested** | NTFS、CI、native directory enumeration、optional MFT path |
| Linux | **Tested** | Amazon Linux 2023 / XFS、WSL2 / ext4、CI |
| macOS | Build supported / **not hardware-tested** | `getattrlistbulk` implementationあり、実機検証は未完了 |

Minimum Rust versions:

- `hyperdu-core`, `hyperdu-cli`, `hyperdu-gui`: **Rust 1.75+**
- `hyperdu-mcp`: **Rust 1.88+** (`rmcp` requirement)

### Filesystem notes

Linux では filesystem に応じて strategy を調整します。たとえば ext4/XFS/ZFS、Btrfs、DrvFS、NFS/SMB/SSHFS/9p/FUSE では physical size や prefetch、buffer size、推奨 thread count の扱いが異なります。

Windows の標準経路は `NtQueryDirectoryFile` です。`HYPERDU_WIN_USE_NTQUERY=0` で `FindFirstFileExW` fallback に切り替えられます。`\\?\` path prefix を使うため、long path も扱います。

## Experimental: persisted Linux snapshots

Linux では、明示的に更新する directory snapshot を実験的に提供しています。

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

これは watcher ではありません。`show` は tree を再走査せず保存値を返し、freshness は常に `stale` と明示します。

DB 配置制約、root identity、failure semantics、計算量、watcher との境界は [docs/index-snapshots.md](docs/index-snapshots.md) に分離しました。

## Benchmarking

GNU `du` との比較は `scripts/bench/vs_du.sh` を使ってください。

```bash
# warm benchmark
scripts/bench/vs_du.sh /path/to/tree

# cold benchmark (drop_caches のため root/sudo が必要)
scripts/bench/vs_du.sh --cold /path/to/tree1 /path/to/tree2
```

この harness は、性能数値が不正に良く見える代表的な失敗を検出して停止します。

- 古い binary を測っている
- HyperDU と比較対象で走査ファイル集合が違う
- cold measurement を最小値だけで評価し、storage burst を拾っている

benchmark header には commit、dirty state、physical core count、filesystem、kernel などを残し、後から測定条件を復元できるようにしています。

詳細な結果と方法論は [docs/benchmarks.md](docs/benchmarks.md) を参照してください。

## Architecture

```text
HyperDiskUsage/
├── hyperdu-core/     # scanning engine / platform-specific fast paths
├── hyperdu-cli/      # `hyperdu` command
├── hyperdu-gui/      # desktop GUI
├── hyperdu-mcp/      # MCP server for AI agents
├── plugin/           # Agent Skill / Plugin
├── docs/             # benchmarks and design notes
├── site/             # bilingual project web site
└── scripts/          # benchmark, packaging, lint, development tools
```

コアと interface を分離しているため、CLI / GUI / MCP は同じ scanner semantics を共有します。

## Development

```bash
# fast compile check
cargo check --workspace

# tests
cargo test --workspace

# formatting
cargo fmt --check

# lint
cargo clippy --workspace --all-targets -- -D warnings
```

performance path を変更する PR では、通常の unit/integration test に加えて benchmark の比較条件と regression の有無も確認してください。

大きな変更は先に Issue で設計・受入条件を合わせることを推奨します。GUI変更では screenshot/GIF、scanner変更では risk / rollback と benchmark evidence があるとレビューしやすくなります。

## Documentation

- [Benchmark methodology and results](docs/benchmarks.md)
- [Linux persisted snapshots](docs/index-snapshots.md)
- [MFT parity verification](docs/design/mft-parity-verification.md)
- [Issue #16 / #41 implementation boundaries](docs/design/issue-16-41-implementation.md)
- [Agent Plugin / Skill](plugin/README.md)
- [Project web site (日本語)](https://automationjp.github.io/HyperDiskUsage/)
- [Project web site (English)](https://automationjp.github.io/HyperDiskUsage/en/)

## Known limitations

- ベータ版です。CLI option、MCP tool schema、出力形式は変更される可能性があります。
- macOS は build path はありますが、実機での性能・互換性検証はまだ完了していません。
- WSL の `/mnt/*` 上では Rust build 時の temporary directory cleanup が不安定になる場合があります。Linux filesystem 側へ checkout するか `CARGO_TARGET_DIR` を変更してください。
- symbolic link は既定では追跡しません。`--follow-links` を有効化する場合は cycle に注意してください。
- network filesystem や HDD では I/O latency が支配的になり、CPU 並列化による差は小さくなる場合があります。

## License

[MIT License](LICENSE)

## Acknowledgements

実装・設計の参考として、特に次のプロジェクトから多くを学んでいます。

- [ripgrep](https://github.com/BurntSushi/ripgrep) — 高速な filesystem / search implementation
- [fd](https://github.com/sharkdp/fd) — parallel filesystem traversal
- [dust](https://github.com/bootandy/dust) — disk usage UX

---

**HyperDU — fast disk analysis for humans and agents.**
