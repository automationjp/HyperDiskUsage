# HyperDU

> **何がディスクを埋めているかを、速く見つける。**  
> Rust 製の高速・クロスプラットフォームなディスク使用量アナライザー。CLI、GUI、GNU `du` 互換モード、AI エージェント向け MCP / Skill を提供します。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu-cli.svg)](https://crates.io/crates/hyperdu-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-black?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20Linux-tested-blue)](#platform-status)

[Setup](docs/setup.md) · [Performance](docs/performance.md) · [Benchmark plan](docs/benchmarks.md) · [Documentation](docs/README.md) · [Web site](https://automationjp.github.io/HyperDiskUsage/) · [English](https://automationjp.github.io/HyperDiskUsage/en/)

---

## Quick start

```bash
cargo install hyperdu-cli --version 0.5.0-beta.2
hyperdu . --top 20
```

クレート名は `hyperdu-cli` ですが、インストールされるコマンド名は **`hyperdu`** です。

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

HyperDU の主題は **高速なディスク使用量解析**です。

単に `du` を Rust で書き直すのではなく、OS ごとの directory enumeration、metadata 取得、並列走査、physical-size accounting まで含めて hot path を最適化しています。

### Benchmark status: remeasurement required

公開用の性能値は現在再計測中です。過去の benchmark は履歴として `docs/old/` に移しました。

新しい測定が完了するまで、倍率を断定しません。

| Scenario | Dataset | HyperDU | Baseline | Ratio |
|---|---|---:|---:|---:|
| Windows / NTFS | TBD | TBD | TBD | TBD |
| Linux / ext4 | TBD | TBD | TBD | TBD |
| Linux / XFS | TBD | TBD | TBD | TBD |

再計測で必要な環境、correctness parity、warm/cold、raw result、公開条件は [Benchmark plan](docs/benchmarks.md) にまとめています。

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
- progress / runtime tuning

### GNU `du` compatibility mode

```bash
hyperdu --compat gnu -sh /var/log
hyperdu --compat gnu -ak /home --max-depth=2
hyperdu --compat gnu -b --time /usr/share
```

```bash
alias du='hyperdu --compat gnu'
```

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
cargo install hyperdu-mcp --version 0.5.0-beta.2
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
# Codex:
codex mcp add hyperdu -- hyperdu-mcp
```

Agent Skill / Plugin は [plugin/README.md](plugin/README.md) を参照してください。

## GUI

`hyperdu-gui` は `egui` / `eframe` ベースの desktop UI です。

- realtime scan
- interactive tree view
- directory drill-down
- throughput 表示
- result export

```bash
cargo install hyperdu-gui --version 0.5.0-beta.2
hyperdu-gui
```

## Installation

**実行だけなら prebuilt binary / Scoop / `.deb` で Rust は不要です。** ソースから build する場合の Rust version、Windows MSVC / Windows SDK、Linux native build tools、GUI依存、MCP環境は [Setup and build environment](docs/setup.md) にまとめています。

### crates.io

```bash
# CLI (Rust 1.75+)
cargo install hyperdu-cli --version 0.5.0-beta.2

# GUI (Rust 1.75+)
cargo install hyperdu-gui --version 0.5.0-beta.2

# MCP server (Rust 1.88+)
cargo install hyperdu-mcp --version 0.5.0-beta.2
```

### Prebuilt binaries

[v0.5.0-beta.2](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.2) から取得できます。

| Platform | CLI | GUI |
|---|---|---|
| Windows x86_64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-cli-windows-x86_64-generic.zip) / [exe](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-cli-windows-x86_64-generic.exe) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-gui-windows-x86_64-generic.zip) |
| Linux x86_64 (glibc) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-cli-linux-x86_64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-gui-linux-x86_64-generic.zip) |
| Linux x86_64 (musl) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-cli-linux-x86_64-musl-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-gui-linux-x86_64-musl-generic.zip) |
| Linux aarch64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-cli-linux-aarch64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.2/hyperdu-gui-linux-aarch64-generic.zip) |

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
cargo install --path hyperdu-cli
```

詳しい build environment は [docs/setup.md](docs/setup.md) を参照してください。

## Platform status

| Platform | Status | Notes |
|---|---|---|
| Windows | **Tested** | NTFS、CI、native enumeration、optional MFT path |
| Linux | **Tested** | XFS / ext4、CI |
| macOS | CLI: **未検証** / GUI: **ビルド不可** | `getattrlistbulk` implementationあり。release workflow は現在対象外 |

Minimum Rust versions:

- `hyperdu-core`, `hyperdu-cli`, `hyperdu-gui`: **Rust 1.75+**
- `hyperdu-mcp`: **Rust 1.88+**
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
- 公開用 benchmark は再計測中です。性能値を更新する前に benchmark gate を通します。
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
