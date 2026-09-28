# hyperdu

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

OS固有のメタデータ取得と並列走査を使うディスク使用量アナライザーです。**CLIと読み取り専用MCPを一つのコマンドに統合**し、GUIやRustアプリと同じ `hyperdu-core` を利用します。

## 公開済みベータ版のインストール

`0.5.0-beta.5` はcrates.ioとGitHub Releasesで公開済みです。

```bash
cargo install hyperdu --locked --version 0.5.0-beta.5
```

クレート名も実行コマンドも `hyperdu` です。旧名 `hyperdu-cli` ではありません。MCPは `hyperdu mcp` を実行した場合だけ起動します。ソースビルドにはRust 1.88以降とOSのビルドツールが必要です。配布済みバイナリの実行にRustは不要です。[環境別セットアップ](../docs/setup.md)

開発中のcheckoutを使う場合は、リポジトリのルートで `cargo install --locked --path hyperdu` を実行します。これは公開版のインストールとは別の選択肢です。

## 使い方

```bash
hyperdu /path --top 20
hyperdu /path --json out.json
hyperdu /path --csv out.csv
hyperdu --compat gnu -k /var/log
hyperdu mcp
hyperdu -- mcp
```

上位ディレクトリ表示、JSON / CSV出力、GNU互換出力、MCP起動、最後は `mcp` という名前のディレクトリの走査です。通常の `--top` は物理サイズで順位付けします。引数の既定値・対応OS・進捗と出力先は [CLIリファレンス](../docs/cli-reference.md) を参照してください。`--time` 系引数には既定で有効な `time-format` featureが必要です。

## 仕組みと使い分け

Windowsは `NtQueryDirectoryFile` で名前・割当サイズ・file IDなどをまとめて取得し、追加のファイルオープンを減らします。Linuxは `getdents64` と `statx` を使い、必要なメタデータ取得を並列に処理します。ワーカーごとのLIFOキューとwork stealingで、偏った木へ仕事を配ります。macOS用の `getattrlistbulk` 実装はありますが、現行CI・Releaseの対象ではありません。

`--mft` は条件付きの実験的なWindows NTFS経路です。不完全な必須読み取りや解析では通常列挙へ戻りますが、成功時の完全な集計一致まで保証しません。GNU / POSIX `du` の全オプション互換でもありません。[比較と制限](../README.md)

MCPの `list_volumes`、`scan_path`、`find_reclaimable` は読み取り専用で、削除しません。SkillはCLIだけでも利用できます。[エージェント導入](../plugin/README.md)

## 開発者向け資料

[開発者ガイド](../docs/developer-guide.md) · [性能設計](../docs/performance.md) · [測定結果と限界](../docs/benchmarks.md) · [GUI](../hyperdu-gui/README.md)

ベンチマークの測定コミットと、公開版・現在のHEADは区別してください。

## ライセンス

MIT
