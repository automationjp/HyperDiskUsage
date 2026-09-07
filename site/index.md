---
layout: default
lang: ja
home_path: /
lang_switch_label: 言語
title: "HyperDU — 何がディスクを埋めているかを、すぐに"
description: "Windows と Linux で実測した高速なディスク使用量解析。何が容量を食っていて、そのうち何を消してよいかを答えます。"
---

# 何がディスクを埋めているか

<p class="lede">
容量が尽きたとき知りたいのは「何が大きいか」だけではありません。<strong>そのうち何を消してよいか</strong>です。HyperDU は両方に答えます。
</p>

```bash
hyperdu C:\ --top 20
```

## 速度

同一マシン（Ryzen 9 3900X / NVMe SSD）での実測です。計測日 2026-09-07。

### Windows — 101,368 ファイル / 2.96 GB

| ツール | 実行時間 | 倍率 |
|---|---|---|
| **HyperDU** | **251 ms** | — |
| robocopy `/L /S` | 14,362 ms | **57×** |
| GNU du 8.32 (MSYS2) | 12,871 ms | **51×** |

### Linux（WSL2 / ext4） — 712,374 ファイル

| ツール | 実行時間 | 倍率 |
|---|---|---|
| **HyperDU** | **191 ms** | — |
| du (uutils coreutils 0.8.0) | 2,900 ms | **15×** |

比較対象は意図的に 2 種類用意しています。MSYS2 の `du` は POSIX 互換層を通るぶん Windows では構造的に不利なので、**互換層を通らない Windows 純正の robocopy** も測りました。それでも 57 倍です。

Linux の `du` は Ubuntu 26.04 既定の uutils（Rust 実装）なので、**Rust 対 Rust** の比較になります。

> **走査量が一致していることを毎回確認しています。** robocopy と HyperDU はファイル数 101,368、バイト数 2,956,477,017 が完全一致しました。片方が何かを除外して速く見えている、ということはありません。

倍率はツリーの形とストレージに強く依存します。深く狭い木では差が縮み、HDD やネットワーク越しでは I/O 律速になります。**環境・手順・不利な結果を含む全数値**は [docs/benchmarks.md](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md) にあります。

## 速い理由

- **OS ごとの一括列挙 API** — Linux は `getdents64` + `statx`、Windows は `NtQueryDirectoryFile`（物理サイズとファイル ID が列挙結果に含まれるため、ハードリンク重複排除に追加のシステムコールが要りません）、macOS は `getattrlistbulk`
- **NTFS の `$MFT` 直読み** — Windows で管理者権限かつボリュームルートを指定した場合
- **ワークスティーリング** — ワーカーごとの LIFO デックから、浅い側＝大きなサブツリーを盗みます

## インストール

### crates.io

```bash
cargo install hyperdu-cli --version {{ site.versions.cli }}
```

ベータ版のため `--version` の明示が要ります。**クレート名は `hyperdu-cli` ですが、入るコマンドは `hyperdu` です**（`ripgrep` が `rg` を入れるのと同じ形）。

### Windows

```powershell
winget install automationjp.HyperDU
scoop install hyperdu
```

> **まだ使えません。** マニフェストは生成できますが、winget は [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) への PR、scoop はバケットへの登録が別途必要で、いずれも未提出です。

### ソースから

```bash
git clone https://github.com/{{ site.repository }}.git
cd HyperDiskUsage
cargo install --path hyperdu-cli
```

## エージェント連携

Claude や Codex のようなエージェントが、**容量の状況を構造化データとして受け取り、何を消してよいか判断できる**ようにする 3 つの面を同梱しています。**互いに依存しないので、どれか 1 つだけを採用できます。**

| 面 | 単独で使えるか |
|---|---|
| MCP サーバ | 任意の MCP クライアント |
| Agent Skill | CLI を叩くので MCP 不要 |
| Agent Plugin | 上 2 つを束ねるだけ |

```bash
cargo install hyperdu-mcp --version {{ site.versions.mcp }}
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
```

公開しているツールは 3 つで、容量逼迫時に必要になる順に対応します。

| ツール | 答えること |
|---|---|
| `list_volumes` | どのドライブが逼迫しているか |
| `scan_path` | その中で何が大きいか |
| `find_reclaimable` | そのうち**再生成できる**のはどれか（放置日数つき） |

**削除ツールは意図的に持たせていません。** エージェントが人間の確認なしにデータを壊す経路を作らないためです。誤った報告は取り返しがつきますが、誤った `rm -rf` はつきません。

`find_reclaimable` は `target/` を名前だけで判定しません。兄弟に `Cargo.toml` が無ければそれは誰かのデータであり、再生成可能と報告するのは危険だからです。

## いま素直に書いておくこと

- **ベータ版です。** API とツールの粒度は変わる可能性があります
- **GitHub Releases はまだありません。** 現時点の導入経路は `cargo install` かソースビルドです
- **macOS は実機未検証**です（ビルドは通ります）
- Linux の実測値は WSL2 上のもので、ベアメタルとは異なる可能性があります

## 使い方

```bash
# 上位 20 件
hyperdu /path --top 20

# 構造化出力
hyperdu /path --json out.json

# du の代替として
hyperdu --compat gnu -sh /var/log
alias du='hyperdu --compat gnu'
```

詳細は [README](https://github.com/{{ site.repository }}#readme) を参照してください。
