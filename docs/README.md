# HyperDU ドキュメント

**日本語** · [English](en/README.md) · [简体中文](zh-CN/README.md)

導入方法、スキャンエンジンの設計、検証方法をまとめています。

| 目的 | 文書 |
|---|---|
| インストール・Rust・OS依存・build/test環境 | [セットアップ](setup.md) |
| 高速化の仕組みとトレードオフ | [性能設計](performance.md) |
| Windows/Linuxの結果・全試行・測定条件 | [ベンチマーク](benchmarks.md) |
| CLI・GUI・MCPとコア、インタラクティブ走査 | [アーキテクチャ](architecture.md) |
| Linuxの保存済みディレクトリ情報 | [スナップショット](index-snapshots.md) |
| AIエージェントとの連携 | [Plugin / Skill / MCP](../plugin/README.md) |

## 環境の準備

配布済みバイナリの実行にRustは不要です。ソースからのCLI（MCP同梱）またはworkspace全体の
ビルドにはRust 1.88+、core/GUIの宣言上の最低版は1.75です。WindowsはMSVCとWindows SDK、
Linuxはネイティブビルドツール、Linux GUIはX11/Waylandの開発ライブラリが必要です。
詳しいコマンドと検証範囲は[セットアップ](setup.md)を参照してください。

## 性能の読み方

OS固有の列挙API、メタデータ問い合わせの削減、work stealingを使い、対応条件ではNTFSの
`$MFT` 直接読み取りも選択できます。性能表は測定した環境・データ・キャッシュ条件にのみ適用します。
Linuxの公開測定はWSL2 / ext4 / uutils du、WindowsはNTFS / robocopyです。
GNU du・cold・XFSを計測した結果ではありません。[全試行と限界](benchmarks.md)を確認してください。

## 現行文書と過去の記録

`docs/` 直下が日本語版、`docs/en/` が英語版、`docs/zh-CN/` が簡体字中国語版です。
コマンド・版・測定表は共通です。`docs/old/` は過去のベンチマークやIssue固有の設計・検証記録を
原文のまま保存しています。現在の仕様や速度の根拠として使わないでください。
[過去資料の一覧](old/README.md)を参照できます。

[POSIX du compatibility audit](posix-compatibility.md)
