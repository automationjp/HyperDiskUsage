# hyperdu

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

A disk-usage analyzer combining native metadata reads and parallel traversal. **One command provides the CLI and read-only MCP server**, reusing the same `hyperdu-core` as the GUI and Rust applications.

## Install

This README ships with `0.5.0-beta.6`. [Website](https://hyperdu.automation.jp/) · [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases)

```bash
cargo install hyperdu --locked --version 0.5.0-beta.6
```

Both the crate and command are `hyperdu`, not the retired `hyperdu-cli`. MCP starts only when you run `hyperdu mcp`. Source builds require Rust 1.88 or later and platform build tools; prebuilt binaries do not require Rust to run. [Platform setup](../docs/en/setup.md)

To install a development checkout instead, run `cargo install --locked --path hyperdu` from the repository root. This is separate from installing the published release.

## Usage

```bash
hyperdu /path --top 20
hyperdu /path --json out.json
hyperdu /path --csv out.csv
hyperdu --compat gnu -k /var/log
hyperdu mcp
hyperdu -- mcp
```

These examples show top directories, JSON / CSV export, GNU-compatible output, starting MCP, and scanning a directory literally named `mcp`. Normal `--top` ranking uses physical size. See the [CLI reference (Japanese)](../docs/cli-reference.md) for defaults, platforms, and progress / output destinations. Time options require the default-enabled `time-format` feature.


### While it runs, and after

During the scan only the last two terminal lines change, every 20 ms: the completed share, files processed, rate and elapsed time, and a file being scanned.

```text
progress:  42% | 612880 files | 201865 f/s | 3.0s
scan: photo_0412.jpg (2.31 MiB)
```

When it finishes those two lines are erased and the result, largest physical size first, remains (example paths):

```text
Top 3 under D:\data (physical desc):
  1. D:\data | phys=204.39 GiB | log=201.76 GiB | files=1641117
  2. D:\data\videos | phys=72.24 GiB | log=71.81 GiB | files=247520
  3. D:\data\photos | phys=32.95 GiB | log=32.44 GiB | files=360626
```

## Mechanism and boundaries

Windows `NtQueryDirectoryFile` batches names, allocation sizes, file IDs, and other metadata to reduce additional file opens. Linux uses `getdents64` plus `statx` and processes required metadata reads in parallel. Per-worker LIFO queues and work stealing distribute work across uneven trees. The macOS `getattrlistbulk` implementation is not currently covered by CI or release builds.

`--mft` is an experimental, conditional Windows NTFS path. Incomplete required reads or parsing fall back to normal enumeration; successful parsing does not guarantee complete accounting parity. HyperDU also does not implement every GNU / POSIX `du` option. [Comparison and limitations](../README.en.md)

MCP tools `list_volumes`, `scan_path`, and `find_reclaimable` are read-only and never delete files. The Skill also works through the CLI alone. [Agent setup](../plugin/README.en.md)

## Developer documentation

[Developer guide (Japanese)](../docs/developer-guide.md) · [Performance design](../docs/en/performance.md) · [Measurements and limitations](../docs/en/benchmarks.md) · [GUI](../hyperdu-gui/README.en.md)

Keep the measured benchmark commit separate from the published release and current HEAD.

## License

MIT
