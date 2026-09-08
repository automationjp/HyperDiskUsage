# Benchmark plan

> **Status: warm 測定済み (2026-09-08, commit `07dc887`) / cold と XFS は未測定**
>
> Windows/NTFS と Linux/ext4 の warm 測定を [公開用結果](#公開用結果) に記載しました。全 run の raw time、correctness gate の結果、この測定の限界も併記しています。**倍率を引用する前に [この測定の限界](#この測定の限界) を読んでください** — Linux の比較対象は GNU du ではなく uutils で、Linux 側は WSL2 です。cold と XFS の欄は測定するまで `TBD` のままにします。過去の数値は [old/benchmarks-2026-09-07.md](old/benchmarks-2026-09-07.md) にあります。

## 目的

HyperDU の性能を、比較対象より都合のよい条件だけ選ばずに再現可能な形で評価します。

確認したいことは次の 3 点です。

1. 同じファイル集合・同じサイズ semantics を走査したときにどれだけ速いか
2. warm / cold、tree shape、filesystem によって差がどう変わるか
3. 高速化によって正確性を失っていないか

## 公開用結果

測定日 2026-09-08、commit `07dc887`。すべて **warm**、Windows 9 回 / Linux 7 回を
両ツール交互に実行した **中央値**。全 run の raw time を各表の下に載せています。

### Windows / NTFS

AMD Ryzen 9 3900X (12C/24T)、128 GiB RAM、WD_BLACK SN850X (NVMe SSD)、
Windows 11 Pro 10.0.26200、NTFS、ベアメタル。

| Dataset | Cache | Files | HyperDU | robocopy `/L /S` | Ratio | Commit |
|---|---|---:|---:|---:|---:|---|
| `.cargo/registry` (C:) | warm | 111,025 | 396 ms | 1,823 ms | 4.60x | `07dc887` |
| `HyperDiskUsage` (F:) | warm | 35,876 | 148 ms | 546 ms | 3.69x | `07dc887` |
| — | cold | — | 未測定 | 未測定 | — | — |

raw (ms):

```
registry  hyperdu   341  468  558  574  412  396  299  333  339   min=299  med=396  max=574
registry  robocopy 1960 1953 1765 2456 1828 1802 1632 1756 1823   min=1632 med=1823 max=2456
hdu-repo  hyperdu   139  249  304  317  490  143  148  144  132   min=132  med=148  max=490
hdu-repo  robocopy  432  450  546  663  489  594  628  451  581   min=432  med=546  max=663
```

### Linux / ext4

同一マシン上の WSL2 (Ubuntu 26.04 LTS、kernel 6.18.33.2-microsoft-standard-WSL2、
vCPU 16、90 GiB RAM、ext4 on /dev/sdd)。**ベアメタルの Linux ではありません。**

| Dataset | Cache | Files | HyperDU | `du` | Ratio | Commit |
|---|---|---:|---:|---:|---:|---|
| `/usr` | warm | 122,814 | 87 ms | 1,093 ms | 12.56x | `07dc887` |
| `/var` | warm | 712,519 | 342 ms | 3,975 ms | 11.62x | `07dc887` |
| wide (500 dir × 40 file) | warm | 20,000 | 23 ms | 113 ms | 4.91x | `07dc887` |
| **deep (400 段 × 5 file)** | warm | 2,000 | 89 ms | 132 ms | **1.48x** | `07dc887` |
| **flat (1 dir × 20k file)** | warm | 20,000 | 100 ms | 145 ms | **1.45x** | `07dc887` |
| — | cold | — | 未測定 | 未測定 | — | — |

raw (ms):

```
usr   hyperdu   82  101   78   87  118   86   98          min=78   med=87   max=118
usr   du      1162 1075 1024 1030 1203 1093 1113          min=1024 med=1093 max=1203
var   hyperdu  296  342  248  297  424  489  448          min=248  med=342  max=489
var   du      3872 3591 3475 3975 4981 4960 4747          min=3475 med=3975 max=4981
wide  hyperdu   22   24   23   24   23   24   23          min=22   med=23   max=24
wide  du       113  115  119  109  102  123  112          min=102  med=113  max=123
deep  hyperdu   78   85   89   92   84   94   91          min=78   med=89   max=94
deep  du       130  132  137  128  135  129  140          min=128  med=132  max=140
flat  hyperdu   96  103   87  117  100  100   98          min=87   med=100  max=117
flat  du       184  155  162  121  118  145  112          min=112  med=145  max=184
```

**最も不利なケースは deep (1.48x) と flat (1.45x)** です。並列化の効きにくい形では
差が 1.5 倍程度まで縮みます。倍率を語るときはこの 2 つを併記してください。

### Linux / XFS

| Dataset | Cache | Files | HyperDU | `du` | Ratio | Commit |
|---|---|---:|---:|---:|---:|---|
| TBD | warm | TBD | TBD | TBD | TBD | TBD |
| TBD | cold | TBD | TBD | TBD | TBD | TBD |

## この測定の限界

倍率を引用する前に読んでください。

- **Linux の `du` は GNU coreutils ではなく uutils coreutils 0.8.0** です。Ubuntu 26.04 は
  uutils を既定の `du` にしており、GNU 版はこのシステムに存在しませんでした
  (`dpkg -L coreutils` に `du` が無く、diversion も無い)。uutils は一般に GNU より遅い
  ため、**この比較は HyperDU に有利に働いています**。「GNU du の N 倍」とは言えません。
- **Linux 側は WSL2** であり、ベアメタルの Linux ではありません。
- **cold は未測定**です。`vs_du.sh --cold` は `/proc/sys/vm/drop_caches` への書き込みに
  root を要し、WSL2 ではページキャッシュの挙動もホストと異なります。
- **XFS は未測定**です。
- Windows の robocopy は `/XJ` でジャンクションを除外し、HyperDU 側も `-x` で同じ境界を
  守らせています。

### correctness gate

速度を測る前に、3 者が同じ集合を走査していることを確認しました。

| Check | HyperDU | 比較対象 | Match |
|---|---:|---:|---|
| files (`/usr`, `--count-links`) | 122,814 | 122,814 (`find -xdev -type f`) | ✓ |
| files (`.cargo/registry`) | 111,025 | 111,025 (robocopy `/L /S`) | ✓ |
| files (`.cargo/registry`) | 111,025 | 111,025 (PowerShell `Get-ChildItem`) | ✓ |
| dirs (`/usr`) | 13,956 | 13,959 (`find -xdev -type d`) | 差 3 = 後述 |
| logical bytes (hardlink 検証) | 100,000 | 100,000 (`du -sb`) | ✓ |

差の内訳は次のとおりで、いずれも意図された semantics です。

- **files の 117 件差 (既定時)**: HyperDU は `du` と同じくハードリンクを畳みます。
  `--count-links` を付けると `find` と完全一致します。100,000 バイトの inode に 4 本
  リンクを張った検証では `du -sb` = 100,000、HyperDU 既定 = `files=1 log=100000` で
  一致し、`--count-links` = `files=4 log=400000` が `find` と一致しました。
- **dirs の 3 件差**: `/usr` 配下のマウントポイントがちょうど 3 個
  (`/usr/lib/modules/...`、`/usr/lib/wsl/drivers`、`/usr/lib/wsl/lib`)。`find -xdev` は
  境界のディレクトリ自体を出力し、HyperDU `-x` は除外します。

### 測定中に潰した罠

同じ間違いを繰り返さないための記録です。

- **robocopy が起動していなかった。** Git Bash のパス変換が `/L` を `L:/` に書き換え、
  robocopy は `ERROR : Invalid Parameter #3` で即座に終了していた。その 100 ms 前後を
  「robocopy は速い」と読むと「HyperDU が 5 倍遅い」という逆の結論が出る。correctness
  gate が robocopy のファイル数 0 を示して止めた。`MSYS_NO_PATHCONV=1` が必要。
- **ハーネスが正しい実装を不一致と誤検知していた。** `find` は全リンクを数え、`du` と
  HyperDU はどちらも既定でハードリンクを畳む。畳んだ結果同士は一致するのに、畳まない
  `find` と比べていた。検証側にだけ `--count-links` を付けて解消 (commit `07dc887`)。
- **`stat -f -c %T` が ext4 を "ext2/ext3" と表示していた。** magic number 0xEF53 を
  ext2/ext3 と共有するため。`findmnt` を先に引くようにした (同 commit)。

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
