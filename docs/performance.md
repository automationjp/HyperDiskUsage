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

filesystem に応じて buffer、prefetch、physical-size の扱いなどを変えます。ネットワーク filesystem や DrvFS では、ローカル ext4/XFS と同じ戦略が常に最適とは限りません。

### Windows

Windows の標準経路では `NtQueryDirectoryFile` を使い、`FileIdFullDirectoryInformation` から directory entry と同時に allocation size と file ID を取得します。

これにより、物理サイズ取得や hardlink 重複排除のために**各ファイルを追加で open する必要を減らせる**ことが大きな利点です。

環境変数 `HYPERDU_WIN_USE_NTQUERY=0` で `FindFirstFileExW` 経路へ切り替えられます。高速経路が使えない状況を診断するときの比較にも使えます。

### Optional NTFS `$MFT` path

Windows MSVCでは `--mft` を指定し、管理者権限・NTFS・volume rootの条件を満たす場合、`$MFT` を直接読む経路を利用できます。除外・深さ制限・最小サイズ・リンク追従・ハードリンク別計上など未対応の設定では通常列挙へ戻ります。対応する設定は[CLIリファレンス](cli-reference.md)に記載しています。

この経路は常時有効ではありません。必要な DATA extent を安全に解決できない場合や条件を満たさない場合は、通常の directory enumeration に fallback します。

解析を完了できない結果は採用しません。ただし解析完了は通常列挙との完全一致を保証せず、MFTは実験的です。

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
