# Architecture

**日本語** · [English](en/architecture.md) · [简体中文](zh-CN/architecture.md)

HyperDU は、**高速な scanner core を 1 つ持ち、その上に CLI / GUI / MCP を載せる**構成です。

性能に関わる処理を interface ごとに重複実装せず、`hyperdu-core` に集約することで、CLI・GUI・AI Agent が同じ走査 semantics と platform fast path を共有します。

Cargo workspaceは `hyperdu/`、`hyperdu-core/`、`hyperdu-gui/` の3クレートです。`hyperdu-core` はCLIやGUIに依存しないRustライブラリで、走査・集計・進捗・キャンセル・索引・レポートを提供します。`hyperdu` は引数処理と表示、`mcp` サブコマンドのstdio待機を担当し、`hyperdu-gui` は同じcoreを直接呼ぶGUIです。

## Overview

```text
       hyperdu CLI        hyperdu mcp        hyperdu-gui
            \                  |                  /
             +-----------------+-----------------+
                               |
                         hyperdu-core
                    scan / model / domain
                         /     |     \
                      Linux Windows macOS
```


## Components

| Component | Responsibility | Performance role |
|---|---|---|
| `hyperdu-core` | tree scan、集計、platform abstraction | hot path の中心 |
| `hyperdu` | command parsing、表示、export | scanner を呼ぶ薄い interface |
| `hyperdu-gui` | desktop UI、drill-down | 同じ core scanner を利用 |
| `hyperdu mcp` (CLI 内の subcommand) | structured MCP tools | 同じ core scanner を agent 向けに公開 |
| `plugin/` | Agent Skill / Plugin | CLI / MCP の利用手順を提供 |
| `scripts/bench/` | benchmark harness | performance claim の再現性を担保 |

## Scan data flow

通常 scan は概ね次の流れです。

1. CLI / GUI / MCP が scan option を構築する
2. `hyperdu-core` が target filesystem と platform を判定する
3. platform-specific enumeration path を選ぶ
4. directory work を worker queue に投入する
5. worker が subtree を処理し、必要に応じて work stealing する
6. file / directory の logical・physical usage を集計する
7. hardlink 等の重複を処理する
8. subtree totals を親へ roll up する
9. interface が human-readable / JSON / CSV / structured MCP result に変換する

## Interactive mode

`hyperdu-core::scan_directory_mode` は `ScanMode::Interactive` と `ScanMode::Batch` を提供します。GUIのインタラクティブモードでは、最初にルート直下を列挙し、子フォルダを1つずつ全ワーカーで走査して結果を通知します。完了済みのフォルダは、残りの走査中にも閲覧できます。

オプションは元のルートで一度だけ準備し、全フェーズでハードリンク・リンク循環の検出状態、ファイルシステム境界、進捗、エラー数、キャンセルを共有します。子フォルダの深さは1から始まるため、深さ制限も元のルート基準です。GUIにファイル列挙や重複判定の別実装を持たせません。

ルート直下のファイルは、コアがフィルタを適用して集計した合計値として通知します。ディレクトリ結果と混同する仮想パスは作りません。MFT直接走査に成功した場合は、結果が一括になることをイベントで明示します。キャンセルされた子フォルダを完了として通知することはありません。

ハードリンクの総量は両モードで一致しますが、重複する名前のどのフォルダにサイズを帰属させるかは走査順に依存します。一括走査と子フォルダごとの内訳まで常に一致するとは限りません。

### GUI のバックグラウンド処理と表示

GUIは専用の `hyperdu-gui-scan` スレッドからコアを呼び、ディレクトリ索引・合計・4種類の並び順もそのスレッドで準備します。走査結果の取り込みや画面の並び順切替では、UIスレッドにファイル列挙や全結果の再ソートを持ち込みません。

```text
core ScanEvent -> background index / sorting
                       |
                node chunks (256)
                       |
                 bounded queue (2)
                       |
           UI ingestion budget -> tree / visible table rows

core progress counter -----------------> status / elapsed time
```

ノード更新は最大256件のチャンク、キュー容量は2メッセージです。UIは1フレームあたり最大1024件の更新作業と3msの目安で取り込み、継続分を次のフレームへ残します。時間は更新の間に確認する協調的な制限であり、1回のメモリ確保や古いモデルの破棄まで3ms以内に収める保証ではありません。バックグラウンド側も全体の索引を保持するため、総メモリ量がこのキュー容量だけに制限されるわけではありません。

親ディレクトリの並び順は参照先ノードを送信した後に公開します。これにより存在しないIDの参照を避けますが、最初のチャンクから表の行が増えるとは限りません。表は表示領域の行を描画し、ツリーは深さと描画件数を制限します。深い階層は表とパンくずから移動できます。

走査開始時に処理中状態へ切り替え、結果キューとは別の共有カウンタで進捗・経過時間を更新します。到着イベントは再描画を要求し、走査中は定期的にも更新します。キャンセルは共有フラグでコアと索引準備へ伝わり、途中結果を完了として表示しません。同期I/Oや実行中の単一ソートの即時中断は保証しません。受信側を破棄するとキュー待ちの送信側を解放し、古い走査の結果を次の走査へ混ぜません。

終端イベントは先行する更新の後に処理します。エラーがある場合、キャンセルされた場合、完了通知なしに送信側が終了した場合を成功完了と区別します。JSON/CSVのユーザー操作によるexportでは全行を複製してパス順にソートします。これやモデル置換など、UI上で同期実行する処理は残っています。

実装: [GUI transport / index](../hyperdu-gui/src/scan.rs)、[UI取り込み・描画](../hyperdu-gui/src/app.rs)、[core modes / events](../hyperdu-core/src/lib.rs)。

## Platform boundary

### Linux

```text
Directory
   |
   +--> getdents64  -- names / inode / type --> worker
                                             |
                                             +--> statx --> size metadata
```

Linux では directory enumeration と metadata 取得が分かれます。正確なサイズを取る以上 metadata syscall 自体は必要なので、列挙効率・並列化・filesystem strategy が重要になります。

### Windows

```text
Directory
   |
   +--> NtQueryDirectoryFile
          |
          +--> name
          +--> file size
          +--> allocation size
          +--> file ID
```

Windows では 1 回の batch enumeration からサイズと file ID まで取得できるため、追加 handle open を減らせることが高速化の中心です。

### Optional MFT path

```text
shared eligibility guard
 Windows MSVC / volume root / administrator / supported options
                         |
                   open NTFS volume
                         |
       extent-limited raw window -> copied record -> fixups / parse
                         |
              parent-record-ID aggregation
                         |
               directory paths / rollup

ineligible or incomplete required read/parse -> directory enumeration
```

MFTはWindows MSVCの実験的な任意経路です。CLI・GUI・コアの直接APIは同じ利用条件を参照します。ボリュームルート・管理者権限に加え、raw/compiled除外条件、深さ制限、最小サイズ、リンク追従、ハードリンク別計上、概算、外部の重複排除キャッシュがある場合はMFTを選びません。通常の走査APIはその場合にディレクトリ列挙へ戻ります。直接APIは `None` を返すため、検証側はMFTが実際に使われたかを区別できます。

readerは既定1MiBのraw-byte windowで連続したMFTレコードをまとめて読みます。先読みは物理extent内に制限し、レコードがextent境界をまたぐ場合は必要な部分を組み立てます。fixupはコピーしたレコードへ適用するので、再読込や拡張レコード参照が修正済みキャッシュを誤って使うことを避けます。先読みに失敗した場合はキャッシュを破棄して必要部分だけを再試行し、必須読取・解析が不完全な結果は採用しません。

集計は親レコードIDへサイズを加算してからディレクトリパスを生成し、コアで親へroll upします。ハードリンクIDと、欠損・循環した親の扱いは既存の集計規則を維持します。読取windowには上限がありますが、全エントリと集計結果はメモリに保持するため、MFT全体を一定メモリで処理する方式ではありません。

readerは走査前・最大256レコードごと・正常な末尾で進捗とキャンセルを確認します。レコード件数は未使用slotも含み、集計後のファイル件数や完了割合とは異なります。拡張レコードの同期読取を即時に中断する保証はありません。Interactive要求でもMFTが成功すると `BatchFallback(Mft)` と `BatchCompleted` で一括結果の配送を明示します。これはMFT失敗による通常列挙へのfallbackとは別です。

利用条件を満たすことや必要レコードを解析できることは、通常列挙との完全一致の証明ではありません。既知の集計差は残るため、速度測定では実際の経路と集計差を別々に確認します。

実装: [共通利用条件](../hyperdu-core/src/platform/windows_impl/mod.rs)、[reader](../hyperdu-core/src/platform/windows_impl/mft_reader.rs)、[raw window](../hyperdu-core/src/platform/windows_impl/mft_reader/window.rs)、[ID集計](../hyperdu-core/src/platform/windows_impl/mft_aggregate.rs)。

## CLI / MCP の進捗通知

通常CLIの集計はstdoutへ、進捗と診断はstderrへ出します。stderrが端末なら処理中表示を自動で有効にし、リダイレクト時は `--progress` で明示できます。開始直後に状態を表示し、完了時は待機中の状態スレッドを起こすので、更新周期を待つ必要はありません。

`hyperdu mcp` はrmcpを使うstdioサーバです。`scan_path` がMCPの `_meta.progressToken` を受け取った場合、標準の `notifications/progress` で初期値0と、その後の増加したファイル件数を通知します。tokenがない場合は通知を送りません。tokenとキャンセル状態はリクエストごとに保持し、並行した呼出しを混同しません。

コアの同期走査は `spawn_blocking` で実行します。コールバックはwatch channelの最新件数を更新するだけなので、通知のために走査スレッドを通信待ちにしません。送信側で増加値をまとめ、途中通知を約250ms間隔に制限します。最終件数が未送信の場合は結果の前に通知しますが、通知自体は件数でありパーセントではなく、正常完了は最終ツール結果で判断します。

リクエストのキャンセル、破棄、通知送信の失敗はコアのキャンセルフラグへ伝わります。これは協調的な中断であり、OSの同期I/Oを即時に止める保証ではありません。この進捗adapterは `scan_path` 用であり、全MCPツールの進捗を保証するものではありません。

実装: [CLI](../hyperdu/src/main.rs)、[MCP tools](../hyperdu/src/mcp.rs)、[MCP progress adapter](../hyperdu/src/mcp/progress.rs)。引数と出力条件は [CLIリファレンス](cli-reference.md) を参照してください。

## Concurrency model

単純な固定 range 分割では、directory tree のサイズ偏りによって worker の idle が増えます。

HyperDU は worker ごとの LIFO deque を持ち、他 worker が work を steal できる構造にしています。

狙いは以下です。

- locality を保ちながら現在の subtree を深掘りする
- idle worker が大きな未処理 subtree を引き取る
- queue の瞬間的な空状態を scan 完了と誤認しない

thread 数そのものではなく、**tree shape の偏りに追従すること**が目的です。

## Correctness before speed

HyperDU の fast path は、結果 semantics を変えないことを前提にします。

特に以下は performance optimization のために省略しません。

- hardlink accounting
- logical / physical size の区別
- unsupported fast path の fallback
- incomplete MFT parse の拒否
- scan error / cancellation の扱い

速度値を公開する場合も、先に scan parity を確認します。手順は [Benchmark plan](benchmarks.md) を参照してください。

## Persisted Linux snapshots

`index refresh` / `index show` は通常 scan の置き換えではなく、明示的に保存した directory aggregate を再利用する別機能です。

自動 watcher ではなく、`show` の結果は `stale` と明示されます。詳細は [Linux directory snapshots](index-snapshots.md) を参照してください。

## Historical design records

過去の Issue 固有設計、MFT 検証記録、旧 persistent-index 案は [old/README.md](old/README.md) へ移動しています。

現在の architecture を理解する入口としては、この文書と [Performance design](performance.md) を使用してください。
