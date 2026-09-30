# HyperDU 与 du 比较

成功的测量与独立oracle一致，已完成的比较也与GNU `du`直接一致。不进行外部字节补正，旧WSL2速度不作为当前证据。测量源码为`2645689515ab2e608a78b7492637e55179e4739a`；详细hash、全部raw sample和corpus fingerprint见[公开测定结果JSON](https://automationjp.github.io/HyperDiskUsage/benchmarks.json)。

## Linux结果（GitHub Actions）

GitHub托管运行环境为Ubuntu 24.04/ext4，flat、wide、deep三种结构各包含1,000,000个256 B普通文件。每个工具交替warm运行8次，共48个raw sample。计时包含进程启动和目录输出，所有目录行的物理分配字节均直接一致。

Linux中位数（HyperDU / GNU `du`；GNU `du` / HyperDU倍率）为Flat 2327.49 / 2763.52 ms（1.187x）、Wide 745.96 / 2428.80 ms（3.256x）、Deep 764.67 / 2434.28 ms（3.183x）。

保守的标题值为3.25x（wide，仅Linux）。[GitHub Actions Linux运行](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086)包含对应的源码、构建、corpus和parity证据。

### Linux command

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

两个工具都输出全部目录行；只在比较时规范行顺序，不调整总量或字节值。

## Windows补充结果（本地）

本地环境为Windows 11、NTFS/NVMe、Ryzen 9 3900X（12 cores／24 threads、128 GiB）。GNU `du`来自Git for Windows MSYS，版本为GNU coreutils 8.32。两工具都使用`--apparent-size`进行逻辑字节统计，并采用warm计时。

1M flat仅对HyperDU单独运行8次，中位数为589.91 ms。GNU `du`在warmup阶段达到600秒timeout，因此没有受理的1M比较或1M倍率；wide／deep的1M测试未运行。

另一个10K诊断包含三种结构、每个工具交替运行2次，保留12个measured sample和6个warmup。所有目录行一致，但这些倍率仅适用于本次小规模诊断，不能作为一般1M标题值。

10K诊断中位数（HyperDU / GNU `du`；GNU `du` / HyperDU倍率）为Flat 32.98 / 704.17 ms（21.35x）、Wide 27.46 / 712.33 ms（25.94x）、Deep 32.32 / 836.73 ms（25.89x）。

### Windows command

```powershell
hyperdu --compat gnu-strict --apparent-size --block-size 1 --one-file-system --io-profile balanced ROOT
du -x --apparent-size --block-size=1 ROOT
```

AWS runbook仅作为未执行计划保留，不构成当前结果或速度值。见[AWS runbook（未执行计划）](../benchmarks/aws-protocol.md)。

[可复现的Linux benchmark runner](../../scripts/bench/du_same_conditions.py)


## 与 dua-cli 和 tokei 的比较（Windows 本地）

将由 `d4ebdb85577c53e881af292b549bc784f05bec60`（v0.5.0-beta.5 的开发快照）通过 `cargo build --locked --release -p hyperdu`（rustc 1.98.0）构建的 HyperDU，与 dua-cli 2.45.0、tokei 15.0.0（均用同一 rustc 以 `cargo install --locked` 构建）进行比较。环境为 Windows 11 / NTFS / NVMe，Ryzen 9 3900X（12 核 / 24 线程，128 GiB 内存）。热缓存，交替运行各工具，每个12次，全部原始样本保存在[记录 JSON](../benchmarks/2026-09-30-windows-vs-dua-tokei.json)。用时为进程启动到退出，包含写入标准输出。每个数据集在计时前后都用独立的 lstat 遍历确认内容未变化。

### dua-cli（同一项工作）

HyperDU 与 dua-cli 的逻辑字节总数在全部5个数据集上都与独立遍历一致，未做任何外部字节修正。

| 数据集 | 文件数 | HyperDU | dua-cli | dua / HyperDU |
|---|---:|---:|---:|---:|
| 宽目录树（合成） | 99,856 | 67.57 ms | 187.37 ms | 2.77x |
| 深层目录树（合成） | 100,000 | 179.68 ms | 194.62 ms | 1.08x |
| 平铺目录（合成） | 100,000 | 98.80 ms | 997.51 ms | 10.10x |
| Cargo registry（真实文件） | 107,953 | 478.55 ms | 554.61 ms | 1.16x |
| node_modules（真实文件） | 27,940 | 72.26 ms | 156.60 ms | 2.17x |

HyperDU 在5项中均更快，但差距取决于目录结构：除平铺目录外为 1.08–2.77 倍。

- 深层目录（1.08x）几乎没有可并行的宽度。波动范围重叠（HyperDU 159-264 ms，dua-cli 170-312 ms），应视为基本相当。
- 平铺目录的 10.10x 不能作为代表值。dua-cli 会逐条输出直接子项（100,001 行），该输出时间已计入。`--depth 1` 不会减少输出，且其总数与独立遍历不一致，因此未使用。
- 真实文件：Cargo registry 为 1.16x，node_modules 为 2.17x。

### tokei（参考，不同的工作）

tokei 统计源码行数：它读取文件内容并按语言解析。HyperDU 只汇总大小，不读取内容。两者不是同一项工作，因此**不给出速度比**，时间仅供参考。合成数据没有扩展名，tokei 不会解析任何内容，所以只测量了2个真实目录。

| 数据集 | 文件数 | tokei 解析的文件数 | HyperDU | tokei |
|---|---:|---:|---:|---:|
| Cargo registry（真实文件） | 107,953 | 80,897 | 478.55 ms | 9,407.19 ms |
| node_modules（真实文件） | 27,940 | 16,576 | 72.26 ms | 782.31 ms |

### 局限

- 测量期间，其他项目的任务（densel 的 GC 与 Python 工作进程）占用了 24 个逻辑 CPU 的约60–90%。各工具交替运行以承受相同负载，但绝对值偏大，单次用时波动很大（HyperDU 平铺目录：68-1,096 ms）。这不是隔离的测量主机。
- 未测量冷缓存。tokei 会读取文件内容，不能把热缓存的数值套用到冷缓存。
- 合成目录是没有扩展名的 4 KiB 普通文件，并受 dua-cli 输出方式（列出全部直接子项）的影响。不应推广到所列目录之外。
- 原计划的第3个真实目录（Rust toolchains，345,102 个文件）因 tokei 单次运行超过10分钟而中止并剔除，原因未调查。
- HyperDU 为 origin/main 的 d4ebdb8，不是已发布的 v0.5.0-beta.5 二进制文件。

### 命令

```powershell
hyperdu ROOT --top 1 --exclude "" --one-file-system
dua aggregate -A -x -f bytes ROOT
tokei ROOT --output json
```

[比较用 benchmark runner](../../scripts/bench/compare_tools.py)
