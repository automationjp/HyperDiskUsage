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
