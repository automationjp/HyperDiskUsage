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
