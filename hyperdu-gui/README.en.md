# hyperdu-gui

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

![HyperDU GUI](../docs/images/gui.png)

`hyperdu-gui` is a desktop GUI for inspecting disk usage through the shared scanner core. Enter a target path or choose a folder, then inspect the directory breakdown as results arrive.

The labels, status messages, and settings in the GUI are Japanese. At startup it searches the operating system's font directories and registers CJK, emoji, UI, and monospace fallbacks, so paths containing Japanese characters can be displayed. Japanese rendering during native startup has been verified.

## Release status

`0.5.0-beta.3` is being prepared for publication. Installing a specific version from the crate registry will be available after publication. For now, install the source checkout from the repository root.

## Install and run

Run these commands from the repository root.

```bash
cargo install --locked --path hyperdu-gui
hyperdu-gui
```

To run from a development checkout:

```bash
cargo run --release -p hyperdu-gui
```

On Linux, the X11 or Wayland development libraries used by `eframe` / `winit` are required. Windows requires a normal desktop session. macOS is not currently a GUI release target. See [setup](../docs/en/setup.md) for Rust versions, OS dependencies, and build steps.

## UI and scan flow

1. Enter a path in **Target folder**, or choose a folder with **Select…**
2. Choose a **Scan mode**
3. Change the detailed settings if needed
4. Press **Start scan**
5. Inspect the directory tree on the left and the table on the right

Settings apply to the next scan. While a scan is running, the target and settings are disabled. The **Cancel** button requests cooperative cancellation.

### Interactive and Batch

`Interactive` is the default. The core first lists the entries directly under the root and reports the aggregate of direct files together with pending child folders. It then scans one child folder at a time with all workers and updates the GUI as each child completes. Completed folders remain available while the rest of the scan continues.

`Batch` receives one complete result map. It is intended to show the whole result after completion rather than display per-child results while the scan is running.

When the Windows MFT path succeeds during an interactive scan, the GUI indicates that it is receiving one batch result instead of the usual per-child results.

## Detailed settings

| Setting | Behavior |
|---|---|
| Name/path contains | Literal contains filter. Enter one pattern per line; surrounding whitespace and empty lines are ignored |
| glob | Glob filter. Enter one pattern per line; surrounding whitespace and empty lines are ignored |
| Regular expression | Regex filter. Enter one pattern per line; surrounding whitespace and empty lines are ignored. An invalid regex produces an error when the scan starts |
| Minimum file size | Enter an integer with `B / KB / KiB / MB / MiB / GB / GiB`. KB/MB/GB use a 1000 multiplier; KiB/MiB/GiB use 1024. Spaces are ignored and case does not matter |
| Maximum depth | `0` means unlimited. A positive value is relative to the original scan root |
| Follow links | Follows symlinks and enables link-cycle detection |
| Count hardlinks separately | Disabled by default, so hardlinks are deduplicated like GNU `du`; enabled, each hardlink is counted separately |
| Same filesystem only | Keeps the scan within the filesystem boundary of the scan root |

### Size calculation

- **Physical + logical** (default): computes allocation/physical size and logical file size.
- **Logical only**: skips physical-size computation and displays logical size as the substitute in the physical column.
- **Approximate (faster)**: skips physical-size computation and estimates regular-file sizes to reduce metadata I/O. When the minimum size is 0, the current regular-file fast path uses 4 KiB (4096 bytes) as the estimate and treats directory entries as 0. It does not prioritize exact file-size results, and the GUI marks the result as approximate. The physical column is also a substitute.

In logical-only and approximate modes, do not interpret the physical column as actual allocation size.

### Speed and I/O

| Setting | Behavior |
|---|---|
| Thread count | `0` uses the core's default thread count. A positive value requests that many workers. Gentle limits the effective count to at most two |
| I/O Standard | **Balanced**: preserves correct results without deliberately warming the page cache |
| I/O Throughput | **Throughput**: uses as much I/O as the hardware can provide |
| I/O Gentle | **Gentle**: stays out of the way of other work by disabling readahead and reducing workers and large-directory splitting |
| Prefetch Auto | Lets the I/O profile decide |
| Prefetch Enabled / Disabled | Explicitly turns readahead on or off. Readahead can reduce latency but increases total bytes read, so it is off unless requested |
| Large-directory yield interval | Splits a large directory and yields every `N` entries. `0` disables it; the GUI accepts `0..=1,000,000` |

### Windows MFT

**Try direct NTFS MFT scan** is an opt-in Windows setting. It requires administrator privileges, NTFS, a volume root, and a whole-volume scan. If the MFT layout cannot be parsed safely and completely, the process is not elevated, the volume is not NTFS, or the target is not the whole volume, HyperDU falls back to ordinary directory enumeration instead of returning a partial result.

The MFT path reports the volume's own view rather than the view a user sees through the filesystem, so it is never enabled by default. When it succeeds, the interactive GUI displays one batch result.

## Result display

### Left tree

Each expanded directory shows at most 500 children, and recursive rendering is capped at 1,500 rows for the whole view. Deeper directories remain accessible through the table. These are rendering limits; they do not remove scan results.

### Right table

The table shows **all** children of the selected directory. Sort by physical size, logical size, file count, or name. The 500-per-directory and 1,500-row tree limits do not apply to this table.

Files directly under the root are shown as **direct file total**. The GUI does not create a virtual path that could be confused with a directory result.

Each row displays `physical / logical`. In logical-only and approximate modes, the physical value is the substitute described above.

## Status, errors, and cancellation

- While scanning, pending child folders show a spinner, and the status displays files seen, remaining folders, and the error count.
- Read errors increment the error count; details are retained and displayed for at most 20 messages.
- If the core emits its completion event with errors, the GUI shows the result but marks it as partial and does not treat it as complete.
- **Cancel** requests cooperative cancellation. Completed-folder partial results remain viewable, but a cancelled scan is not reported as complete.
- A missing root, invalid pattern or size, or scan-thread failure is shown as an error state.

The JSON and CSV save buttons are enabled only when the scan has emitted `Finished`, the error count is zero, and no scan is running. Partial, cancelled, and failed results cannot be exported.

## Normal CLI / MCP

The GUI is a visual and interactive front-end. For scripts, CI, structured output, or AI agents, use the normal `hyperdu` CLI and `hyperdu mcp`. See the [hyperdu CLI / MCP README](../hyperdu/README.en.md) for installation and MCP registration.

## License

MIT. Fonts bundled through the egui dependency chain carry their own OFL-1.1 and Ubuntu-font-1.0 licenses.
