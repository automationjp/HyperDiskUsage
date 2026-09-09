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

### GUI background work and display

A dedicated `hyperdu-gui-scan` thread calls the core and prepares the directory index, cached totals, and four sort orders. Result ingestion and display-order changes do not move file enumeration or a full-result sort onto the UI thread.

```text
core ScanEvent -> background index / sorting
                       |
                node chunks (256)
                       |
                 bounded queue (2)
                       |
           UI ingestion budget -> tree / visible table rows

core progress counter -----------------> status / elapsed time
```

Node updates contain at most 256 nodes, and the queue holds two messages. The UI ingests at most 1,024 units of update work per frame, with a 3 ms time budget, and defers remaining work to later frames. Time is checked cooperatively between updates; a single allocation or destruction of an old model is not guaranteed to finish within 3 ms. The worker also retains a full index, so total memory is not bounded by the queue capacity alone.

A parent's sorted index is published after its referenced nodes have been sent. This avoids references to missing IDs, but does not guarantee that table rows appear with the first chunk. The table renders visible rows, and tree rendering has depth and row budgets. Deeper directories remain accessible through the table and breadcrumbs.

Starting a scan immediately sets a working state. A shared counter updates progress and elapsed time independently of the result queue. Incoming events request repainting, with periodic refreshes while scanning. Cancellation reaches the core and index preparation through a shared flag; partial results are not labeled complete. It cannot immediately interrupt synchronous I/O or an individual sort already in progress. Dropping the receiver releases a sender blocked on the queue, and replacing the scan handle isolates old results from the next scan.

Terminal events are handled after preceding updates. Read errors, cancellation, and a disconnected sender without a terminal event are distinguished from successful completion. User-triggered JSON/CSV export copies all rows and sorts them by path. This and some other operations, including model replacement, still run synchronously on the UI thread.

Implementation: [GUI transport / index](../../hyperdu-gui/src/scan.rs), [UI ingestion / rendering](../../hyperdu-gui/src/app.rs), and [core modes / events](../../hyperdu-core/src/lib.rs).

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
shared eligibility guard
 Windows MSVC / volume root / administrator / supported options
                         |
                   open NTFS volume
                         |
       extent-limited raw window -> copied record -> fixups / parse
                         |
              parent-record-ID aggregation
                         |
               directory paths / rollup

ineligible or incomplete required read/parse -> directory enumeration
```

MFT is an experimental, optional Windows MSVC path. The CLI, GUI, and direct core API use the same eligibility check. Besides a volume root and administrator privileges, the guard rejects raw/compiled exclusions, depth limits, minimum sizes, link following, separate hardlink counting, approximate sizes, and an externally supplied deduplication cache. Normal scan APIs then use directory enumeration. The direct API returns `None`, allowing validation to distinguish an actual MFT scan from a fallback.

The reader batches sequential MFT records through a raw-byte window whose default limit is 1 MiB. Read-ahead stays inside a physical extent; a record crossing an extent boundary is assembled from the required segments. Fixups are applied to a copied record, so rereads and extension-record lookups cannot mistake already-patched cached bytes for raw data. Failed read-ahead clears the cache and retries the required segment. Incomplete required reads or parsing are not accepted as a result.

Aggregation adds sizes to parent record IDs before constructing directory paths and rolling totals up in the core. Hardlink identities and the handling of missing or cyclic parents retain the existing accounting rules. The read window is bounded, but the reader keeps all entries and aggregate results in memory; this is not a constant-memory scan of the entire MFT.

The reader checks progress and cancellation before iteration, at intervals of at most 256 records, and at a successful end. Record counts include inactive slots and differ from final file counts or completion percentages. Synchronous extension reads cannot be interrupted immediately. A successful MFT scan requested through Interactive mode emits `BatchFallback(Mft)` and `BatchCompleted` to identify batched delivery. This is distinct from falling back to enumeration after MFT failure.

Eligibility and successful parsing of required records do not prove complete parity with enumeration. Known accounting differences remain, so performance validation must report the path actually used and accounting differences separately.

Implementation: [shared eligibility](../../hyperdu-core/src/platform/windows_impl/mod.rs), [reader](../../hyperdu-core/src/platform/windows_impl/mft_reader.rs), [raw window](../../hyperdu-core/src/platform/windows_impl/mft_reader/window.rs), and [ID aggregation](../../hyperdu-core/src/platform/windows_impl/mft_aggregate.rs).

## CLI / MCP progress delivery

The ordinary CLI writes results to stdout and progress/diagnostics to stderr. A terminal on stderr enables the working indicator automatically; use `--progress` for redirected stderr. It reports a working state immediately and wakes the waiting status thread on completion instead of waiting for its refresh interval.

`hyperdu mcp` is an rmcp stdio server. When `scan_path` receives MCP `_meta.progressToken`, it sends standard `notifications/progress` messages with an initial value of zero followed by increasing file counts. No token means no progress notifications. Tokens and cancellation state belong to each request, keeping concurrent calls separate.

The synchronous core scan runs through `spawn_blocking`. Its callback only updates the latest count in a watch channel, so the scanner does not wait for protocol transport. The async sender coalesces updates and throttles intermediate notifications to approximately 250 ms intervals. It sends the final count before the result when that count has not already been sent. Counts are not percentages; the final tool result establishes successful completion.

Request cancellation, request destruction, or a notification transport failure propagates to the core cancellation flag. This is cooperative cancellation, not an immediate interruption of synchronous OS I/O. This progress adapter serves `scan_path`; it does not promise progress for every MCP tool.

Implementation: [CLI](../../hyperdu-cli/src/main.rs), [MCP tools](../../hyperdu-cli/src/mcp.rs), and [MCP progress adapter](../../hyperdu-cli/src/mcp/progress.rs). See the [CLI reference (Japanese)](../cli-reference.md) for argument and output conditions.

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
