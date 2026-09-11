# Performance design

[日本語](../performance.md) | [English](../en/performance.md) | [简体中文](../zh-CN/performance.md)

HyperDU centers its performance design on **reducing the OS calls and waiting time required for scanning while correctly aggregating disk usage**.

This document is not a benchmark report showing “how many times faster” HyperDU is. See [benchmarks](benchmarks.md) for ratios and their measurement conditions. This document explains where the implementation gets its speedups.

## Conclusion

The five main performance design points are:

1. **Use fast, OS-specific enumeration APIs**
2. **Reduce per-file extra syscalls and handle opens**
3. **Process subtrees in parallel and absorb imbalance with work stealing**
4. **Avoid operations that are unfavorable for a given filesystem**
5. **Use a faster dedicated path only when available, with a safe fallback**

## Platform fast paths

| Platform | Main path | Goal |
|---|---|---|
| Linux | `getdents64` + `statx` | Read directory entries in batches and lower the overhead of metadata retrieval |
| Windows | `NtQueryDirectoryFile` + `FileIdFullDirectoryInformation` | Retrieve names, sizes, allocation size, and file IDs in batches |
| macOS | `getattrlistbulk` | Retrieve directory metadata in bulk |

### Linux

Linux directory entries do not include file sizes, so exact size aggregation requires metadata for each file. HyperDU uses `getdents64` for directory enumeration and `statx` for metadata retrieval.

Therefore, Linux is not about “eliminating syscalls completely.” The focus is to **reduce enumeration overhead and parallelize metadata retrieval efficiently**.

Buffering, prefetching, and physical-size handling vary by filesystem. On network filesystems and DrvFS, the same strategy is not always optimal as on local ext4/XFS.

### Windows

The standard Windows path uses `NtQueryDirectoryFile` and obtains directory entries together with allocation size and file ID from `FileIdFullDirectoryInformation`.

The major benefit is that it **reduces the need to open each file separately** for physical-size retrieval and hardlink deduplication.

Set the environment variable `HYPERDU_WIN_USE_NTQUERY=0` to switch to the `FindFirstFileExW` path. This is also useful for comparison when diagnosing situations where the fast path cannot be used.

### Optional NTFS `$MFT` path

On Windows MSVC, `--mft` requests direct reading at an NTFS volume root with administrator privileges. Unsupported filters, depth/minimum-size settings, link following or separate hardlink counting select enumeration instead. See the [eligibility conditions](../cli-reference.md).

This path is not always enabled. If the required DATA extents cannot be resolved safely, or if the conditions are not met, HyperDU falls back to ordinary directory enumeration.

Incomplete parsing is rejected. Completed parsing does not guarantee exact equality with enumeration; MFT remains experimental.

## Parallel traversal

The core scanner processes the directory tree with a per-worker LIFO deque and work stealing.

The important point is not simply increasing the number of threads. Directory trees differ in size by subtree, so fixed partitioning leaves some workers idle early.

HyperDU allows other workers to steal unprocessed work, distributing CPU and I/O waits even when processing is concentrated in a large subtree.

It also tracks in-flight work so that all workers do not exit merely because a queue is temporarily empty.

## What can make the gap smaller

The effect of optimization depends on the workload. The gap may be smaller especially under these conditions:

- Few files
- A deep, narrow tree
- HDD
- NFS / SMB / SSHFS / 9p / FUSE and other network or virtual filesystems
- Storage latency is the dominant factor
- The fast path cannot be used because of permission or filesystem conditions
- The dominant factor changes between cold and warm caches

For this reason, the README does not use a single ratio as the performance basis.

## Performance claim status

The current Linux benchmark used GitHub Actions Ubuntu 24.04/ext4 with 1M regular 256 B files in three shapes, eight alternating warm runs per tool (48 raw samples), and direct allocated-byte parity for every directory row. Median GNU du / HyperDU ratios were 1.187x flat, 3.256x wide and 3.183x deep; the conservative 3.25x headline is Linux only. Windows 11 supplemental measurements used local NTFS/NVMe and `--apparent-size`; the 1M flat HyperDU-only median was 589.91 ms because GNU du timed out during its 600-second warmup, so there is no accepted 1M comparison or ratio. The separate 10K ratios are diagnostics only. WSL2 timings are not current speed evidence. See the [benchmark results and method](benchmarks.md).

## Before publishing a speed claim

Before putting performance values in the README or website, satisfy at least the following:

- [ ] Pin the commit being measured
- [ ] Measure a release build
- [ ] Confirm that HyperDU and the comparison target scan the same amount of data
- [ ] Record the comparator name and version
- [ ] Record the filesystem, kernel, CPU, and storage
- [ ] Do not mix up warm and cold runs
- [ ] Avoid order bias in cold runs
- [ ] Do not claim performance from only a single best result
- [ ] Keep unfavorable results
- [ ] Store raw results in a form that can be rechecked later

See the [benchmark results and method](benchmarks.md) for the concrete measurement conditions and provenance links.
