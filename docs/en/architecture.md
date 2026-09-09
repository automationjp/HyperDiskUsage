# Architecture

[日本語](../architecture.md) | [English](../en/architecture.md) | [简体中文](../zh-CN/architecture.md)

HyperDU has **one fast scanner core with CLI / GUI / MCP layers on top**.

By concentrating performance-sensitive processing in `hyperdu-core` instead of reimplementing it for each interface, the CLI, GUI, and AI agent share the same scan semantics and platform fast paths.

## Overview

```text
       hyperdu CLI        hyperdu mcp        hyperdu-gui
            \                  |                  /
             +-----------------+-----------------+
                               |
                         hyperdu-core
                    scan / model / domain
                         /     |     \
                      Linux Windows macOS
```

## Components

| Component | Responsibility | Performance role |
|---|---|---|
| `hyperdu-core` | Tree scanning, aggregation, and platform abstraction | Center of the hot path |
| `hyperdu` | Command parsing, display, and export | Thin interface that calls the scanner |
| `hyperdu-gui` | Desktop UI and drill-down | Uses the same core scanner |
| `hyperdu mcp` (subcommand inside the CLI) | Structured MCP tools | Exposes the same core scanner to agents |
| `plugin/` | Agent Skill / Plugin | Provides CLI / MCP usage procedures |
| `scripts/bench/` | Benchmark harness | Ensures reproducibility of performance claims |

## Scan data flow

A normal scan follows this general flow:

1. The CLI / GUI / MCP builds scan options
2. `hyperdu-core` determines the target filesystem and platform
3. It selects the platform-specific enumeration path
4. It submits directory work to the worker queue
5. Workers process subtrees and use work stealing when needed
6. They aggregate logical and physical usage for files and directories
7. They handle duplicates such as hardlinks
8. They roll subtree totals up to the parent
9. The interface converts the result to human-readable, JSON, CSV, or structured MCP output

## Interactive mode

`hyperdu-core::scan_directory_mode` provides `ScanMode::Interactive` and `ScanMode::Batch`. In GUI interactive mode, HyperDU first enumerates the entries directly under the root, then scans child folders one at a time with all workers and reports results. Completed folders remain viewable while the remaining scan continues.

Options are prepared once from the original root, and hardlink/link-cycle detection state, filesystem boundaries, progress, error counts, and cancellation are shared across all phases. Because child-folder depth starts at 1, depth limits are also relative to the original root. The GUI does not contain separate implementations for file enumeration or duplicate detection.

Files directly under the root are reported as the aggregate total after the core applies its filters. HyperDU does not create virtual paths that could be confused with directory results. When a direct MFT scan succeeds, an event explicitly indicates that the result is batched. A cancelled child folder is never reported as completed.

The total for hardlinks is consistent in both modes, but which folder receives the size of a duplicated name depends on scan order. The results of a batch scan and a per-child-folder breakdown therefore do not always match in every detail.

## Platform boundary

### Linux

```text
Directory
   |
   +--> getdents64  -- names / inode / type --> worker
                                             |
                                             +--> statx --> size metadata
```

On Linux, directory enumeration and metadata retrieval are separate. Since metadata syscalls are necessary for exact sizes, enumeration efficiency, parallelism, and filesystem strategy are important.

### Windows

```text
Directory
   |
   +--> NtQueryDirectoryFile
          |
          +--> name
          +--> file size
          +--> allocation size
          +--> file ID
```

On Windows, one batch enumeration obtains sizes and file IDs as well, so reducing additional handle opens is central to the speedup.

### Optional MFT path

```text
--mft + supported NTFS volume root
              |
              v
        read / parse $MFT
              |
       complete + valid ?
          /          \
        yes           no
         |             |
         v             v
      result      directory enumeration
```

The MFT backend is an optional fast path. It does not force partial aggregation for a layout it cannot parse; it falls back to the normal path.

## Concurrency model

With simple fixed-range partitioning, size imbalance in the directory tree increases worker idle time.

HyperDU gives each worker a LIFO deque and lets other workers steal work.

The goals are:

- Keep locality while exploring the current subtree deeply
- Let idle workers take over large unprocessed subtrees
- Avoid mistaking a queue's momentary empty state for scan completion

The goal is to follow **tree-shape imbalance**, rather than simply increasing the number of threads.

## Correctness before speed

HyperDU's fast paths assume that result semantics do not change.

The following are not omitted for performance optimization:

- Hardlink accounting
- The distinction between logical and physical size
- Fallback when a fast path is unsupported
- Rejection of an incomplete MFT parse
- Handling of scan errors and cancellation

When publishing speed values, confirm scan parity first. See the [Benchmark plan](benchmarks.md) for the procedure.

## Persisted Linux snapshots

`index refresh` / `index show` is a separate feature that reuses an explicitly saved directory aggregate; it does not replace a normal scan.

It is not an automatic watcher, and the result of `show` is explicitly marked `stale`. See [Linux directory snapshots](index-snapshots.md) for details.

## Historical design records

Historical issue-specific designs, MFT verification records, and the old persistent-index proposal have moved to [old/README.md](../old/README.md).

Use this document and [Performance design](performance.md) as the entry points for understanding the current architecture.
