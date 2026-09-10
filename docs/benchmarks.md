# HyperDU と du の比較

最新版のAWS EC2ベンチマークを準備中です。GNU `du` と同じ条件（物理割当量・リンク非追跡・ハードリンク重複排除・同一filesystem・全ディレクトリ出力）で、各ディレクトリの表示バイト数が直接一致した結果だけを掲載します。以前のWSL2測定は補正を要する異なる集計条件だったため、速度の根拠から外しました。

## 採用条件

- AWS EC2のLinux x86_64で、最新ソースsnapshotからreleaseビルドします。WSL2の速度をAWSの結果として使いません。
- GNU duの実装・version、source／binary SHA-256、取得日時、instance・CPU・RAM・EBS・filesystem・kernelを記録します。
- 両ツールは同じ読み取り専用データ、同じ権限、同じ集計・出力条件で実行します。Python等によるサイズ補正は行いません。
- 初回照合と毎回の計測で、ディレクトリ別の出力値・行集合が完全に一致することを要求します。不一致・エラー・データ変更は測定失敗とし、速度表に採用しません。
- 性能設定は別のpilotで選び、本計測の前に固定します。近似サイズや論理サイズへの置換で高速化しません。
- warm計測は初回照合後、先行ツールを交互に変えて各8回実行し、プロセス起動と出力を含む中央値と全試行を保存します。不利な結果も残します。

## 比較コマンド

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

双方ともディレクトリ行を全件出力します。出力順は比較時だけ正規化し、合計やバイト数は補正しません。HyperDUのスレッド数・I/O設定など、採用した性能設定は結果とともに明示します。

## ディスクとディレクトリ

Linuxではディスク用MFT経路を使わず、マウントされたfilesystemのルートもディレクトリも同じ列挙エンジンで走査します。専用EBSのマウントルートと代表ディレクトリを候補とし、測定可能な範囲を確定後に記載します。OS稼働中の `/` 全体や変化中のデータを比較対象にしません。

## 現在の状態

同一条件のGNU du回帰テストはローカルLinuxで修正前の不一致と修正後の一致を確認済みです。これは正確性の検証で、AWSの速度測定ではありません。AWS接続先と有効な認証情報が未確定のため、最新の速度値はまだありません。

[Reproducible benchmark runner](../scripts/bench/du_same_conditions.py)

[AWS runbook and provenance records](benchmarks/aws-protocol.md)

## OS 高速経路の共通 corpus

[remeasure.py](../scripts/bench/remeasure.py) は、同じ corpus に対する baseline/candidate の比較に使います。flat・wide・deep の生成、既存 tree、100 万件規模を指定できます。独立した filesystem metadata oracle と全ディレクトリの logical bytes・allocated bytes・file count を照合し、全試行の一致と走査前後の corpus fingerprint 一致を採用条件にします。GNU du と AWS の比較は上記の別 protocol を使います。

計測には clean な source commit、binary SHA-256、その commit と binary を結ぶ build record が必要です。最低 4 組の交互試行と中央値を保存し、entries/sec、CPU user/system time、page faults、peak RSS を記録します。read bytes は OS ごとの意味を明記し、取得できない項目は理由付きの null にします。Linux の syscall 診断は `strace` による別走査で測り、時間計測に混ぜません。entries の分母は訪問したディレクトリ内で列挙した項目数です。

warm を既定とし、cold は各試行の前に実行する cache-reset コマンドを明示した場合だけ選べます。storage・network・runner の条件は `--environment-label` 等で記録します。未測定の NVMe/HDD/SMB/NFS に結果を外挿しません。出力は上書きせず、失敗時も `complete: false` と証跡を残します。

CI の macOS ARM64 と Intel jobs は native fallback と bulk metadata を同じ release binary、flat/deep 各 25,000 files、4 組で比較し、raw JSON と build record を artifact に保存します。この runner 内の計測は、専用機や cold-cache の性能保証ではありません。
