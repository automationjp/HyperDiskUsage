# hyperdu-bench + LogMosaic (core-driver v1)

Rust のベンチマークと同時プロファイル収集。測定対象の走査は **1 回**のまま、CPU / I/O / scheduler のプロセス統計、割り当て統計、HyperDU 内部イベント、利用可能なら Linux perf を同じ単調時計へ関連付けます。LogMosaic の実際の `file_tail -> logmosaic_json_to_otlp -> text` 経路を実行し、出力の件数・時刻・trace/span ID・各属性まで照合します。

## 今回の境界

比較対象は `hyperdu-core` の公開 `FileSystemScanner` API を呼ぶ **計測専用ドライバ**です。公開済み `hyperdu` CLI そのもの、GUI、MCP、MFT の計測ではありません。CLI の既存ベンチマーク結果や倍率を上書きしません。既存走査コードと PR #94 の担当ファイルには変更を加えません。

ベンチマークの正解値は runner 独自の `symlink_metadata` 走査で計算し、HyperDU の集計コードを流用しません。hardlink を file identity で重複排除した通常ファイルの logical bytes / files / directories が契約です。v1 では symlink、reparse point、別 filesystem、特殊ファイルを拒否します。allocated-size 比較、他社 CLI アダプタ、cold-cache 制御、統計的有意差判定は未実装です。

通常の配布 workspace とは独立した非公開 crate です。LogMosaic のコードや独自ライセンスを MIT 配布物へ取り込まず、利用者が用意する別プロセスとして接続します。

## Build

```sh
cargo build --manifest-path bench/hyperdu-bench/Cargo.toml --release
```

生成物は `bench/hyperdu-bench/target/release/hyperdu-bench` と `hyperdu-bench-driver`（Windows は `.exe`）。release 最適化を保ち、perf 用 debug symbol は残します。driver の allocator は counting wrapper + mimalloc secure です。通常の `run` では計装記録と allocation counter を無効にします。

## Linux

```sh
mkdir -p bench-data bench-results
BENCH=bench/hyperdu-bench/target/release/hyperdu-bench
DRIVER=bench/hyperdu-bench/target/release/hyperdu-bench-driver
$BENCH dataset --root bench-data/wide --shape wide --files 100000 --bytes 4096
$BENCH run --root bench-data/wide --driver "$DRIVER" --output bench-results/warm-01 --runs 8
# base driver も同じ protocol でビルドしたものを指定。ツールを AB/BA 順に交互実行。
$BENCH run --root bench-data/wide --driver "$DRIVER" --base-driver /opt/base/hyperdu-bench-driver --output bench-results/compare-01 --runs 8
# LogMosaic は別途ビルド/導入した本物の executable を指定する。
$BENCH diagnose --root bench-data/wide --driver "$DRIVER" --output bench-results/diag-01 --logmosaic-agent /opt/logmosaic/logmosaic-agent --perf /usr/bin/perf
```

`--perf` は任意。能力の事前probeは短い無作業プロセスに対して行い、対象datasetを繰り返し走査しません。使えるイベントを1つの `perf record --clockid mono` セッションにまとめ、ドライバ起動前から有効にします。ドライバの ready/go handshake 後に native sampler と走査を並行実行します。権限昇格、sysctl変更、システム全体のWPR/perfセッション停止は行いません。

## Windows (PowerShell)

```powershell
New-Item -ItemType Directory -Force bench-data, bench-results
$bench = '.\bench\hyperdu-bench\target\release\hyperdu-bench.exe'
$driver = '.\bench\hyperdu-bench\target\release\hyperdu-bench-driver.exe'
& $bench dataset --root .\bench-data\wide --shape wide --files 100000
& $bench diagnose --root .\bench-data\wide --driver $driver --output .\bench-results\diag-01 --logmosaic-agent C:\tools\logmosaic-agent.exe
```

Windows は QPC へ時刻を統一し、GetProcessTimes / GetProcessMemoryInfo / GetProcessIoCounters と内部イベント・allocation counter を取得します。**WPR/ETW の詳細 trace はまだ実装していません。** Windows の transfer counter を物理ディスク読み取り量として扱いません。macOS のnative process/perf collectorは未実装です。

## Source semantics

| Source | 実装 | 限界 |
|---|---|---|
| time | 共通 monotonic -> UNIX ns anchor、誤差幅保存 | 到着時刻で元のevent時刻を上書きしない |
| CPU / memory | Linux procfs / Windows process APIs、5ms sampling | 短すぎる走査は `no_samples`。未取得は null |
| I/O | Linux read_bytes/write_bytes + rchar/wchar、Windows transfer counters | 物理IO latencyや非同期IO因果関係は導出しない |
| scheduler | Linux leader-thread切替counter、利用可能ならperf sched_switch | 全workerのready/blocked時間やcritical pathではない |
| allocation | GlobalAllocの成功alloc/free/realloc累積counter | request size。allocation stack、live heap、RSSではない。計装自身も含む |
| internal | core scan + 各directory処理の開始時刻/継続時間/OS thread ID | ネスト・並列intervalを加算して総wall時間にしない |
| Linux perf | cpu-clock / raw_syscalls enter+exit / sched_switch、利用可能なsubset | 権限/PMU/tracefsで欠落。parse/loss状態を記録 |
| LogMosaic | 実agentのcanonical adapter + text exporter | logsデータプレーンを利用。OTLP trace/metric receiverは新設しない |

プロファイラを独立実行する設計ではありません。**収集は同時**、LogMosaicへの配送は走査終了後の `run-once` です。元時刻を保持するため相関は失われませんが、live dashboard機能を実装したという意味ではありません。`complete=true` は走査・oracle・配送検証の完了、`profile_complete` は全分析機能の充足であり区別します。v1の `profile_complete` は false で、未対応sourceを明示します。

## Evidence / safety

`result.json`, `samples.jsonl`, `clock.json`, `internal.jsonl`, `timeline.jsonl`, `logmosaic.jsonl`, `logmosaic.toml`, `report.md` と、任意の `perf.data` / `perf-script.txt` を保存します。すべてのイベントに `experiment.id` / `event.id` / `process.pid` / `thread.id` / `time.monotonic_ns` を持たせます。LogMosaic実装の照合基準は `662947a47d33ccd5a641479137b2796a9a92fb00`。任意に渡されたagentがそのrevisionのbuildであると推定せず、実バイナリSHA256と出力の保持検証を記録します。

原本のprofileを残し、割合の増減だけで原因や改善効果を断定しません。CPU samplingをwall-timeへ換算したり、syscall滞在時間を全てdisk待ちと分類したりしません。

outputと実行バイナリはdataset外、outputは新規ディレクトリのみ。既存結果/fixtureは上書き・削除しません。fixtureは最大10M files / 64GiB payload、eventは200k / 128MiBで制限します。timeout時は所有する子process/groupだけを終了します。quota超過/不正出力/改変/LogMosaic件数不一致は成功扱いにしません。内部イベントはファイルパスを記録しませんが、perf raw artifactはシンボルやアドレスを含み得るため、実データのartifactを公開CIへuploadしないでください。外部OTLP送信やcredential設定は自動的に行いません。

`scan_ns` はdriver内の高分解能測定で、primary medianはこちら。`observed_process_elapsed_ns` は親から観測した起動〜終了で2msの終了確認間隔を明示します。初期版の微小な起動時間差を精密な改善値として使わないでください。中央値と全sampleを残し、遅いsampleを削除しません。

## Tests

```sh
cargo test --manifest-path bench/hyperdu-bench/Cargo.toml --all-targets
# Real integration only (not a stub agent):
LOGMOSAIC_TEST_AGENT=/absolute/path/logmosaic-agent cargo test --manifest-path bench/hyperdu-bench/Cargo.toml --test e2e real_logmosaic_roundtrip -- --ignored --nocapture
```

`.github/workflows/benchmark-logmosaic.yml` で Linux / Windows のドライバ・runner試験と、固定したLogMosaicソースからの実agent roundtripを分けて検証します。通常のtestでignoreされる実agent試験を、実連携PASSと数えないでください。
