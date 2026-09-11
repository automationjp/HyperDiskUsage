# HyperDU ドキュメント

**日本語** · [English](en/README.md) · [简体中文](zh-CN/README.md)

導入方法、スキャンエンジンの設計、検証方法をまとめています。

| 目的 | 文書 |
|---|---|
| CLI引数の既定値・出力形式・対応OS | [CLIパラメータリファレンス](cli-reference.md) |
| インストール・Rust・OS依存・build/test環境 | [セットアップ](setup.md) |
| 高速化の仕組みとトレードオフ | [性能設計](performance.md) |
| GNU duとの同一条件比較・全試行・測定条件 | [ベンチマーク](benchmarks.md) |
| CLI・GUI・MCPとコア、インタラクティブ走査 | [アーキテクチャ](architecture.md) |
| Linuxの保存済みディレクトリ情報 | [スナップショット](index-snapshots.md) |
| AIエージェントとの連携 | [Plugin / Skill / MCP](../plugin/README.md) |
| 2026-09-10時点のセキュリティ監査 | [セキュリティ監査報告書](security-audit-2026-09-10.md) |

## 環境の準備

配布済みバイナリの実行にRustは不要です。ソースからのCLI（MCP同梱）またはworkspace全体の
ビルドにはRust 1.88+、core/GUIの宣言上の最低版は1.75です。WindowsはMSVCとWindows SDK、
Linuxはネイティブビルドツール、Linux GUIはX11/Waylandの開発ライブラリが必要です。
詳しいコマンドと検証範囲は[セットアップ](setup.md)を参照してください。

## 性能の読み方

OS固有の列挙と並列処理の設計は[性能設計](performance.md)を参照してください。現行のLinux結果は、GitHub ActionsのUbuntu 24.04/ext4で、256 Bの通常ファイル100万個を3形状に配置し、各ツールを交互に8回warm計測したものです（全48 raw sample）。全ディレクトリ行の割当バイト数が直接一致し、GNU `du` / HyperDU の速度比は平坦1.187x、wide 3.256x、deep 3.183xでした。保守的な見出し値3.25xはLinuxだけに適用します。Windows 11の補足結果と制限、およびAWSの未実行runbookは[比較条件と取得状況](benchmarks.md)に記載しています。旧WSL2比較の倍率は現行速度の根拠に使用しません。

## 現行文書と過去の記録

`docs/` 直下が日本語版、`docs/en/` が英語版、`docs/zh-CN/` が簡体字中国語版です。
コマンド・版・測定表は共通です。`docs/old/` は過去のベンチマークやIssue固有の設計・検証記録を
原文のまま保存しています。現在の仕様や速度の根拠として使わないでください。
[過去資料の一覧](old/README.md)を参照できます。

[POSIX du compatibility audit](posix-compatibility.md)
