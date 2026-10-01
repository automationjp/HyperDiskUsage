# hyperdu-bench + LogMosaic — core-driver v1

Rust製ベンチマークと同時プロファイル収集を、LogMosaicの実際の取り込み・変換・出力へ接続します。対象走査は**1回**で、CPU・I/O・schedulerのプロセス統計、allocation、HyperDU内部イベント、利用可能なLinux perfイベントを同じ単調時計へ関連付けます。

**今回の測定対象は計測専用ドライバ `hyperdu-core-driver-v1` です。公開済みCLI・GUI・MCPそのものの性能ではありません。** 既存の公開倍率や走査コードを変更せず、公開 `FileSystemScanner` APIを利用します。LogMosaicのコードは取り込まず、別プロセスとして使用します。

## Build / tests

```sh
cargo build --locked --manifest-path bench/hyperdu-bench/Cargo.toml --release
cargo test --locked --manifest-path bench/hyperdu-bench/Cargo.toml --all-targets
cargo clippy --locked --manifest-path bench/hyperdu-bench/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path bench/hyperdu-bench/Cargo.toml --check
```

このcrateは通常のRelease workspaceから独立した `publish=false` のworkspaceで、独自の `Cargo.lock` を固定しています。生成物は `bench/hyperdu-bench/target/release/hyperdu-bench` と `hyperdu-bench-driver`（Windowsは `.exe`）。release最適化を維持しdebug symbolsは残します。driverはmimalloc secureをcounting wrapper経由で使い、通常の `run` ではallocation記録とイベント生成を無効化します。

## Linuxでの操作

```sh
mkdir -p bench-data bench-results
BENCH=bench/hyperdu-bench/target/release/hyperdu-bench
DRIVER=bench/hyperdu-bench/target/release/hyperdu-bench-driver
$BENCH dataset --root bench-data/wide --shape wide --files 100000 --bytes 4096
$BENCH run --root bench-data/wide --driver "$DRIVER" --output bench-results/warm-01 --runs 8
$BENCH run --root bench-data/wide --driver "$DRIVER" --base-driver /opt/base/hyperdu-bench-driver --output bench-results/compare-01 --runs 8

# 詳細traceはまず小規模fixtureで動作・イベント量を確認する。
$BENCH dataset --root bench-data/diagnostic --shape wide --files 1000 --bytes 4096
$BENCH diagnose --root bench-data/diagnostic --driver "$DRIVER" --output bench-results/diag-01 --logmosaic-agent /opt/logmosaic/logmosaic-agent --perf /usr/bin/perf
```

`--perf` は任意。能力probeは無作業の短いプロセスで行い、測定datasetを再走査しません。使用可能なイベントを一つの `perf record --clockid mono` にまとめ、対象起動前から有効化します。CPUは99Hzのstack sampling、syscall/schedulerはstackなしのtracepointを同時収集します。ready/go handshake後に走査とnative samplerが並行します。権限昇格やsysctl変更は自動実行しません。

`run` はbase/candidateをAB/BAの順で交互に実行し、各回を独立oracleと照合します。全sampleと中央値を保持し、外れ値を除去しません。`scan_ns` がprimary metricで、親の起動〜終了観測値には2msの終了確認間隔があることを記録します。統計的有意差を判定したという意味ではありません。

## Windowsでの操作

```powershell
New-Item -ItemType Directory -Force bench-data, bench-results
$bench = '.\bench\hyperdu-bench\target\release\hyperdu-bench.exe'
$driver = '.\bench\hyperdu-bench\target\release\hyperdu-bench-driver.exe'
& $bench dataset --root .\bench-data\wide --shape wide --files 1000
& $bench diagnose --root .\bench-data\wide --driver $driver --output .\bench-results\diag-01 --logmosaic-agent C:\tools\logmosaic-agent.exe
```

WindowsはQPC、GetProcessTimes / GetProcessMemoryInfo / GetProcessIoCountersを用います。transfer counterは物理disk trafficとは区別します。Windows ETW/WPRの詳細traceとmacOS native collectorは未実装です。

## LogMosaic連携と分析

```text
1回の走査
  ├─ native CPU / RSS / I/O counters
  ├─ allocation counters
  ├─ HyperDU scan / directory intervals
  └─ Linux perf（利用可能な場合）
             ↓ 同一のmonotonic clock・実験ID
       timeline.jsonl
             ↓ 実logmosaic-agent run-once
       file_tail → logmosaic_json_to_otlp → text exporter
             ↓ 全event保持を検証
       logmosaic.jsonl
             ├─ bottlenecks.json / bottlenecks.md
             └─ timeline.trace.json
```

収集は同時、**LogMosaicへの配送は走査後**です。元のevent時刻を保持しますがlive dashboardではありません。出力件数、重複、timestamp、trace/span ID、実験ID、すべての入力属性を突合します。LogMosaicの契約照合対象は `662947a47d33ccd5a641479137b2796a9a92fb00`。指定されたagentのbuild元を推定せず実バイナリSHA256を記録します。

診断レポートは検証済みの**LogMosaic出力**から生成します。CPU sample上位、PID/TIDごとに対応したsyscall entry/exit間隔、ワーカー別directory処理量、scan前後のnative CPU/I/O差分、allocation累積値を示します。`timeline.trace.json` はTrace Event形式で、表示用に相対microsecondsへ変換し、正確なnanosecondsはJSONLに保持します。

CPU sample割合をwall時間の割合と偽ったり、syscall滞在時間を全てdisk待ちと分類したりしません。並列ワーカーのinterval合計は単一のwall時間ではありません。観測値から原因や改善倍率を自動的に断定しません。

## 取得内容と未対応

| Source | 取得内容 | 限界 |
|---|---|---|
| Time | CLOCK_MONOTONIC/QPC → UNIX ns共通anchor、誤差幅 | 到着時刻で元時刻を上書きしない |
| CPU / memory | native counter、5ms sampling、scan境界snapshot | 短い走査はno_samples、未取得はnull |
| I/O | Linux read/write_bytesとrchar/wchar、Windows transfer counters | physical I/O latencyや非同期I/O因果は未対応 |
| Syscall | Linux raw_syscalls enter/exit、PID/TID/番号で対応付け | 権限・tracefsに依存、番号はarchitecture固有 |
| Scheduler | Linux leader切替counter、perf sched_switch | 全workerのready/blocked/critical pathではない |
| Allocation | 成功alloc/free/realloc数、requested bytes | allocation stacks/live heap/RSSではない。計装自身も含む |
| Internal | scanと各directoryの開始/継続時間、OS thread ID | queue/steal/rollup個別scopeは未対応 |

`complete=true` は走査・oracle・LogMosaic配送検証の完了です。全profiler機能の完備とは別で、v1は `profile_complete=false` とsourceごとの取得状態を出力します。未対応や権限不足を0として扱いません。

正解値はrunner独自のmetadata走査で求め、HyperDUコードをoracleに使いません。契約は通常ファイルのhardlink重複排除後logical bytes / file数 / directory数です。symlink・reparse point・別filesystem・特殊fileは拒否します。allocated-byte/cold-cache/他ツールCLI adapter/GUI・MCP・MFT診断は未実装です。

## 安全性と証拠

結果・実行バイナリはdataset外、outputは新規directoryのみで上書き・削除しません。実行前後のdataset fingerprintとbinary hashを確認します。UNIXの結果directoryは0700、ファイルは0600です。timeout時はこのrunが所有するchild/process groupだけを終了し、既存profiler sessionを停止しません。

fixtureは最大10M files / 64GiB payload、正規化イベントは200k件 / 128MiB、perf rawは128MiBで制限します。大量syscall traceではこの制限に達するため、最初は小規模diagnostic fixtureを使用してください。超過や欠損、失敗、改変、不正出力を成功として隠しません。内部eventには実ファイルパスを載せませんが、perf rawにはシンボル・アドレスが含まれ得るため、実データのartifactを公開CIへuploadしないでください。外部OTLP転送やcredential設定は自動化していません。

`.github/workflows/benchmark-logmosaic.yml` はread-onlyでLinux/Windowsのネイティブ試験・fmt・clippyを行います。LogMosaicのソースがprivateであるため、実agentのbuild/roundtripは `automationjp/logmosaic#131` のprivate CIから、固定した公開HyperDU commitを取得して実行します。公開側へprivate repo用tokenを渡しません。一時的なlock/format取り込みworkflowは撤去済みです。

実agent連携を手元で実行する場合:

```sh
LOGMOSAIC_TEST_AGENT=/absolute/path/logmosaic-agent cargo test --locked --manifest-path bench/hyperdu-bench/Cargo.toml --test e2e real_logmosaic_roundtrip -- --ignored --nocapture
```

通常testでignoreされた実agent試験は、連携PASSとして数えません。
