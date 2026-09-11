# HyperDU と GNU du の比較

GitHub Actions の hosted Linux runner で、HyperDiskUsage と GNU `du` を直接比較します。物理割当量・リンク非追跡・ハードリンク重複排除・同一 filesystem・全ディレクトリ出力を揃え、各ディレクトリの表示バイト数が直接一致した結果だけを速度の根拠にします。WSL2 や内部 API 同士の比較を、製品間の速度比較として掲載しません。

## 再現方法

`ci` workflow の手動実行で `product_benchmark=true` を指定します。通常の push・PR・既定の手動実行は従来の検証を続け、この入力を指定した実行は専用の `benchmark-du` job だけを動かします。

```bash
gh workflow run ci.yml --ref COMMIT_BRANCH --field product_benchmark=true
```

`ubuntu-24.04` の GitHub hosted runner 上で、取得した commit を `cargo build --locked --release -p hyperdu` でビルドします。既定 features、通常の release profile、移植可能なコンパイラ設定を使い、I/O profile・スレッド数・filesystem 選択も既定にします。CI のビルド短縮用 LTO 設定はこの job では使いません。

## 採用条件

- Linux x86_64 の GitHub hosted runner であること、workflow の commit・repository・run ID・attempt・job が記録と一致することを検査します。WSL・self-hosted・ローカル実行はこのモードで受け付けません。これは実行来歴の整合性検証であり、暗号学的な環境証明ではありません。
- GNU coreutils du の version・package・binary SHA-256、HyperDU の source snapshot・binary SHA-256・ビルド設定、CPU・RAM・filesystem・kernel・runner image・run URL を保存します。
- flat・wide・deep の各 tree は 256 bytes の通常ファイル 100 万件です。生成器は [remeasure.py](../scripts/bench/remeasure.py) の `make_tree` を共用します。1 tree ずつ生成・計測・削除し、生成やビルドを時間計測に重ねません。
- 両ツールは同じ変更しないデータ、同じ権限、同じ集計・出力条件で実行します。Python 等によるサイズ補正や論理サイズへの置換は行いません。
- 初回照合と毎回の計測で、ディレクトリ別の出力値・行集合が完全に一致することを要求します。前後の corpus fingerprint、両 binary の hash も一致しなければ不採用です。
- 初回照合後の warm 計測で、先行ツールを交互に変えて各8回実行します。プロセス起動と出力を含む中央値・全16試行を保存します。遅かった条件や失敗した実行も残し、全条件で高速だと一般化しません。

## 比較コマンド

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system --io-profile balanced --dir-yield-every 0 -- ROOT
/usr/bin/du -x --block-size=1 -- ROOT
```

双方とも全ディレクトリ行を出力します。出力順は照合時だけ正規化し、合計やバイト数は補正しません。比較 runner は [du_same_conditions.py](../scripts/bench/du_same_conditions.py) です。

## 記録と適用範囲

artifact `hyperdu-gnu-du-RUN_ID-ATTEMPT` に各 tree の JSON、全試行、照合値、実行ログ、ソース・ビルド・環境記録を保存します。失敗時も取得できた記録を保存し、`complete: false` を成功結果として扱いません。公開する速度値は成功した実測と run URL に結び付けます。GitHub hosted runner の warm-cache、合成 directory workload の結果を、別 OS・cold cache・物理ディスク全体・NVMe/HDD/SMB/NFS の性能に外挿しません。

AWS を使う既存の別プロトコルは [AWS runbook](benchmarks/aws-protocol.md) を参照してください。比較 runner の既定モードは互換性のため AWS のままで、GitHub job は `--environment github-actions --github-record ...` を明示します。

## 内部経路の検証

[remeasure.py](../scripts/bench/remeasure.py) による native backend 同士の比較は、正確性・fallback・実装選択を検証する開発資料です。独立 metadata oracle、全ディレクトリの logical/allocated bytes・file count、前後の fingerprint を照合し、CPU time・page faults・RSS 等も保存します。これらの内部比較を HyperDU と他製品の速度比較に混ぜません。
