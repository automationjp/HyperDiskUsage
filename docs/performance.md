# Performance design

HyperDU は、**ディスク使用量を正しく集計しながら、走査に必要な OS 呼び出しと待ち時間を減らすこと**を性能設計の中心に置いています。

この文書は「何倍速いか」を示す benchmark report ではありません。倍率は環境依存なので、現在は再計測中です。ここでは、実装上どこで高速化しているかを説明します。

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

Windows では `--mft` を指定し、管理者権限・NTFS・volume root などの条件を満たす場合、`$MFT` を直接読む経路を利用できます。

この経路は常時有効ではありません。必要な DATA extent を安全に解決できない場合や条件を満たさない場合は、通常の directory enumeration に fallback します。

**高速化のために不完全な結果を返すことはしない**、という境界を優先しています。

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

そのため、README では単一の倍率だけを性能根拠にしません。

## Performance claim status

新しい公開用 benchmark を取得するまで、以下は意図的に空欄です。

| Scenario | HyperDU | Baseline | Ratio | Status |
|---|---:|---:|---:|---|
| Windows / NTFS / warm | TBD | TBD | TBD | Re-test required |
| Windows / NTFS / cold | TBD | TBD | TBD | Re-test required |
| Linux / ext4 / warm | TBD | TBD | TBD | Re-test required |
| Linux / ext4 / cold | TBD | TBD | TBD | Re-test required |
| Linux / XFS / warm | TBD | TBD | TBD | Re-test required |
| Linux / XFS / cold | TBD | TBD | TBD | Re-test required |

## Before publishing a speed claim

性能値を README / Web site に掲載する前に、最低限以下を満たします。

- [ ] 測定対象 commit を固定する
- [ ] release build で測定する
- [ ] HyperDU と比較対象の走査量が一致することを確認する
- [ ] comparator の名前と version を記録する
- [ ] filesystem、kernel、CPU、storage を記録する
- [ ] warm / cold を混同しない
- [ ] cold は実行順序による偏りを避ける
- [ ] 単発の最良値だけで性能を主張しない
- [ ] 不利な結果も残す
- [ ] raw result を後から再検証できる形で保存する

具体的な再計測手順と結果記入欄は [Benchmark plan](benchmarks.md) を参照してください。
