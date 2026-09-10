# Directory index v2（実験機能）

**日本語** · [English](en/index-snapshots.md) · [简体中文](zh-CN/index-snapshots.md)

Windows、Linux、macOS でファイルの identity とサイズを保存し、ディレクトリ集計を再利用できます。対応するローカル filesystem では foreground の変更監視も利用できます。

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx" --once
```

- `refresh` は全体を走査して v2 snapshot を置き換えます。
- `show` は保存値を読みます。ROOT の identity 確認以外はファイルツリーを走査しません。
- `watch` は native journal を読み、変更されたファイルや新しく入った subtree を更新します。Ctrl-C で終了します。
- `watch --once` は journal に追いついて保存した後に終了します。30 秒で追いつけない場合はエラーを返します。

v1 保存ファイルは `refresh` で作り直してください。v1 Rust API は互換性のため残しています。

## 監視方式

| OS | 方式と条件 | 再起動時 |
| --- | --- | --- |
| Windows | NTFS/ReFS の既存 USN journal。volume を読める権限が必要。journal は作成・変更しません | volume、journal ID、利用可能な USN 範囲を検証して再開 |
| macOS | ローカル APFS/HFS の device 別 FSEvents | device UUID と event ID を検証して履歴を再生 |
| Linux | fanotify の filesystem mark。権限や file handle が非対応なら recursive inotify | queue は永続化されないため全体を再走査 |

Linux は `HYPERDU_INDEX_LINUX_BACKEND=auto|fanotify|inotify` で方式を指定できます。明示的な fanotify 指定が使えない場合はエラーです。監視上限、queue overflow、journal 欠落・再作成を検出した場合は再走査します。root 自体が置き換えられた場合は明示的な refresh が必要です。SMB/NFS など native source が非対応の場合は watch が理由を返します。refresh / show は利用できます。

## 集計と freshness

通常ファイルの logical bytes、実際の allocation bytes、ファイル数を保持します。ディレクトリ自身の storage、symlink/reparse point、special file、名前付き alternate stream は加算しません。hardlink は volume と完全な file ID で重複排除し、親 ID と名前の順で決めた一つのリンクへ計上します。Windows は 128-bit file ID、名前は native encoding を保持します。

- `unknown`: 観測前。
- `stale`: 保存値、更新中、または未検証。
- `observed`: その時点までに配送されたイベントを処理済み。atomic snapshot や完全な最新性を意味しません。

mmap、監視外の hardlink 経由の書き込み、リモート変更などは通知されないことがあります。このため watch は `--reconcile-seconds`（既定 900 秒、1〜86400 秒）ごとに全体を照合します。show は常に stale です。

## 保存とコスト

通常のファイル変更は対象の metadata と祖先の集計を更新します。既知ディレクトリの移動では子孫を再走査しません。新規 subtree や再照合では該当ディレクトリを列挙します。

初回走査・snapshot の読み書きは O(ファイルとリンクとディレクトリ数)、読み込み後の root 集計は O(1) です。メモリへの変更反映と、全 snapshot の保存を分けます。`--checkpoint-seconds`（既定 60 秒、1〜3600 秒）で保存間隔を制限し、初回 catch-up と正常終了時にも保存します。強制終了後は最後の checkpoint から再開または再構築します。

`--poll-ms` は既定 100 ms（10〜60000 ms）。stdout は JSON Lines で、`logical_bytes`, `physical_bytes`, `files`, `freshness`, `journal`, `checkpoint_written`, `notifications`, `observed_entries`, `directories_read`, `rebuilt` などを出力します。作業量はその poll の値で、watch 開始時の baseline は含みません。

## Database の制約

- 親ディレクトリを事前に作成し、database を ROOT の外に置きます。このため `/` 全体を CLI の ROOT にはできません。
- database と writer lock は通常ファイルを使い、symlink を拒否します。
- 一つの filesystem 内だけを対象にします。子 mount は別の index として作成します。
- root identity、保存形式、名前、グラフ、サイズ上限、checksum を検証します。checksum は偶発的な破損検出で、認証ではありません。
- 同時 writer は拒否します。隣の `.lock` ファイルは保持し、OS lock はプロセス終了時に解放されます。
- 未完了の更新は保存しません。snapshot と cursor は同じ atomic replacement に含まれます。

通常 scan のオプションは index の固定集計条件には適用しません。実ディレクトリ名が index の場合は `hyperdu ./index` または `hyperdu -- index` で通常走査できます。

[Architecture](architecture.md) · [Performance](performance.md) · [Historical design](old/README.md)