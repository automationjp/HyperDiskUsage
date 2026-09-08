# Benchmark plan

> **Status: remeasurement required**
>
> README / Web site で公開する性能値は現在再計測中です。過去の数値は [old/benchmarks-2026-09-07.md](old/benchmarks-2026-09-07.md) に移動しました。新しい測定が完了するまで、この文書の結果欄は意図的に `TBD` のままにします。

## 目的

HyperDU の性能を、比較対象より都合のよい条件だけ選ばずに再現可能な形で評価します。

確認したいことは次の 3 点です。

1. 同じファイル集合・同じサイズ semantics を走査したときにどれだけ速いか
2. warm / cold、tree shape、filesystem によって差がどう変わるか
3. 高速化によって正確性を失っていないか

## 公開用結果

### Windows / NTFS

| Dataset | Cache | Files | HyperDU | robocopy `/L /S` | GNU du / other | Ratio | Commit |
|---|---|---:|---:|---:|---:|---:|---|
| TBD | warm | TBD | TBD | TBD | TBD | TBD | TBD |
| TBD | cold | TBD | TBD | TBD | TBD | TBD | TBD |

### Linux / ext4

| Dataset | Cache | Files | HyperDU | `du` | Ratio | Commit |
|---|---|---:|---:|---:|---:|---|
| TBD | warm | TBD | TBD | TBD | TBD | TBD |
| TBD | cold | TBD | TBD | TBD | TBD | TBD |

### Linux / XFS

| Dataset | Cache | Files | HyperDU | `du` | Ratio | Commit |
|---|---|---:|---:|---:|---:|---|
| TBD | warm | TBD | TBD | TBD | TBD | TBD |
| TBD | cold | TBD | TBD | TBD | TBD | TBD |

## 再計測で必ず記録するもの

### Hardware / OS

- [ ] CPU model
- [ ] physical cores / logical threads
- [ ] RAM
- [ ] storage model / media type
- [ ] OS / kernel version
- [ ] filesystem
- [ ] WSL / VM / bare metal の区別

### Build

- [ ] Git commit SHA
- [ ] working tree が clean か
- [ ] `cargo build --release -p hyperdu-cli` の build を使用
- [ ] `rustc --version`
- [ ] 特別な `RUSTFLAGS` / feature の有無

### Comparator

- [ ] tool name
- [ ] exact version
- [ ] command line
- [ ] Windows では POSIX compatibility layer 経由か native tool かを明記
- [ ] Linux の `du` が GNU coreutils / uutils のどちらかを明記

## Correctness gate

速度比較の前に、両ツールが同じ仕事をしていることを確認します。

最低限、次の値を記録します。

| Check | HyperDU | Comparator | Match? |
|---|---:|---:|---|
| file count | TBD | TBD | TBD |
| directory count | TBD | TBD | TBD |
| logical bytes | TBD | TBD | TBD |
| physical / allocated bytes | TBD | TBD | TBD |

hardlink、symlink、sparse file、permission error、filesystem boundary など semantics が異なる場合は、倍率を出す前に差を説明します。

**走査対象が一致しない benchmark は公開用の速度根拠にしません。**

## Warm benchmark

Linux では付属 harness を優先します。

```bash
cargo build --release -p hyperdu-cli
scripts/bench/vs_du.sh /path/to/tree
```

再計測では、少なくとも次を残します。

- [ ] 全 run の raw time
- [ ] median
- [ ] minimum（参考値として残す場合）
- [ ] files / dirs / bytes parity
- [ ] background I/O の有無

単発の最小値だけを代表値にはしません。

## Cold benchmark

```bash
scripts/bench/vs_du.sh --cold /path/to/tree
```

cold benchmark では storage cache / burst の影響が大きいため、次を必須にします。

- [ ] cache reset 方法を記録
- [ ] burn-in を行う
- [ ] HyperDU / comparator の実行順を交互にする
- [ ] 複数 pair の median を使う
- [ ] storage burst が枯渇した run を隠さない

## Windows comparison

Windows では、少なくとも 2 種類の比較を検討します。

1. **Native baseline** — `robocopy /L /S` など Windows native API を使うツール
2. **du compatibility baseline** — GNU du / uutils など利用者が `du` 代替として比較しやすいもの

MSYS2 等の compatibility layer を通る結果だけで「Windows で何倍速い」と主張しないようにします。

## Test matrix

優先順位は次のとおりです。

| Priority | Scenario | Why |
|---|---|---|
| P0 | Windows / NTFS / NVMe | Windows fast path の主要対象 |
| P0 | Linux / ext4 / NVMe | Linux の代表的なローカル filesystem |
| P0 | Linux / XFS | サーバ用途の主要対象 |
| P1 | shallow / wide tree | 並列化・bulk enumeration が効きやすいケース |
| P1 | deep / narrow tree | 不利なケースを確認する |
| P1 | many small files | metadata-heavy workload |
| P2 | HDD | I/O-bound 時の差を確認する |
| P2 | NFS / SMB / DrvFS | network / virtual filesystem の挙動を確認する |
| P2 | Windows `--mft` | optional MFT path の効果と parity を確認する |

## PR / release publication gate

新しい速度値を README に入れる PR では、PR 本文に以下を添付します。

- [ ] benchmark commit
- [ ] environment summary
- [ ] exact commands
- [ ] raw result または保存先
- [ ] correctness parity
- [ ] warm / cold の区別
- [ ] median と run count
- [ ] 比較対象 version
- [ ] 最も不利だった代表ケース

すべて揃うまで、README と Web site の数値は `TBD` のままにします。
