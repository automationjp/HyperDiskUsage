# 開発者ガイド：HyperDUの仕組みと組み込み方

[プロジェクトREADME](../README.md) · [詳細アーキテクチャ](architecture.md) · [性能設計](performance.md) · [CLIリファレンス](cli-reference.md)

HyperDUは、ディスク容量調査のための**再利用可能な走査エンジンと、その利用インターフェース**です。CLI、GUI、MCPを別々の解析プログラムとして作るのではなく、共通の集計・走査処理を `hyperdu-core` に置きます。

このページは現在の実装を読むための案内です。公開済みパッケージの導入はREADME、測定時点の性能は[ベンチマーク記録](benchmarks.md)、過去の設計案は[履歴資料](old/README.md)を参照してください。計画書に項目があるだけで実装済み・公開済みとは判断しません。

## 1. 最初に読むコード

| 目的 | 入口 | 担当すること |
|---|---|---|
| CLIの挙動を調べる | [`hyperdu/src/main.rs`](../hyperdu/src/main.rs) | 引数、互換モード、標準出力・診断、進捗 |
| Rust APIを組み込む | [`hyperdu-core/src/lib.rs`](../hyperdu-core/src/lib.rs) | `Options`、`StatMap`、走査API、イベント |
| OSごとの読み取りを調べる | [`hyperdu-core/src/platform/`](../hyperdu-core/src/platform/) | 列挙・メタデータ取得・Windows MFT |
| 並列処理を調べる | [`hyperdu-core/src/scheduler.rs`](../hyperdu-core/src/scheduler.rs) | ワーカーのジョブ配送 |
| GUIへ結果を届ける | [`hyperdu-gui/src/scan.rs`](../hyperdu-gui/src/scan.rs) | バックグラウンド走査、索引、チャンク転送 |
| GUIで結果を表示する | [`hyperdu-gui/src/app.rs`](../hyperdu-gui/src/app.rs) | 取り込み、一覧、ツリー、ユーザー操作 |
| AIから利用する | [`hyperdu/src/mcp.rs`](../hyperdu/src/mcp.rs) | MCPツールと要求・結果の変換 |
| Linuxスナップショットを使う | [`hyperdu/src/index_cli.rs`](../hyperdu/src/index_cli.rs) | 保存値の更新と読み出し |
| Agent Skillを配布する | [`plugin/`](../plugin/) | CLI利用手順とMCP接続設定。Rustの別ビルド対象ではない |

`hyperdu-core` はCLIやGUIに依存しません。CLIを子プロセスとして起動しなくてもRustアプリから利用できます。旧クレート名 `hyperdu-cli` から移行する場合、現在のインストール対象・コマンドは `hyperdu` です。

## 2. 「走査を速くする」の内訳

### メタデータの取得経路を変える

典型的な実装ではディレクトリから名前を列挙し、それぞれのファイルのサイズを問い合わせます。HyperDUのWindows経路は `NtQueryDirectoryFile` の列挙結果から、名前・サイズ・割当サイズ・file IDをまとめて受け取り、追加のファイルオープンを減らします。

Linuxは `getdents64` によるディレクトリエントリ取得と `statx` によるメタデータ取得を組み合わせます。**正確なサイズのために必要なI/Oまで不要になる方式ではありません。** ディレクトリ列挙の効率と並列スケジューリングを合わせて考えます。macOS用の `getattrlistbulk` 実装はありますが、現行CI・Releaseには含まれていません。

これはAPIと設計パターンの比較です。すべての競合製品が個別取得だけを使っている、あるいは各最適化の効果を単独の倍率で実証した、という意味ではありません。実際の速度差には、ファイル数、木の形、キャッシュ、ストレージ、集計方法が影響します。

### 大きさが偏った木へ仕事を配る

ワーカーごとのLIFOキューとwork stealingを使います。あるワーカーに残った仕事を、空いたワーカーが引き受ける構成です。単にスレッド数を増やすだけでなく、ディレクトリ木の偏りを処理する設計です。

ただし並列化はI/Oを増やし、同じストレージを使う別処理へ影響する場合があります。`IoProfile` はその負荷方針です。Balancedは既定で意図的な先読みを避け、Gentleはワーカー数・先読み・巨大ディレクトリ分割を抑えます。これは集計規則を決める互換モードや、概算を選ぶ性能設定とは別の軸です。

### インターフェースごとに走査を書かない

```text
hyperdu CLI ───────┐
hyperdu mcp ───────┼─> Options / scan API ─> hyperdu-core ─> OS固有経路
hyperdu-gui ───────┘                            │
                         集計結果・イベント・進捗・キャンセル
```

集計、ハードリンク処理、プラットフォーム最適化を共通化し、CLIは引数と出力、GUIは表示、MCPは構造化された契約を担当します。ただし利用可能なオプション、結果の届け方、MFTの採否まで各入口が無条件に同じになるわけではありません。

## 3. Rustから組み込む

バッチ走査の最小例です。既存のRustプロジェクトで `hyperdu-core` を依存に追加します。依存版は利用する公開版に合わせて固定してください。

```rust
use std::path::Path;
use hyperdu_core::{scan_directory, Options};

fn main() -> anyhow::Result<()> {
    let options = Options::default();
    let directories = scan_directory(Path::new("."), &options)?;
    for (path, stat) in &directories {
        println!("{}\tlogical={}\tphysical={}\tfiles={}",
            path.display(), stat.logical, stat.physical, stat.files);
    }
    Ok(())
}
```

返り値の `StatMap` は**ディレクトリパスをキーとする集計結果**です。ファイル一件ごとの一覧だと解釈しないでください。安定した順序が必要なら呼び出し側で順序を決め、JSON / CSVにはコアの [`report`](../hyperdu-core/src/report.rs) を利用できます。

実運用では、最小例に加えてエラー報告・進捗・キャンセルを接続します。`Options::cancel` は共有の `Arc<AtomicBool>` です。キャンセルは協調的なもので、実行中の同期I/Oの即時停止ではありません。回復可能な読み取りエラーもあるため、返り値が `Ok` であることだけを「全件読み取り成功」の証拠にしないでください。コアのAPIドキュメントと既存フロントエンドの処理を参照します。

## 4. 完了を待たずに表示する

`scan_directory_mode` のInteractiveモードは、最初にルート直下を列挙して `RootListed` を通知し、子フォルダごとに走査して `ChildCompleted` を届けます。正常終了は `Finished`、キャンセルは `Cancelled` で区別します。これは**一度の走査における段階的な結果配送**であり、ファイル変更の常時監視ではありません。

GUIでは走査・索引・並び順の準備を専用スレッドへ移し、256ノード単位・容量2メッセージのキューで配送します。UIは1フレーム1024件・3msを目安に取り込みますが、時間制限は協調的で、メモリ確保やモデル破棄まで含む厳密な3ms保証ではありません。全結果の索引は保持するため、全体が一定メモリで動くという意味でもありません。

JSON / CSV export時の全行の複製・ソートなど、UI上の同期処理は残っています。大規模データのUI応答性を評価する際は、走査中だけでなくexport時も確認します。

## 5. AI連携の境界

`hyperdu mcp` はstdioサーバです。`list_volumes`、`scan_path`、`find_reclaimable` の3ツールを提供します。`scan_path` でprogress tokenが与えられた場合、初期値0と増加した処理件数をMCPのprogress通知で届けます。件数は完了パーセントではありません。他の全ツールへ同じ通知契約があると推測しないでください。

3ツールに削除機能はありません。ただし「読み取り専用」はアクセス範囲を自動隔離するサンドボックスという意味ではありません。実行ユーザーの権限と、調査対象パスの指定を運用側で管理してください。`find_reclaimable` の結果も削除許可ではなく、確認候補です。

Agent SkillはCLIを使った手順であり、MCPサーバを動かさずに利用できます。Pluginは配布のまとまりです。この3つを別々の走査エンジンとして説明しないでください。

## 6. 正確性と対応範囲を誤解しない

| 項目 | 現在の境界 |
|---|---|
| 物理サイズと論理サイズ | 別の値。論理のみ・概算では物理欄に代替値が入る場合があり、実割当サイズとして使わない |
| ハードリンク | 既定は重複排除。合計が同じでも、どのフォルダへ帰属するかは走査順に依存する |
| GNU / POSIX互換 | 選択できる互換モードと全オプション完全互換は別。未対応項目は[監査](posix-compatibility.md)を参照 |
| MFT | Windows MSVC・NTFSボリュームルート・管理者権限・対応条件が必要な任意経路。成功しても通常列挙との完全一致を保証しない |
| MFTとInteractive | MFT成功時は `BatchFallback(Mft)` と `BatchCompleted` で一括配送を明示。MFT失敗から通常列挙へ戻る動作とは別 |
| Linux index | `refresh` が保存値を更新し、`show` は保存値を `stale` として読む。自動追従watcherではない |
| macOS | 実装の存在と検証・配布を区別。現行CIとReleaseはWindows / Linux |
| 最低Rust版 | manifestの宣言値と依存を含むビルド保証は別。GUIの1.75宣言はeguiの要求と矛盾する |

GUIが使うegui系の [0.32.0のworkspace manifest](https://github.com/emilk/egui/blob/0.32.0/Cargo.toml) は `rust-version = "1.85"` を宣言します。これはGUIが1.75では構築できない根拠ですが、依存一式を含めて「1.85なら必ず構築可能」と証明したものではありません。最低版CIとmanifestの整合は別途解決が必要です。当面は新しいstable Rustを使います。

## 7. 変更を検証する

```bash
cargo check --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test --workspace --locked --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
cargo check --workspace --locked --features hyperdu-core/prof-tracy
cargo check --workspace --locked --features hyperdu-core/prof-puffin
python3 scripts/package/test_supply_chain.py
python3 scripts/package/test_registry.py
```

PythonテストにはPyYAMLが必要です。TracyとPuffinは同時に有効化せず、`--all-features` を使わないでください。OS固有の経路はそのOSで検証します。Linux上のテスト成功だけでWindowsのMFTやGUIの描画を検証済みとは扱いません。

性能変更は[ベンチマーク手順](benchmarks.md)に従い、集計の一致を先に確認します。報告にはコミット、OS / ファイルシステム、ハードウェア、データセット、cache条件、logical / allocated、試行回数、timeoutを残してください。

## 8. CI・リリースとドキュメントの更新

通常CIはビルド、テスト、オフラインの供給網回帰テストに加え、公開しない状態で `cargo package --workspace --locked` と公開リクエストデータ生成を確認します。

Releaseのcrates.io経路は [`registry.yml`](../.github/workflows/registry.yml) に分離しています。secretのないrunnerでパッケージを検証し、別runnerが同一runのartifactをデータとして読み、固定のレジストリAPIへ送ります。公開runnerはcheckout、Cargoビルド、artifact内コードの実行を行いません。既存版はチェックサム一致と非yankを確認し、通信エラーを「未公開」と誤判定しません。

この経路のオフラインテスト・パッケージ生成成功と、実際のレジストリ公開成功は別の証拠です。PRを検証するためだけに新しい版を公開しないでください。

ドキュメント更新では、次の3つの時点を分けます。

- **公開版**：実在するRelease assetsとcrates.ioの版。インストール案内に使う。
- **現在の実装**：mainまたはレビュー対象HEAD。開発者向けのコード説明に使う。
- **測定版**：benchmarkのcommit。版番号だけを更新して性能測定を最新版の証拠にしない。

公開版を更新するときは、日本語・英語・簡体字のREADME、CLI / GUIのREADME、サイトの導入案内を合わせて確認します。ストア登録、OS対応、性能倍率、概算値の意味もそれぞれ実証した範囲で表現します。
