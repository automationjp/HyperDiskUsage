# HyperDU

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

> **Find what fills your disk — with one engine for people, applications, and agents.**
> A Rust disk-usage analyzer combining platform-specific metadata reads with parallel traversal. Inspect from the CLI, explore in the GUI, or pass structured results to an AI agent through MCP.

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

[Website](https://hyperdu.automation.jp/en/) · [Setup](docs/en/setup.md) · [Developer guide (Japanese)](docs/developer-guide.md) · [CLI reference (Japanese)](docs/cli-reference.md) · [Documentation](docs/en/README.md)

## Use the published beta

**v0.5.0-beta.5 is published**, both on [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5) and crates.io. It is a beta, not an unreleased preview and not a stable release: CLI options, MCP schemas, and output formats may still change.

```bash
cargo install hyperdu --locked --version 0.5.0-beta.5
hyperdu . --top 20
```

The crate and executable are both **`hyperdu`**, not the retired name `hyperdu-cli`. One installation provides the CLI and the MCP server; the server starts only when you run `hyperdu mcp`.

**Prebuilt Windows / Linux binaries do not require Rust to run.** For source builds, use a recent stable Rust and platform build tools. Current source declares Rust 1.82 for core, 1.85 for GUI, and 1.88 for CLI, with Linux / Windows CI at each floor. Immutable published beta manifests are not rewritten by this source fix; use a recent stable Rust when installing the published package.

## What changes compared with a conventional implementation?

The difference is not simply “written in Rust.” HyperDU combines **how metadata is acquired, how traversal work is shared, and how results are reused**.

| Problem | Implementation | Practical difference |
|---|---|---|
| Listing names and then querying each file can multiply OS calls | Windows `NtQueryDirectoryFile` batches names, sizes, allocation sizes, and file IDs | Fewer additional file opens. Linux uses `getdents64` plus `statx`, but still needs metadata reads |
| Fixed work allocation leaves one worker processing a large subtree | Per-worker LIFO queues with work stealing | Idle workers take available work from an uneven directory tree |
| Separate CLI, GUI, and agent scanners drift in accounting behavior | All consume the independent `hyperdu-core` library | Shared hard-link accounting, aggregation, and platform optimizations |
| Parsing human-readable shell output creates a weak integration contract | Built-in MCP tools with typed arguments and structured results | Programmatic inspection without a deletion tool |
| A GUI that waits for the entire tree provides no early exploration | Interactive mode delivers completed child folders | Explore completed results during the remaining scan; this is not continuous monitoring |

This compares implementation patterns, not every competing product. Other tools may also batch or parallelize. Speed claims below apply only to the measured workloads.

See the [developer guide](docs/developer-guide.md) and [architecture](docs/en/architecture.md) for code entry points and tradeoffs.

## CLI

```bash
hyperdu /path/to/data --top 20
hyperdu /path/to/data --json usage.json
hyperdu /path/to/data --csv usage.csv
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -b --time /usr/share
hyperdu --help
```

Normal top output ranks **directories by physical size**, including when `--apparent-size` is present. Time options require the default-enabled `time-format` feature. Exclusions, display depth, scan limits, and link following are distinct controls; consult the [CLI reference](docs/cli-reference.md).

**Not a complete GNU / POSIX `du` replacement.** `--compat posix-strict` selects defaults such as 512-byte units, but required POSIX options `-a`, `-s`, `-H`, and `-L` are not implemented. Do not unconditionally alias `du` to HyperDU. [Compatibility audit](docs/posix-compatibility.md)

## GUI

```bash
cargo install hyperdu-gui --locked --version 0.5.0-beta.5
hyperdu-gui
```

The Windows / Linux desktop app uses `egui` / `eframe`. Interactive mode delivers child-folder results progressively; Batch mode receives a complete map. Features include a directory tree, breadcrumbs, sortable rows, filters, progress, error and cancellation states, and JSON / CSV export. UI labels are Japanese.

Approximate mode does not promise exact allocation sizes. Cancellation and read errors are not reported as successful completion. [GUI behavior and limitations](hyperdu-gui/README.en.md)

## AI agents

```bash
hyperdu mcp
# Client registration examples:
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

| Tool | Purpose |
|---|---|
| `list_volumes` | Inspect volume capacity and free space |
| `scan_path` | Inspect usage under a path |
| `find_reclaimable` | Find outputs that might be regenerated and reviewed for cleanup |

**All three tools are read-only and never delete files.** A cleanup candidate is not proof that deletion is safe; a person decides. The MCP server, CLI-based Agent Skill, and distributing Plugin are independent entry points. The Skill does not require MCP. [Agent setup](plugin/README.en.md)

## Measured performance, with boundaries

These are previously published measurements, **not a fresh benchmark of the current HEAD or every environment**. The measured source is `2645689515ab2e608a78b7492637e55179e4739a`, separate from the published package version.

### Linux: one million files per shape

GitHub-hosted Ubuntu 24.04 / ext4, AMD EPYC 9V74 (4 vCPU, 15.6 GiB RAM), 1,000,000 regular 256-byte files per shape. Warm cache, allocated bytes, medians of eight alternating runs per tool. Successful directory rows matched an independent oracle.

| Shape | HyperDU | GNU `du` | du / HyperDU |
|---|---:|---:|---:|
| flat | 2327.49 ms | 2763.52 ms | 1.187× |
| wide | 745.96 ms | 2428.80 ms | 3.256× |
| deep | 764.67 ms | 2434.28 ms | 3.183× |

“Up to 3.25× faster” is the headline for this Linux measurement, not a claim about cold caches, other filesystems, HDDs, or network storage.

### Windows: standalone 1M result and 10K diagnostic

Windows 11 / NTFS / NVMe, Ryzen 9 3900X (12 cores / 24 threads, 128 GiB RAM), warm cache and logical bytes. For the flat 1M tree, HyperDU's standalone median over eight runs was 589.91 ms. GNU `du` 8.32 (Git for Windows / MSYS) reached the 600-second warmup timeout: **there is no accepted 1M comparison ratio**. Wide / deep 1M workloads were not run.

| 10K diagnostic, median of two runs per tool | HyperDU | GNU `du` (MSYS) | du / HyperDU |
|---|---:|---:|---:|
| flat | 32.98 ms | 704.17 ms | 21.35× |
| wide | 27.46 ms | 712.33 ms | 25.94× |
| deep | 32.32 ms | 836.73 ms | 25.89× |

Do not extrapolate these diagnostic ratios to 1M files or Windows generally. Accounting, environment, and run counts also differ from Linux.

### Windows: comparison with dua-cli and tokei

Windows 11 / NTFS / NVMe, Ryzen 9 3900X, warm cache, logical bytes. HyperDU (development build d4ebdb8) and dua-cli produced totals equal to an independent walk on every dataset. Medians of 12 runs per tool.

| Dataset | Files | HyperDU | dua-cli | dua / HyperDU |
|---|---:|---:|---:|---:|
| Wide directory tree (synthetic) | 99,856 | 67.57 ms | 187.37 ms | 2.77× |
| Deep directory tree (synthetic) | 100,000 | 179.68 ms | 194.62 ms | 1.08× |
| Flat directory (synthetic) | 100,000 | 98.80 ms | 997.51 ms | 10.10× |
| Cargo registry (real files) | 107,953 | 478.55 ms | 554.61 ms | 1.16× |
| node_modules (real files) | 27,940 | 72.26 ms | 156.60 ms | 2.17× |

HyperDU was faster on all five, but the gap depends on tree shape (1.08-2.77x excluding flat). The deep tree has no width to parallelize and is roughly equal. The flat 10.10x includes dua-cli printing 100,001 rows and is not representative.

tokei reads contents and counts lines, a different job, so **no speed ratio is shown**. For reference, the Cargo registry (107,953 files) took 478.55 ms in HyperDU and 9,407.19 ms in tokei; node_modules (27,940 files) took 72.26 ms and 782.31 ms. Unrelated jobs held the CPU at roughly 60-90% busy during measurement, so absolute times are inflated.

[Method and limitations](docs/en/benchmarks.md) · [Record JSON](docs/benchmarks/2026-09-30-windows-vs-dua-tokei.json)

[Workflow evidence](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) · [Raw data](https://hyperdu.automation.jp/benchmarks.json) · [Method and limitations](docs/en/benchmarks.md) · [Performance design](docs/en/performance.md)

## Distribution and platform status

The [published release](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5) provides Windows x86_64 and Linux x86_64 (glibc / musl) / aarch64 CLI and GUI binaries. Choose the available formats from its Assets list. A packaging manifest does not establish registration in a package store.

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

For winget, check the actual catalog with `winget search --id automationjp.HyperDU --exact`; this repository's manifest alone does not establish availability.

<a id="platform-status"></a>

| OS | Current verification / distribution scope |
|---|---|
| Windows | Native CI, NTFS fixture, CLI / GUI distribution |
| Linux | CI, CLI / GUI distribution, ext4 benchmark above |
| macOS | `getattrlistbulk` implementation exists, but no current CI or release target. CLI verification is incomplete; GUI use is not guaranteed |

Windows `--mft` is an experimental opt-in path requiring an NTFS volume root, administrator privileges, and supported options. Incomplete required reads or parsing fall back to normal enumeration. **Successful MFT parsing is not proof of complete accounting parity.** [MFT boundaries](docs/en/architecture.md)

## Experimental Linux snapshots

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

This is not a watcher. `show` reads saved values and always marks freshness as `stale`. [Snapshot contract](docs/en/index-snapshots.md)

## Development

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p hyperdu -- --help
```

See the [developer guide](docs/developer-guide.md) for crate responsibilities, Rust integration, CI / publication trust boundaries, and feature-specific checks. Tracy and Puffin cannot be enabled together; do not use `--all-features` as an aggregate verification command.

[Architecture](docs/en/architecture.md) · [Setup](docs/en/setup.md) · [Historical documents](docs/old/README.md) · [Documentation index](docs/en/README.md)

## License / Acknowledgements

[MIT License](LICENSE). Design references: [ripgrep](https://github.com/BurntSushi/ripgrep), [fd](https://github.com/sharkdp/fd), and [dust](https://github.com/bootandy/dust).
