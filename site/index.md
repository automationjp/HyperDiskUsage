---
layout: default
lang: ja
home_path: /
lang_switch_label: 言語
title: "HyperDU — 高速なディスク使用量解析"
description: "OS 固有の高速経路と並列走査でディスク使用量を解析する Rust 製ツール。性能値は再計測中です。"
---

# 高速なディスク使用量解析

<p class="lede">
HyperDU は、<strong>何がディスクを埋めているかを速く見つける</strong>ための Rust 製ディスク使用量アナライザーです。
</p>

```bash
cargo install hyperdu-cli --version {{ site.versions.cli }}
hyperdu /path --top 20
```

## Performance

HyperDU は、単に `du` を Rust で書き直したものではありません。OS 固有の directory enumeration、metadata 取得、並列走査まで含めて hot path を最適化しています。

### 公開用 benchmark は再計測中です

過去の数値は履歴として保存し、現在の性能主張には使用しません。新しい測定が完了するまで結果欄は `TBD` とします。

| Scenario | HyperDU | Baseline | Ratio |
|---|---:|---:|---:|
| Windows / NTFS | TBD | TBD | TBD |
| Linux / ext4 | TBD | TBD | TBD |
| Linux / XFS | TBD | TBD | TBD |

再計測条件と作業項目は [Benchmark plan](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md) を参照してください。

## 速い理由

- **Linux** — `getdents64` + `statx` で低オーバーヘッドな列挙と metadata 取得
- **Windows** — `NtQueryDirectoryFile` で name・size・allocation size・file ID を batch 取得
- **macOS** — `getattrlistbulk` で metadata を bulk 取得
- **Work stealing** — directory tree の大きさの偏りに合わせて worker 間で work を再分配
- **Optional NTFS `$MFT` path** — 条件を満たす volume root では直接読み取り。安全に解析できない場合は通常列挙へ fallback

設計詳細は [Performance design](https://github.com/{{ site.repository }}/blob/main/docs/performance.md) にまとめています。

## 使い方

```bash
# 上位 20 件
hyperdu /path --top 20

# JSON
hyperdu /path --json out.json

# GNU du 互換モード
hyperdu --compat gnu -sh /var/log
```

## AI Agent 連携

Claude や Codex などから、ディスク逼迫を structured data として調査できます。

| Tool | 答えること |
|---|---|
| `list_volumes` | どの volume が逼迫しているか |
| `scan_path` | その中で何が大きいか |
| `find_reclaimable` | 再生成可能な候補は何か |

**削除ツールは意図的に提供していません。**

```bash
cargo install hyperdu-mcp --version {{ site.versions.mcp }}
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
```

## インストール

crates.io、GitHub Release の prebuilt binary、Scoop から導入できます。winget は申請中です。

詳細な導入方法は [README](https://github.com/{{ site.repository }}#installation) を参照してください。

## 現在の状態

- Beta
- Windows / Linux は検証対象
- macOS は CLI 実機未検証、GUI は現在 release 対象外
- 公開用 performance benchmark は再計測中

## Documentation

- [Performance design](https://github.com/{{ site.repository }}/blob/main/docs/performance.md)
- [Benchmark plan](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md)
- [Architecture](https://github.com/{{ site.repository }}/blob/main/docs/architecture.md)
- [Documentation index](https://github.com/{{ site.repository }}/blob/main/docs/README.md)

過去の benchmark・Issue 固有設計は `docs/old/` に保存しています。
