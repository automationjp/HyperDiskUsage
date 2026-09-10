# hyperdu

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

OS固有の高速な列挙処理と並列走査を使う、クロスプラットフォームのディスク使用量解析ツールです。

## 性能

Windows / NTFS と Linux / WSL2 / ext4 の[測定結果と限界](../docs/benchmarks.md)を公開します。
coldとXFSは未測定です。実装の仕組みは[性能設計](../docs/performance.md)を参照してください。

## インストール

```bash
cargo install hyperdu --version 0.5.0-beta.3
```

クレート名も実行コマンドも `hyperdu`。CLIとMCPを一度に導入し、MCPは `hyperdu mcp` を実行した
場合だけ起動します。Rust 1.88+ が必要です。

この版は公開準備中です。現在はリポジトリのルートから
`cargo install --locked --path hyperdu` で導入してください。上のレジストリ用コマンドは公開後に利用できます。
配布済みバイナリの実行にRustは不要です。MSVC/SDK・Linuxの依存環境は[セットアップ](../docs/setup.md)を参照してください。

## 使い方

引数の既定値・対応OS・進捗と出力先は [CLIパラメータリファレンス](../docs/cli-reference.md) を参照してください。`--time` 系引数は既定で有効な `time-format` featureが必要です。

```bash
hyperdu /path --top 20
hyperdu /path --json out.json
hyperdu --compat gnu -k /var/log
hyperdu mcp
hyperdu -- mcp
```

上から順に、大きいディレクトリの表示、JSON出力、GNU du互換モード、MCPサーバ起動、
`mcp` という名前のディレクトリの走査です。

## 高速走査の仕組み

- Linux: `getdents64` + `statx`
- Windows: 割当サイズとファイルIDを含む `NtQueryDirectoryFile` の一括列挙
- macOS: `getattrlistbulk`
- ワーカーごとのLIFOキューとwork stealing
- 条件を満たす場合のNTFS `$MFT` 直接走査

MFTで完全な結果を安全に解析できない場合は、通常のディレクトリ列挙へ戻ります。
インストール形式・GUI・プラットフォーム状況・制限は[プロジェクトREADME](../README.md)を参照してください。

## ライセンス

MIT
