# 性能设计

[日本語](../performance.md) | [English](../en/performance.md) | [简体中文](../zh-CN/performance.md)

HyperDU 的性能设计核心是：**在正确汇总磁盘使用量的同时，减少扫描所需的 OS 调用和等待时间**。

本文不是说明“快多少倍”的 benchmark report。倍率及测量条件见[基准测试](benchmarks.md)。本文说明实现在哪些位置进行了加速。

## 结论

性能设计主要有以下 5 个要点：

1. **使用 OS 专用的高速枚举 API**
2. **减少每个文件额外的 syscall / handle open**
3. **并行处理子树，并通过 work stealing 吸收不均衡**
4. **针对不同 filesystem 避免不利的处理**
5. **仅在可用时使用更快的专用路径，并安全地 fallback**

## Platform fast paths

| Platform | Main path | 目标 |
|---|---|---|
| Linux | `getdents64` + `statx` | 批量读取 directory entry，降低 metadata 获取的开销 |
| Windows | `NtQueryDirectoryFile` + `FileIdFullDirectoryInformation` | 批量获取名称、大小、allocation size 和 file ID |
| macOS | `getattrlistbulk` | 批量获取 directory metadata |

### Linux

Linux 的 directory entry 不包含文件大小，因此精确汇总大小需要获取每个文件的 metadata。HyperDU 使用 `getdents64` 进行 directory enumeration，使用 `statx` 获取 metadata。

因此 Linux 的重点不是“完全消除 syscall”，而是**降低枚举开销，并高效地并行获取 metadata**。

HyperDU 会根据 filesystem 调整 buffer、prefetch、physical-size 的处理方式。对于 network filesystem 或 DrvFS，与本地 ext4/XFS 相同的策略并不总是最优。

### Windows

Windows 的标准路径使用 `NtQueryDirectoryFile`，并从 `FileIdFullDirectoryInformation` 中同时取得 directory entry、allocation size 和 file ID。

这样做的主要优势是：为了获取 physical size 和进行 hardlink 去重，**减少了逐个 open 文件的需要**。

设置环境变量 `HYPERDU_WIN_USE_NTQUERY=0` 可以切换到 `FindFirstFileExW` 路径。在诊断无法使用 fast path 的情况时，也可以用它进行对比。

### 可选的 NTFS `$MFT` 路径

在Windows MSVC上，`--mft`需要管理员权限和NTFS卷根。排除条件、深度／最小大小、追踪链接或单独计算硬链接等不支持的设置会转为普通枚举。参见[适用条件](../cli-reference.md)。

该路径并非始终启用。如果无法安全解析所需的 DATA extent，或不满足条件，HyperDU 会 fallback 到普通 directory enumeration。

无法完成解析时拒绝结果；解析完成不保证与枚举完全一致，MFT仍为实验功能。

## Parallel traversal

核心 scanner 使用每个 worker 的 LIFO deque 和 work stealing 处理 directory tree。

关键并不是简单增加 thread 数量。由于不同子树的 directory tree 大小不同，固定分割会使一部分 worker 提前空闲。

HyperDU 允许其他 worker 窃取尚未处理的 work，即使处理集中在大型 subtree 中，也能分散 CPU 和 I/O 等待。

此外，HyperDU 会追踪 in-flight work，避免仅因 queue 暂时为空就让所有 worker 退出。

## 哪些情况会缩小差距

加速效果取决于 workload。在以下条件下，差距尤其可能缩小：

- 文件数量少
- tree 深而窄
- HDD
- NFS / SMB / SSHFS / 9p / FUSE 等 network・virtual filesystem
- storage latency 占主导地位
- 由于权限或 filesystem 条件无法使用 fast path
- cold cache 和 warm cache 的主导因素不同

因此，README 不会将单一倍率作为性能依据。

## 性能声明状态

当前Linux benchmark使用GitHub Actions Ubuntu 24.04/ext4，1M个256 B普通文件分为三种结构，每个工具交替warm运行8次（共48个raw sample），所有目录行的物理分配字节直接一致。GNU du / HyperDU中位数倍率为平铺1.187x、宽目录3.256x、深目录3.183x；保守的3.25x标题值仅适用于Linux。Windows 11补充测量使用本地NTFS/NVMe和`--apparent-size`；由于GNU du在600秒warmup中超时，1M flat只有HyperDU单独运行的589.91 ms中位数，没有受理的1M比较或倍率。单独的10K倍率仅为诊断结果。旧WSL2速度不作为当前证据。参见[benchmark结果与方法](benchmarks.md)。

## 发布速度声明前

在 README / Web site 中发布性能值前，至少满足以下条件：

- [ ] 固定被测量的 commit
- [ ] 使用 release build 测量
- [ ] 确认 HyperDU 和比较对象扫描的数据量一致
- [ ] 记录 comparator 的名称和 version
- [ ] 记录 filesystem、kernel、CPU、storage
- [ ] 不要混淆 warm / cold
- [ ] cold 测量要避免执行顺序造成的偏差
- [ ] 不要只用单次最佳值声称性能
- [ ] 保留不利结果
- [ ] 以便之后重新验证的形式保存 raw result

具体的测量条件和来源链接请参见 [benchmark结果与方法](benchmarks.md)。
