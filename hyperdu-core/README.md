# hyperdu-core

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

[HyperDU](https://github.com/automationjp/HyperDiskUsage) の走査エンジンです。
ディレクトリツリーを並列に走査し、各ディレクトリの論理サイズ・物理サイズ・ファイル数を返します。
通常の利用には [`hyperdu`](https://crates.io/crates/hyperdu) コマンドを使ってください。
このクレートはアプリケーションへ走査機能を組み込むためのライブラリです。

未公開のチェックアウトから組み込む場合は、コアクレートを直接依存に指定します。

```toml
[dependencies]
hyperdu-core = { path = "../hyperdu-core" }
```

この checkout で生成する Rust APIドキュメントでは、一括・インタラクティブ走査、
共有キャンセルと進捗フック、レポート出力、永続インデックス、ボリューム情報のAPIを説明します。
`cargo doc --locked -p hyperdu-core --no-deps --open` で開けます。
公開済みバージョンには[別のRust APIドキュメント](https://docs.rs/hyperdu-core/latest/hyperdu_core/)があります。
コアクレートは `hyperdu` CLI・MCPサーバー・GUIなしで直接利用できます。

## 一括走査

```rust
use hyperdu_core::{scan_directory, Options};

let stats = scan_directory("/path", &Options::default())?;
let root = stats.get(std::path::Path::new("/path")).unwrap();
println!("{} bytes across {} files", root.physical, root.files);
# Ok::<(), anyhow::Error>(())
```

集計値には子孫のサイズを含みます。`scan_directory_mode` の `ScanMode::Interactive` では、
最初にルート直下の一覧と直接のファイル合計、続いて完了した子フォルダごとの結果を通知します。
オプション・ハードリンクの重複排除・リンク循環・ファイルシステム境界・進捗・エラー・キャンセルを
全フェーズで共有します。MFT走査が成功した場合は、一括結果になることを明示します。
イベントの契約は[アーキテクチャ](../docs/architecture.md)を参照してください。

## 実装

- Linux: `getdents64` + `statx`。
- Windows: 割当サイズとファイルIDを含む `NtQueryDirectoryFile` の一括列挙。
  権限とボリュームルートの条件を満たす場合はNTFS `$MFT` の直接読み取りも利用できます。
- macOS: `getattrlistbulk`。実機検証は未実施です。
- ワーカーごとのLIFOキューとwork stealing。
- ファイルシステム上の識別子によるハードリンク重複排除。既定の `du` と同様に集計します。

`volume::list()` はファイルシステムの容量、`index` は保存可能なディレクトリ集計を提供します。
`reclaimable` は生成物の候補を検出しますが、更新時刻のサンプルは不使用や削除の安全性を保証しません。
削除操作は行いません。

## ライセンス

MIT
