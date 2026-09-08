# HyperDU Documentation

HyperDU のドキュメントは、**高速なディスク使用量解析をどう実現し、どうセットアップし、どう検証するか**を中心に整理しています。

現在公開する性能数値は再計測中です。過去の計測値は `old/` に保存し、現行ドキュメントでは新しい測定が完了するまで `TBD` としています。

## まず読む

| 目的 | ドキュメント |
|---|---|
| インストール、Rust、OS依存、build/test環境を準備したい | [Setup and build environment](setup.md) |
| HyperDU がなぜ速いのか知りたい | [Performance design](performance.md) |
| 性能を再計測・比較したい | [Benchmark plan](benchmarks.md) |
| CLI / GUI / MCP と scanner の関係を知りたい | [Architecture](architecture.md) |
| Linux の保存済み snapshot を使いたい | [Linux directory snapshots](index-snapshots.md) |
| AI Agent / MCP / Skill を使いたい | [Agent Plugin / Skill](../plugin/README.md) |

## Setup first

HyperDU は導入方法によって必要な環境が異なります。

- **prebuilt / Scoop / `.deb` を使うだけ**: Rust toolchain は不要
- **CLI / Core / GUI をソースから build**: Rust 1.75+ が最低要件
- **MCP または workspace 全体を build/test**: Rust 1.88+ が必要
- **Windows source build**: MSVC toolchain + Windows SDK を推奨
- **Linux source build**: Rust に加えて native build toolchain が必要
- **Linux GUI**: X11 / Wayland development libraries が必要

コマンドを含む詳細は [Setup and build environment](setup.md) を参照してください。

## Performance first

HyperDU の主題は、単に Rust で `du` を再実装することではありません。

- OS 固有の高速な列挙 API を使う
- syscall / handle open をできるだけ減らす
- サブツリー単位で並列化し、work stealing で負荷を均す
- filesystem の性格に応じて走査戦略を変える
- Windows では条件を満たす場合に NTFS `$MFT` を直接読む

詳細は [Performance design](performance.md) を参照してください。

## Benchmark status

**現在の公開用 benchmark は再計測待ちです。**

| Platform | Dataset | HyperDU | Comparison | Ratio |
|---|---|---:|---:|---:|
| Windows / NTFS | TBD | TBD | TBD | TBD |
| Linux / ext4 | TBD | TBD | TBD | TBD |
| Linux / XFS | TBD | TBD | TBD | TBD |

数値を埋める前に必要な作業は [Benchmark plan](benchmarks.md) にチェックリストとして記載しています。

## Current docs と old docs の区別

`docs/` 直下は、現在の利用者・開発者が参照する文書です。

`docs/old/` は過去の benchmark、Issue 固有の設計、実装・検証記録です。履歴として残しますが、**現在仕様や現在の性能主張の根拠としては使用しません**。

過去資料の一覧は [old/README.md](old/README.md) を参照してください。
