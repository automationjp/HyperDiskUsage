# Performance design

**日本語** · [English](en/performance.md) · [简体中文](zh-CN/performance.md)

HyperDU は、**ディスク使用量を正しく集計しながら、走査に必要な OS 呼び出しと待ち時間を減らすこと**を性能設計の中心に置いています。

この文書では、実装上どこで高速化しているかを説明します。倍率と測定条件は[ベンチマーク](benchmarks.md)を参照してください。

## 結論

性能の主な設計ポイントは次の 5 つです。

1. **OS 固有の高速な列挙 API を使う**
2. **ファイルごとの追加 syscall / handle open を減らす**
3. **サブツリーを並列処理し、work stealing で偏りを吸収する**
4. **filesystem ごとに不利な処理を避ける**
5. **利用可能な場合だけ、より高速な専用経路を使い、安全に fallback する**

## Platform fast paths

| Platform | Main path | 狙い |
|---|---|---|
| Linux | `getdents64` + `statx` | directory entry をまとめて読み、metadata 取得を低オーバーヘッド化する |
| Windows | `NtQueryDirectoryFile` + `FileIdFullDirectoryInformation` | 名前、サイズ、allocation size、file ID をバッチで取得する |
| macOS | `getattrlistbulk` | directory metadata を bulk 取得する |

### Linux

Linux の directory entry にはファイルサイズが含まれないため、正確なサイズ集計では各ファイルの metadata が必要です。HyperDU は directory enumeration に `getdents64` を使い、metadata 取得には `statx` を使います。

したがって Linux では「syscall を完全になくす」のではなく、**列挙オーバーヘッドを小さくし、metadata 取得を効率よく並列化する**ことが中心になります。

filesystem に応じて buffer、prefetch、推奨スレッド数を変えます。割当量を論理サイズへ自動的に置き換えません。ネットワーク filesystem や DrvFS では、ローカル ext4/XFS と同じ戦略が常に最適とは限りません。

Linux x86_64 GNU の `--xfs-bulk` は、CAP_SYS_ADMIN があり、superblock が読み取り専用の XFS 全体を走査するときだけ BulkStat を使います。inode と directory entry を結び付け、取得できない metadata は通常の経路で補います。書き込み可能な XFS、subdirectory、非対応 ioctl では使用しません。

`linux-io-uring` feature と `--io-uring` を指定すると、最大64件の `statx` をまとめて発行します。path と結果 buffer は全 completion を回収するまで保持し、要求失敗・欠落属性は同期取得へ戻します。既定は同期経路です。

### macOS

`getattrlistbulk` でファイルの data-fork logical length、全 fork の allocation、device ID、file ID、link count を取得します。返却 attribute mask と record 境界を検査し、必要な metadata が欠ける項目だけ `fstatat` で補います。通常ファイルごとの `lstat` とフルパス生成を避け、hardlink は device と inode の組で重複排除します。FIFO・ソケット等は通常ファイルに数えません。

`HYPERDU_MAC_USE_GALB=0` は native fallback との比較用です。高速 API が最初から非対応の場合は通常走査へ戻り、実際のアクセス失敗は scan error として残します。

### Filesystem selection

Linux の mount 情報に加え、Windows の volume 情報と macOS の `statfs` を使って filesystem を検出します。NTFS・ReFS・APFS/HFS の native 経路、SMB/NFS 等の network 経路を区別します。Windows の UNC と network drive は remote として扱います。検出不能時は generic 設定を使い、検出結果だけで MFT を有効にしません。

### Windows

Windows の標準経路では `NtQueryDirectoryFile` を使い、`FileIdFullDirectoryInformation` から directory entry と同時に allocation size と file ID を取得します。

これにより、物理サイズ取得や hardlink 重複排除のために**各ファイルを追加で open する必要を減らせる**ことが大きな利点です。

環境変数 `HYPERDU_WIN_USE_NTQUERY=0` で `FindFirstFileExW` 経路へ切り替えられます。高速経路が使えない状況を診断するときの比較にも使えます。

### Optional NTFS `$MFT` path

Windows MSVCでは `--mft` を指定し、管理者権限・NTFS・volume rootの条件を満たす場合、`$MFT` を直接読む経路を利用できます。除外・深さ制限・最小サイズ・リンク追従・ハードリンク別計上など未対応の設定では通常列挙へ戻ります。対応する設定は[CLIリファレンス](cli-reference.md)に記載しています。

この経路は常時有効ではありません。必要な DATA extent を安全に解決できない場合や条件を満たさない場合は、通常の directory enumeration に fallback します。

ディレクトリごとに通常列挙と同じ一覧取得権限を確認します。アクセス拒否のディレクトリは空の行として残し、配下を集計しません。symlink と junction は追跡せず除外します。未知・不完全な reparse metadata、想定外の権限確認エラー、除外対象に複数のハードリンク名があり可視性を確定できない場合は通常列挙へ戻します。解析完了は変更中のボリュームとの完全一致を保証せず、MFTは実験的です。

`HYPERDU_MFT_IO=sync|overlapped|unbuffered|auto` で MFT I/O を比較できます。`auto` は geometry が確認できれば buffered overlapped を使い、非対応なら同期読み取りを選びます。unbuffered は明示指定だけで有効になります。現在の1 MiB window を解析する間に次の window を読み、キャンセルやランダムな extension record 読み取りでも未完了 I/O の buffer を解放しません。短い読み取りや geometry の不一致は同期処理へ戻します。これは `--mft` の利用条件を緩めません。

## SIMD and incremental updates

名前の絞り込みと MFT の UTF-16 ASCII 部分は、実行時に対応 CPU を確認して AVX2 または ARM64 NEON を使います。末尾・非 ASCII・不正 surrogate は scalar 処理で同じ結果を返します。`HYPERDU_SIMD=scalar` で比較用の scalar 経路を選べます。AVX-512 は Rust 1.89 以上で `simd-avx512` feature を有効にしたビルドだけに含み、実行時の CPU 判定も必要です。

[Directory index v2](index-snapshots.md) は、初回走査の後に OS 通知が示した項目と必要な subtree を再確認します。通常のファイル更新では祖先の合計だけを更新し、directory rename は子の identity を保持します。通知の欠落・再開条件の不一致では全体を再構築し、通知だけでは検出できない変更に備えて定期照合します。

## Parallel traversal

コア scanner は、worker ごとの LIFO deque と work stealing を使って directory tree を処理します。

重要なのは、単純に thread 数を増やすことではありません。directory tree はサブツリーごとに大きさが異なるため、固定分割だけでは一部 worker が先に空きます。

HyperDU は未処理 work を他 worker が奪えるようにし、巨大な subtree に処理が偏った場合でも CPU と I/O の待ちを分散します。

また、queue が一時的に空になっただけで全 worker が終了しないよう、in-flight work を追跡します。

## What can make the gap smaller

高速化の効果は workload に依存します。特に次の条件では差が縮む可能性があります。

- ファイル数が少ない
- 深く狭い tree
- HDD
- NFS / SMB / SSHFS / 9p / FUSE など network・virtual filesystem
- storage latency が支配的
- 権限や filesystem 条件により fast path が使えない
- cold cache と warm cache で支配要因が変わる
