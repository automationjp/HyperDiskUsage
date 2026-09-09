# CLI パラメータリファレンス

通常の走査、保存済みスナップショット、MCP サーバは同じ `hyperdu` コマンドから起動します。このページは現行ソースの引数定義と実際の参照先に基づく日本語の共通リファレンスです。インストールしたバイナリで使える引数は `hyperdu --help` で確認してください。OS・ビルド条件に合わない走査引数は一覧から外れ、指定すると引数エラーになります。

```text
hyperdu [OPTIONS] [ROOTS...]
hyperdu mcp
hyperdu index refresh ROOT --database FILE
hyperdu index show ROOT --database FILE
```

`-h` / `--help` はヘルプ、`-V` / `--version` はバージョン表示です。`/help` と `/?` もヘルプの別名として扱います。`mcp` や `index` という名前のディレクトリを通常走査する場合は `hyperdu ./mcp` または `hyperdu -- mcp` のように指定します。`--` より後は引数として解釈されないため、走査オプションはその前に置いてください。

## 通常走査の入力と絞り込み

以下の既定値は、明示的な引数・環境変数・実行ファイル横の `hyperdu-config.json` による変更がない場合です。サブコマンドと通常走査のオプションは併用しません。

| 引数 | 既定値 | 挙動 |
|---|---|---|
| `ROOTS...` | `.` | 走査するディレクトリ。複数指定可。標準モードでも全ルートを走査し、ルートごとの集計と合計を表示します。 |
| `--exclude LIST` | なし | カンマ区切りの名前の部分一致。`.git` は `.github` にも一致します。 |
| `--exclude-from FILE` | なし | 複数回指定可。1行1パターンで、`re:` は正規表現、`glob:` はglob、その他は部分一致です。空行と `#` で始まる行を除きます。 |
| `--max-depth N` | `0` | 最大走査深さ。`0` は無制限、`1` はルート直下までです。 |
| `--min-file-size BYTES` | `0` | 指定バイト数未満のファイルを集計から除外。`0` は無効です。 |
| `--follow-links` | 無効 | シンボリックリンク／ジャンクションを追跡します。通常走査では循環検知を有効にします。 |
| `-x` / `--one-file-system` | 無効 | 異なるファイルシステムへの横断を制限します。 |
| `--logical-only` | 無効 | 可能な限り物理割当サイズの問い合わせを省き、論理サイズを取得します。 |
| `--count-links` | 無効 | ハードリンクを別ファイルとして数えます。省略時は通常重複排除しますが、`--perf turbo` では有効になります。 |

`--exclude-from` の読込失敗は現行実装ではエラーとして報告されません。除外を前提とする集計ではファイルの存在・内容を確認し、標準出力の `Excludes` と結果を照合してください。

## 並列度と I/O

| 引数 | 既定値 | 挙動 |
|---|---|---|
| `--threads N` | 利用可能CPU数×4を4〜32に制限 | 未指定時はコアの `default_threads()` を使います。CPU数取得失敗時は16。明示指定はfilesystem自動設定の推奨スレッド数より優先しますが、I/Oプロファイルの上限は適用されます。 |
| `--perf PROFILE` | `balanced` | `turbo` / `balanced` / `strict`。サイズ取得と重複排除・互換出力の設定です。 |
| `--io-profile PROFILE` | `balanced` | `throughput` / `balanced` / `gentle`。並列度と先読みの設定です。サイズを概算する設定ではありません。 |
| `--no-fs-auto` | 無効 | filesystem自動設定を止めます。主な設定変更はLinuxのfilesystem戦略です。 |

`--perf turbo` は論理サイズのみ・ハードリンクを別計上とし、概算サイズには切り替えません。`balanced` は通常のサイズ取得と重複排除、`strict` は重複排除を有効にして `gnu-strict` 相当の出力を選びます。`--compat posix-strict` と併用するとPOSIX形式を維持します。これらはGNU/POSIXの完全互換を保証しません。

`--io-profile gentle` は要求数を最大2ワーカーに制限し、大きいディレクトリの分割を止めます。先読みは `throughput` で既定有効、`balanced` / `gentle` で既定無効です。先読み自体はLinux x86_64の対応実装でのみ働き、利用可能な場合は `--prefetch` で明示的に上書きできます。

## 標準出力・JSON・CSV・分類

`--compat hyperdu` が標準モードです。物理サイズ順の一覧とサマリをstdoutへ表示します。複数ルートでは各ルートの合計も表示します。互いに重なるルートを指定すると、ルートごとの合計には重複した範囲が含まれるため、独立した領域の合計として扱わないでください。

| 引数 | 既定値 | 挙動・条件 |
|---|---|---|
| `--top N` | `30` | 物理サイズ順の上位N行。`0` は一覧0行。互換出力には適用しません。 |
| `--json FILE` | なし | 標準モードの走査結果をJSON配列で保存。各行は `path`, `logical`, `physical`, `files` です。 |
| `--csv FILE` | なし | 同じ4列でCSVを保存。 |
| `--classify MODE` | 無効 | 標準モードで追加分類します。`basic` は基本分類、`deep` は先頭バイトによるMIME推定も行います。 |
| `--class-report FILE` | なし | `--classify` を指定したとき、分類JSONを保存。 |
| `--class-report-csv FILE` | なし | `--classify` を指定したとき、`kind,key,files,bytes` 列の分類CSVを保存。 |
| `-v` / `--verbose` | 無効 | 標準モードで `hyperdu-report.json` / `.csv` をカレントディレクトリへ自動保存。分類時は `class-report.json` / `.csv` も保存します。明示した出力先が優先です。 |

JSON/CSVには `--top` で制限する前のディレクトリ行を保存します。各行は子孫を含む集計なので、全行を合算すると重複計上になります。出力先の既存ファイルは上書きされます。分類は最初のルートを対象に追加走査します。`--classify` は現行実装では `deep` 以外を基本分類として扱うため、値の誤記に注意してください。

`--json` はstdoutをJSON専用に変える設定ではありません。機械処理には指定したファイルを読み取ってください。互換出力モードでは、これらのファイル出力・分類・上位N行指定は行われません。

## du 互換出力と時刻

| 引数 | 既定値 | 挙動 |
|---|---|---|
| `--compat MODE` | `hyperdu` | `gnu`, `gnu-strict`, `posix-strict` で `ブロック数<TAB>パス` の出力を選択します。各ルート内の行はパス順です。 |
| `--apparent-size` | 無効 | 互換出力のブロック計算に論理サイズを使い、物理サイズの取得を省略します。標準モードの順位は変更しません。 |
| `--block-size SIZE` | 通常1024 | 例: `512`, `1024`, `1K`, `1M`, `1G`。POSIX形式または `POSIXLY_CORRECT` 存在時の未指定値は512です。正の値を指定してください。 |
| `-b` | 無効 | 互換出力で `--apparent-size --block-size=1` 相当。 |
| `-k` / `-m` / `-g` | 無効 | ブロックサイズを1K / 1M / 1Gにします。 |
| `--si` | 無効 | K/M/Gを1024の累乗ではなく1000の累乗として解釈。 |
| `--time` | 無効 | 互換出力に時刻列を追加。既定で有効な `time-format` featureが必要です。 |
| `--time-kind KIND` | 時刻表示時 `mtime` | `mtime` / `atime` / `ctime`。指定すると時刻列も有効になります。`time-format` が必要です。 |
| `--time-style STYLE` | `iso` | `iso`, `long-iso`, `full-iso`, `+<strftimeパターン>`。単独では時刻列を有効にしません。`time-format` が必要です。 |

単位指定を重ねた場合の優先順は `-b` → `-k` → `-m` → `-g` → `--block-size` →既定値です。未認識の `--block-size` は現行実装では1024にフォールバックするため、上記の形式を使ってください。時刻はUTCで整形し、Windowsの `ctime` は作成時刻、Unixの `ctime` はmetadata変更時刻です。

```bash
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -b --time /usr/share  # time-format有効時
```

GNU/POSIX `du` の全オプションは提供していません。`-s`、`-a`、`-H`、`-L` は未対応です。`-h` はヘルプであり、human-readableサイズ指定ではありません。

Linux x86_64 glibcの `gnu-strict` / `posix-strict` では、ディレクトリ自身の物理割当量と、追跡しないリンク・特殊ファイルも集計します。ディレクトリの見かけのサイズはGNU duと同じく0です。`gnu` と標準モードの通常ファイル中心の集計は維持しています。直接比較には `--compat gnu-strict --block-size 1 --one-file-system` を使用します。この対応を他OS・musl・rootとして渡した通常ファイルや全GNUオプションの互換性へ広げて解釈しないでください。
## 進捗と診断

| 引数 | 既定値 | 挙動 |
|---|---|---|
| `--progress` | stderrが端末なら自動有効 | 開始時に `scanning …`、その後件数・速度・サンプルをstderrへ表示。stderrをリダイレクトするときも表示するには明示します。 |
| `--progress-every N` | `8192` | ファイル件数による進捗通知の間隔。小さい値は更新頻度を上げます。 |

集計や互換出力はstdout、進捗・警告・filesystem戦略の診断はstderrへ出ます。進捗有効時、通知が止まった期間にも処理中表示を定期更新します。`HYPERDU_PROGRESS_KEEPALIVE_SECS` の既定は1秒、最小1秒です。件数通知は全レコード処理の完了割合を表すものではありません。

```bash
hyperdu /path --progress --progress-every 1024 >result.txt 2>progress.log
```

## プラットフォーム限定引数

| 引数 | 受付条件 | 未指定時・効果 |
|---|---|---|
| `--approximate` | Unix、ただしmacOSを除く | 既定無効。論理サイズのみの対応経路で通常ファイルを4KiB相当とする概算を許可します。精密な容量比較には使わないでください。 |
| `--dir-yield-every N` | Unix、ただしmacOSを除く | `HYPERDU_DIR_YIELD_EVERY`、未設定なら0（分割無効）。gentleでは0へ上書きします。 |
| `--prefetch[=true\|false]` | Linux x86_64 | 引数単独はtrue。`--prefetch=false` で無効。未指定時はI/Oプロファイルに従います。 |
| `--pin-threads` | Linux | 既定無効。`HYPERDU_PIN_THREADS=1` 相当。CPUへのワーカー固定を要求します。 |
| `--galb-buf-kb KiB` | macOS | `getattrlistbulk` バッファ。環境変数 `HYPERDU_GALB_BUF_KB`、未設定なら64KiB。指定値は最小4KiBです。 |
| `--win-ntquery` | Windows MSVC | NtQuery列挙を明示有効。対応ビルドでは既定有効で、`HYPERDU_WIN_USE_NTQUERY=0` を上書きします。 |
| `--mft` | Windows MSVC | 既定無効。以下のMFT利用条件を満たす場合に直接読み取りを要求します。 |

### MFT の利用条件と制限

`--mft` はNTFSボリュームのルート（例: `C:\`）と管理者権限を必要とする実験的機能です。通常の列挙との集計差が確認されており、完全一致は保証しません。利用条件を満たさない場合や、読取・解析を完了できない場合は通常の列挙へ戻ります。

MFT側で扱えない除外条件、深さ制限、最小ファイルサイズ、リンク追従、ハードリンク別計上、概算、共有重複排除キャッシュを伴う設定も通常の列挙へ戻します。CLIでは `--count-links` や `--perf turbo`、重複排除キャッシュを用意する互換出力もこの対象になります。これは未対応の指定を無視してMFT集計を返すことを避けるための条件です。

## MCP サーバ

```bash
hyperdu mcp
```

MCPクライアントが子プロセスを起動し、stdin/stdoutでプロトコルを送受信します。通常走査のテキスト表示とは別のモードです。`list_volumes` / `scan_path` / `find_reclaimable` の3ツールを提供し、ファイル削除ツールはありません。ツールの引数はMCPスキーマで渡し、通常走査のCLIオプションは付けません。詳細は [Plugin / Skill / MCP](../plugin/README.md) を参照してください。

`scan_path` はrequestのprogress tokenがある場合に標準の進捗通知を返し、tokenがない場合は送りません。開始通知の後は最新の進捗値を間引いて送ります。総件数が未確定なら割合を捏造せず、最終結果は通常のMCP応答として返します。キャンセルは協調的で、同期I/Oの即時中断を保証しません。
## Linux の保存済みスナップショット

`index` はLinux限定です。その他のOSでは明示的なエラーになります。`--help` からコマンドの構文は確認できます。

| コマンド・引数 | 必須／既定値 | 挙動 |
|---|---|---|
| `index refresh ROOT --database FILE` | ROOTとFILEが必須 | 走査してスナップショットを置き換えます。バックグラウンド監視は起動しません。 |
| `index show ROOT --database FILE` | ROOTとFILEが必須 | 保存した集計を読み出します。ディレクトリ全体を再走査しませんが、ルートidentityの確認は行います。 |

`ROOT` はディレクトリで、databaseはその外側に置きます。databaseの親ディレクトリは事前に作成してください。既存databaseがシンボリックリンクなど通常ファイル以外なら拒否します。割込み・ルートidentityの変更が起きたrefreshは旧スナップショットを保存します。

両コマンドはJSONをstdoutに出力し、`root`, `database`, `device`, `inode`, `physical_bytes`, `files`, `directory_count`, `freshness`, `monitoring` を含みます。`freshness` は常に `stale`、`monitoring` は `false` です。詳細は [Linuxスナップショット](index-snapshots.md) を参照してください。

## 実装との対応

- [CLI引数と出力処理](../hyperdu-cli/src/main.rs)
- [index / MCPサブコマンドの入口](../hyperdu-cli/src/index_cli.rs)
- [コアの既定値・走査設定](../hyperdu-core/src/lib.rs)
- [MFTの利用条件](../hyperdu-core/src/platform/windows_impl/mod.rs)
- [プラットフォーム／featureによる引数の回帰テスト](../hyperdu-cli/tests/removed_options.rs)

性能値と測定日は [ベンチマーク](benchmarks.md) を参照してください。このリファレンスは速度を保証するものではありません。
