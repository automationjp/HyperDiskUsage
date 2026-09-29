# HyperDU

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

> **「何がディスクを埋めているか」を、人にもアプリにも、同じエンジンで。**
> OS固有のメタデータ取得と並列走査を組み合わせた、Rust製のディスク使用量アナライザーです。CLIで調べ、GUIで掘り下げ、MCPでAIエージェントへ構造化データを渡せます。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

[Webサイト](https://hyperdu.automation.jp/) · [セットアップ](docs/setup.md) · [開発者ガイド](docs/developer-guide.md) · [CLIリファレンス](docs/cli-reference.md) · [ドキュメント一覧](docs/README.md)

## 公開済みベータ版を使う

**v0.5.0-beta.5 は公開済みです。** [GitHub Release](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5)とcrates.ioから導入できます。「未公開」ではありませんが、安定版ではなくベータ版のため、CLI・MCPスキーマ・出力形式は変更される可能性があります。

```bash
cargo install hyperdu --locked --version 0.5.0-beta.5
hyperdu . --top 20
```

クレート名もコマンド名も **`hyperdu`** です。旧名 `hyperdu-cli` ではありません。CLIとMCPサーバを一緒にインストールし、MCPは `hyperdu mcp` を実行したときだけ起動します。

実行だけなら、ReleaseのWindows / Linux向けバイナリを使えます。**配布済みバイナリの実行にRustは不要**です。ソースビルドにはOSのビルドツールと新しいstable Rustを使ってください。現在のソースの最低Rust版はcoreが1.82、GUIが1.85、CLIが1.88です。各版をLinux / Windows CIで確認します。公開済みベータ版に同梱された古いmanifestは書き換わらないため、公開版のソース導入には新しいstable Rustを使用してください。

## 従来の調べ方と、何が違うのか

HyperDUの特徴は「Rustだから速い」ことではなく、**サイズを集める方法、仕事の分け方、結果の再利用先を一体で設計していること**です。

| 課題 | HyperDUの実装 | 開発者にとっての違い |
|---|---|---|
| 名前を列挙してから個別にメタデータを調べると、OS呼び出しが増える | Windowsは `NtQueryDirectoryFile` で名前・サイズ・割当サイズ・file IDをまとめて取得 | 追加のファイルオープンを減らせる。Linuxでは `getdents64` と `statx` を使うが、必要なメタデータ取得までゼロになるわけではない |
| 固定的な仕事分担では、大きいサブツリーを担当するワーカーだけが残る | ワーカーごとのLIFOキューとwork stealing | 空いたワーカーが仕事を引き受け、偏ったディレクトリ木を処理する |
| CLI・GUI・エージェントで別々の走査を実装すると、集計規則がずれる | 独立ライブラリ `hyperdu-core` を各入口から利用 | ハードリンク、サイズ集計、プラットフォーム最適化を共通化する |
| コマンド出力をAIに解釈させるだけでは、入力と結果の契約が曖昧になる | 組み込みMCPの型付きツールと構造化結果 | 調査の結果をプログラムで扱える。削除ツールは提供しない |
| 全走査が終わるまでGUIで何も確認できない | Interactiveモードで完了した子フォルダから結果を通知 | 残りの走査中にも完了済みの内訳を閲覧できる。常時監視とは異なる |

これは設計上の比較です。他のすべての解析ツールが単一スレッド、個別取得、非構造化出力であるという主張ではありません。速度の比較は、下記の実測条件に限定します。

[開発者ガイド](docs/developer-guide.md)では、各仕組みのコード上の入口、組み込み例、性能と正確性の境界を説明しています。

## CLIで調べる

```bash
hyperdu /path/to/data --top 20
hyperdu /path/to/data --json usage.json
hyperdu /path/to/data --csv usage.csv
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -b --time /usr/share
hyperdu --help
```

通常の上位表示は**ディレクトリを物理サイズで順位付け**します。`--apparent-size` を付けても `--top` の順位は物理サイズ基準です。`--time` 系オプションには既定で有効な `time-format` featureが必要です。除外条件、出力の深さ、走査制限、リンク追従はそれぞれ意味が違うため、[CLIリファレンス](docs/cli-reference.md)を参照してください。

**GNU / POSIX `du` の完全な置き換えではありません。** `--compat posix-strict` は512-byte単位などを選びますが、POSIX必須の `-a`、`-s`、`-H`、`-L` は未対応です。無条件の `du` エイリアス化は避けてください。[互換性監査](docs/posix-compatibility.md)

## GUI

```bash
cargo install hyperdu-gui --locked --version 0.5.0-beta.5
hyperdu-gui
```

Windows / Linux向けの `egui` / `eframe` GUIです。Interactiveモードでは子フォルダの結果を順次表示し、Batchモードでは全体の結果を受け取ります。ツリー・パンくず・並べ替え可能な一覧、フィルタ、進捗・エラー・キャンセル表示、JSON / CSV出力を備えています。

画面の文字は日本語です。概算モードは正確な割当サイズを保証せず、キャンセル・読み取りエラーを正常完了とは扱いません。[GUIの操作と制限](hyperdu-gui/README.md)

## AIエージェントから利用する

```bash
hyperdu mcp
# MCPクライアントへの登録例
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

| ツール | 用途 |
|---|---|
| `list_volumes` | ボリュームの容量と空き容量を調べる |
| `scan_path` | 指定パスの使用量を調べる |
| `find_reclaimable` | 再生成できる可能性がある出力などを整理候補として探す |

**3ツールは読み取り専用で、削除しません。** 候補であることは削除して安全であることの保証ではなく、人が確認して判断します。MCPサーバ、CLIを使うAgent Skill、両者を配布するPluginは独立した入口です。SkillだけならMCPは不要です。[導入手順](plugin/README.md)

## 実測性能と、その読み方

以下は公開済みベンチマークの記録であり、現在のHEADや全環境で再計測した値ではありません。**測定ソースは `2645689515ab2e608a78b7492637e55179e4739a`** です。公開版の番号と測定コミットを混同しません。

### Linux：構成ごとに100万ファイル

GitHub-hosted Ubuntu 24.04 / ext4、AMD EPYC 9V74（4 vCPU、15.6 GiB RAM）。各構成に256 Bの通常ファイルを1,000,000個作成し、warm cache・割当バイトで、各ツール8回の交互実行の中央値を比較しました。成功したディレクトリ行は独立したoracleと一致しています。

| 構成 | HyperDU | GNU `du` | du / HyperDU |
|---|---:|---:|---:|
| flat | 2327.49 ms | 2763.52 ms | 1.187倍 |
| wide | 745.96 ms | 2428.80 ms | 3.256倍 |
| deep | 764.67 ms | 2434.28 ms | 3.183倍 |

「最大3.25倍高速」はこのLinux測定の見出しです。cold cache、別ファイルシステム、HDD、ネットワーク越しの結果へは広げません。

### Windows：100万ファイル単独計測と1万ファイル診断

Windows 11 / NTFS / NVMe、Ryzen 9 3900X（12 cores / 24 threads、128 GiB RAM）、warm cache・論理バイト。100万ファイルのflatはHyperDU単独8回の中央値が589.91 msでした。GNU `du` 8.32（Git for Windows / MSYS）はwarmupで600秒に達したため、**100万ファイルの受理済み比較倍率はありません**。wide / deepの100万ファイルは未実行です。

| 1万ファイル診断・各ツール2回の中央値 | HyperDU | GNU `du` (MSYS) | du / HyperDU |
|---|---:|---:|---:|
| flat | 32.98 ms | 704.17 ms | 21.35倍 |
| wide | 27.46 ms | 712.33 ms | 25.94倍 |
| deep | 32.32 ms | 836.73 ms | 25.89倍 |

この診断倍率を100万ファイルや一般的なWindows性能へ外挿しません。Linuxとは集計方法・環境・試行回数も異なります。

[実行記録](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) · [生データ](https://hyperdu.automation.jp/benchmarks.json) · [測定方法と限界](docs/benchmarks.md) · [性能設計](docs/performance.md)

## 配布とプラットフォーム

[公開済みRelease](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5)で、Windows x86_64のCLI / GUI、Linux x86_64（glibc / musl）・aarch64のバイナリと配布形式を選べます。ファイル名と提供形式はReleaseのAssetsを確認してください。パッケージ生成用manifestが存在することと、ストアへの登録完了は別です。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

wingetは、このリポジトリのmanifestだけを根拠に登録済みとは案内しません。利用する場合は `winget search --id automationjp.HyperDU --exact` でカタログを確認してください。

<a id="platform-status"></a>

| OS | 現行の検証・提供範囲 |
|---|---|
| Windows | ネイティブCI、NTFS fixture、CLI / GUIの配布 |
| Linux | CI、CLI / GUIの配布、上記ext4ベンチマーク |
| macOS | `getattrlistbulk` の実装あり。ただし現行CI・Release対象外。CLIの検証は未完了で、GUIの利用を保証しない |

Windowsの `--mft` はNTFSボリュームルート・管理者権限・対応オプションなどの条件を要する実験的な任意経路です。必要な読み取りや解析が不完全なら通常列挙へ戻ります。ただし**MFTに成功したことは通常列挙との完全一致の証明ではありません**。[MFTの利用条件と限界](docs/architecture.md#optional-mft-path)

## 実験的なLinuxスナップショット

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

これは常時監視ではありません。`show` は保存値を読み、freshnessは常に `stale` です。[スナップショットの仕様](docs/index-snapshots.md)

## 開発する

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p hyperdu -- --help
```

クレート構成、Rust APIの組み込み例、CI・配布の権限分離、機能別の検証コマンドは[開発者ガイド](docs/developer-guide.md)にまとめています。TracyとPuffinは同時に有効化できないため、`--all-features` を一括の検証コマンドにしないでください。

[詳細アーキテクチャ](docs/architecture.md) · [環境別セットアップ](docs/setup.md) · [旧設計・履歴](docs/old/README.md) · [全ドキュメント](docs/README.md)

## License / Acknowledgements

[MIT License](LICENSE)。設計上の参考： [ripgrep](https://github.com/BurntSushi/ripgrep)、[fd](https://github.com/sharkdp/fd)、[dust](https://github.com/bootandy/dust)。
