# 基准测试

[日本語](../benchmarks.md) | [English](../en/benchmarks.md) | **简体中文**

本文定义当前可复现的 warm benchmark 以及证据边界。被测 source 固定为 commit `0a089c90c0e2995ef702ff053820d129265c3659`。结果表和全部试验值由已完成的 JSON 报告生成。

## 范围与状态

本 benchmark 覆盖 **Windows / NTFS** 和 **Linux / WSL2 / ext4**。初次 correctness run warm cache 后，每个 dataset **严格运行8次**，交替改变两个工具的先后顺序。记录的是包含进程启动和输出在内的进程时间中位数。倍率为 `baseline / HyperDU`；某一行倍率低于 1 表示该行 HyperDU 更慢。

比较工具为 Windows 的 `robocopy` 和 Linux 的 **uutils `du` 0.8.0**。Linux 比较对象不是 GNU `du`，也不是 bare-metal Linux 测量。

## 环境

- **Windows：** Windows 11 Pro 10.0.26200，AMD Ryzen 9 3900X（12C/24T），128 GiB RAM。synthetic fixture 位于 F: 的 WD_BLACK SN850X HS 4 TB，registry tree 位于 C: 的 GIGABYTE GP-ASM2NE6100TTTD；两者均为 NTFS/NVMe。`robocopy` 版本为 10.0.26100.1 (WinBuild.160101.0800)。
- **Linux：** 同一台 PC 上的 WSL2，Ubuntu 26.04，kernel 6.18.33.2-microsoft-standard-WSL2，16 vCPU，约90 GiB RAM，ext4。比较对象为 uutils `du` 0.8.0。

release build 和 source provenance 单独记录在 [build record](../benchmarks/build-record.md) 中。

## 结果

### Windows

`hyperdu 0.5.0-beta.3` / SHA-256 `b4c4c4ca78119d61319c9b0a342ab7b7e7dfed3992a44fe56d88ddb5b6a9c3d8`

Comparator: `10.0.26100.1 (WinBuild.160101.0800)`

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 30.721 | 57.424 | 1.87x |
| deep | 2,000 | 77.123 | 72.041 | 0.93x |
| flat | 20,000 | 34.081 | 42.825 | 1.26x |
| registry | 111,813 | 332.236 | 1838.460 | 5.53x |

```text
wide hyperdu 34.665 30.334 33.340 29.218 31.107 30.203 29.661 32.721
wide baseline 60.855 55.954 58.894 62.127 53.976 65.117 55.036 55.126
deep hyperdu 80.579 82.515 92.383 76.418 75.246 72.922 77.827 75.048
deep baseline 72.060 81.065 76.229 72.021 79.658 71.715 69.163 71.954
flat hyperdu 35.492 29.856 33.223 30.974 35.251 34.938 44.015 31.479
flat baseline 42.960 42.689 43.561 50.821 37.715 39.821 73.456 41.414
registry hyperdu 329.521 321.446 325.137 330.354 334.119 356.102 343.325 514.561
registry baseline 1691.376 1823.197 1937.345 1853.723 1592.403 1687.168 2615.618 2950.394
```

[JSON: commands, gates, raw samples](../benchmarks/2026-09-09-windows.json)

### Linux

`hyperdu 0.5.0-beta.3` / SHA-256 `a43fff5f5b7a7663b15f081c1361256ef111461848f1fe49419b0e829bd32eb4`

Comparator: `du (uutils coreutils) 0.8.0`

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 47.343 | 208.596 | 4.41x |
| deep | 2,000 | 89.107 | 143.359 | 1.61x |
| flat | 20,000 | 130.465 | 184.377 | 1.41x |
| registry | 37,814 | 144.584 | 978.928 | 6.77x |

```text
wide hyperdu 24.960 19.303 27.865 57.863 83.760 57.849 77.518 36.838
wide baseline 219.142 186.493 265.028 186.324 211.074 259.606 206.119 145.009
deep hyperdu 88.301 82.276 80.782 89.913 103.993 111.027 110.154 84.604
deep baseline 161.231 157.352 143.158 148.519 141.402 114.486 143.561 133.878
flat hyperdu 118.474 105.591 140.027 130.245 151.339 181.407 89.872 130.685
flat baseline 173.565 138.248 179.544 193.533 189.211 237.937 189.578 157.039
registry hyperdu 115.010 142.254 146.913 107.350 150.306 153.823 178.075 112.644
registry baseline 944.617 947.984 908.689 1558.686 1043.559 1187.297 1009.872 839.885
```

[JSON: commands, gates, raw samples](../benchmarks/2026-09-09-linux.json)


## 方法

`scripts/bench/remeasure.py` 会创建三种 synthetic workload：

- **wide：** 500 个目录 × 每个40个文件
- **deep：** 400 层目录 × 每层5个文件
- **flat：** 一个目录中包含20,000个文件

每个 synthetic file 包含 4,096 bytes。也可以传入额外的 Cargo registry tree，但它是各 OS 不同的 dataset，不用于跨 OS 速度比较。

harness 对 HyperDU 使用：

```text
hyperdu <root> --top 1 --exclude "" --one-file-system
```

Windows baseline 使用带 `/L /S /XJ /BYTES` 的 `robocopy`，并追加 no-copy、no-header、no-progress 和 zero-retry 选项。Linux baseline 为：

```text
du -sx --block-size=1 <root>
```

第一次 gate pass 用于 warm cache。随后8次计时交替执行顺序：一次 HyperDU 先执行，下一次 baseline 先执行，两个顺序各执行4次。harness 不会 drop caches，也不会删除 dataset。

## Correctness 与稳定性 gate

计时前，harness 在同一 device 上运行独立的 Python `lstat` 遍历。它按 device/inode identity 折叠 hardlink，并将 directory count、file count 和 logical bytes 与 HyperDU 对照。Linux 还会对照 physical bytes。在检查 allocated total 时，`du` 的比较值会包含 directory 和 link 的 allocation bytes。

Windows 的 physical-size validation **未执行**。Windows gate 只将 file 和 logical-byte workload 与 `robocopy` 对照，physical 列仍未验证。

计时后，harness 会确认：

- 独立 oracle 仍描述同一个 dataset；
- HyperDU 的 JSON totals 没有变化；
- release binary 的 SHA-256 没有变化；
- 输出 JSON 达到 `complete: true`。

上方链接的 build record 必须证明 binary 是从本文所列 source commit 构建的。

## 限制

- 尚未测量 cold-cache 性能。
- 尚未测量 XFS。
- 尚未测量 macOS。
- 尚未测量 NTFS MFT 直接路径。
- 尚未测量 GUI rendering 和 GUI Interactive mode 性能。本文是 CLI scan timing。
- Windows physical-size parity 未验证。
- `robocopy` 测量 native enumeration/copy-plan 操作，而 `du` 报告 allocated bytes。gate 中 totals 一致不表示两个工具实现了相同的完整功能集合。
- error handling、links、compression、sparse files 和其他特殊情况需要单独进行语义验证；本速度报告不对这些情况作保证。
- host background work 和 WSL2 行为会影响结果。不要将差异较小的结果泛化。

不要脱离产生该结果的 workload、comparator、cache state、build provenance 和 correctness gate，发布单一速度倍率。

## 复现

使用 lockfile 从固定 source 进行 release build：

```powershell
cargo build --release --locked -p hyperdu
```

运行8次交替 warm trials：

```powershell
python scripts/bench/remeasure.py --bin <release-binary> --commit 0a089c90c0e2995ef702ff053820d129265c3659 --output <results.json> --dataset-parent <scratch> --runs 8
```

使用 `--synthetic-root <directory>` 复用已有的 `wide`、`deep` 和 `flat` fixture。使用 `--tree <directory>` 添加额外 real tree。检查 JSON，只有在 `complete: true` 时继续；raw samples 与 [build record](../benchmarks/build-record.md) 一起保留。

## 相关文档

- [性能设计](performance.md)
- [环境配置](setup.md)
- [架构](architecture.md)
- [Linux directory snapshots](index-snapshots.md)

测量在共享开发电脑上进行；其他任务（包括编译）可能影响耗时，未隔离后台负载。
