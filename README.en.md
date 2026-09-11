# HyperDU

> **Find what is filling your disk, fast.**<br>
> A fast, cross-platform disk usage analyzer written in Rust. It provides a CLI, GUI, GNU `du` compatibility mode, and MCP / Skill interfaces for AI agents.

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-black?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20Linux-tested-blue)](#platform-status)

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

[Setup](docs/en/setup.md) · [Performance](docs/en/performance.md) · [Benchmark plan](docs/en/benchmarks.md) · [Documentation](docs/en/README.md) · [Web site](https://automationjp.github.io/HyperDiskUsage/) · [Japanese](https://automationjp.github.io/HyperDiskUsage/)

---

## Quick start

See the [CLI parameter reference (Japanese)](docs/cli-reference.md) for defaults, output modes, and platform restrictions. The `--time` options require the `time-format` feature, enabled by default.

> **0.5.0-beta.3 is being prepared for publication.** On this branch, install it with `cargo install --locked --path hyperdu`. The crates.io commands and newly named distribution artifacts below become available after the release is published.

```bash
cargo install hyperdu --version 0.5.0-beta.3
hyperdu . --top 20
```

The crate and executable are both **`hyperdu`**. This installs the CLI and MCP server together.

The workspace has three user-facing crates: `hyperdu-core` is the shared scanning engine, `hyperdu` is the CLI binary with the built-in MCP server (`hyperdu mcp`), and `hyperdu-gui` is the separate GUI package.

```bash
# Analyze the current directory
hyperdu .

# Show the 20 largest items
hyperdu /path/to/data --top 20

# Export to JSON
hyperdu /path/to/data --json result.json

# GNU du compatibility mode
hyperdu --compat gnu -k /var/log
```

Run `hyperdu --help` to see all options.

## Performance

HyperDU is focused on **fast disk usage analysis**.

Rather than simply rewriting `du` in Rust, it optimizes the hot path for OS-specific directory enumeration, metadata retrieval, parallel traversal, and physical-size accounting.

### HyperDU compared with du

The measured Linux and Windows results are reported with the conditions for each environment. Every successful directory row matched the independent oracle. Speed ratio is GNU `du` time divided by HyperDU time. Measured source snapshot: `2645689515ab2e608a78b7492637e55179e4739a`. [GitHub Actions run](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) · [benchmarks.json](https://automationjp.github.io/HyperDiskUsage/benchmarks.json) · [site performance results](https://automationjp.github.io/HyperDiskUsage/en/#performance) · [Details](docs/en/benchmarks.md)

#### Linux — 1M files per shape

On GitHub-hosted Ubuntu 24.04 / ext4 with an AMD EPYC 9V74 (4 vCPU, 15.6 GiB RAM), each shape contained 1,000,000 regular 256-byte files. These are warm-cache medians using allocated bytes and eight alternating runs per tool. The Linux headline is **up to 3.25× faster** (3.256× for the wide shape) and applies to Linux only.

| Shape | HyperDU | GNU `du` | du / HyperDU |
|---|---:|---:|---:|
| flat | 2327.49 ms | 2763.52 ms | 1.187× |
| wide | 745.96 ms | 2428.80 ms | 3.256× |
| deep | 764.67 ms | 2434.28 ms | 3.183× |

#### Windows — 1M result and 10K diagnostic

Measurements used Windows 11 / NTFS / NVMe on an AMD Ryzen 9 3900X (12 cores / 24 threads, 128 GiB RAM), with a warm cache and apparent-size logical bytes. For the flat 1M tree, standalone HyperDU measured a median of 589.91 ms over eight runs. GNU `du` 8.32 (Git for Windows / MSYS) timed out during warmup at 600 seconds before a measured run. There is no accepted 1M GNU `du` baseline or ratio; wide and deep 1M workloads were not run.

The 10K comparison diagnostic uses medians of two runs per tool. These ratios apply only to this small diagnostic and are not general 1M-file speed claims.

| Shape | HyperDU | GNU `du` 8.32 (MSYS) | du / HyperDU |
|---|---:|---:|---:|
| flat | 32.98 ms | 704.17 ms | 21.35× |
| wide | 27.46 ms | 712.33 ms | 25.94× |
| deep | 32.32 ms | 836.73 ms | 25.89× |

### Why it is fast

| Platform | Main path | Optimization |
|---|---|---|
| Linux | `getdents64` + `statx` | Fetch directory entries in batches and aggregate metadata efficiently in parallel |
| Windows | `NtQueryDirectoryFile` / `FileIdFullDirectoryInformation` | Fetch names, sizes, allocation sizes, and file IDs in batches |
| macOS | `getattrlistbulk` | Fetch metadata in bulk |

In addition, `hyperdu-core` uses per-worker LIFO deques and work stealing to redistribute work according to directory-tree imbalance.

On Windows, `--mft` enables a direct `$MFT` read path when the NTFS volume root and permission requirements are met. If safe analysis is not possible, it falls back to directory enumeration.

See [Performance design](docs/en/performance.md) for optimization details and [Architecture](docs/en/architecture.md) for relationships between components.

## What HyperDU provides

### Fast disk analysis

- Logical size / physical allocation size accounting
- Hard-link deduplication
- Multithreaded scanning + work stealing
- Filesystem-specific scan strategies
- Exclude / maximum depth / minimum file size
- JSON / CSV output
- Basic / deep classification
- Progress display

### GNU `du` compatibility mode

```bash
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -k /home --max-depth=2
hyperdu --compat gnu -b --time /usr/share
```

HyperDU is not fully POSIX `du` compatible. `--compat posix-strict` selects defaults such as 512-byte units, but required POSIX options `-a`, `-s`, `-H` and `-L` are not implemented. Do not use it as a drop-in replacement or alias for `du`.

Compatibility is tested continuously, but HyperDU does not guarantee unconditional, complete reproduction of every GNU coreutils behavior.

### Structured output

```bash
hyperdu /srv/data --json usage.json
hyperdu /srv/data --csv usage.csv
```

## AI agents: MCP / Skill / Plugin

HyperDU can also be used by AI agents such as Claude and Codex.

| Interface | Purpose | MCP required? |
|---|---|---|
| MCP server | Typed arguments / structured results | Yes |
| Agent Skill | Triage workflow using the `hyperdu` CLI | No |
| Agent Plugin | Distributes MCP + Skill together | Optional |

The MCP server exposes three tools:

| Tool | Question it answers |
|---|---|
| `list_volumes` | Which volume is running out of space? |
| `scan_path` | What is taking up the most space there? |
| `find_reclaimable` | Which candidates can be regenerated? |

**There is intentionally no deletion tool.** This avoids giving agents a path to destroy data without human confirmation.

```bash
cargo install hyperdu --version 0.5.0-beta.3
claude mcp add --transport stdio hyperdu -- hyperdu mcp
# Codex:
codex mcp add hyperdu -- hyperdu mcp
```

See [plugin/README.en.md](plugin/README.en.md) for the Agent Skill / Plugin.

## GUI

`hyperdu-gui` is an eGUI / eframe-based desktop UI.

It offers Interactive scanning (the default) and Batch scanning with shared core semantics, advanced filter and performance options, and JSON/CSV export.

<!-- Parent task: verify the final GUI feature wording before release. -->

- Realtime scanning
- Interactive tree view
- Directory drill-down
- Throughput display
- Result export

```bash
cargo install hyperdu-gui --version 0.5.0-beta.3
hyperdu-gui
```

## Installation

**Rust is not required to run a prebuilt binary, use Scoop, or install the `.deb` package.** Rust versions, Windows MSVC / Windows SDK, Linux native build tools, GUI dependencies, and MCP requirements for source builds are described in [Setup and build environment](docs/en/setup.md).

### crates.io

```bash
# CLI + MCP (Rust 1.88+)
cargo install hyperdu --version 0.5.0-beta.3

# GUI (Rust 1.75+)
cargo install hyperdu-gui --version 0.5.0-beta.3

# Start the MCP server only when needed
hyperdu mcp
```

### Prebuilt binaries

After publication, downloads will be available from [Releases](https://github.com/automationjp/HyperDiskUsage/releases). The following are the planned filenames for the next release.

| Platform | CLI | GUI |
|---|---|---|
| Windows x86_64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.zip) / [exe](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.exe) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-windows-x86_64-generic.zip) |
| Linux x86_64 (glibc) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-generic.zip) |
| Linux x86_64 (musl) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-musl-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-musl-generic.zip) |
| Linux aarch64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-aarch64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-aarch64-generic.zip) |

The same release also includes a `.deb` package for Debian / Ubuntu.

### Scoop (Windows)

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

### winget (Windows) — pending submission

```powershell
winget install automationjp.HyperDU
```

The manifest has passed `winget validate`, but it cannot be used until registration in winget-pkgs is complete.

### From source

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo install --path hyperdu
```

See [docs/en/setup.md](docs/en/setup.md) for the detailed build environment.

<a id="platform-status"></a>
## Platform status

| Platform | Status | Notes |
|---|---|---|
| Windows | **Tested** | NTFS, CI, native enumeration, optional MFT path |
| Linux | **Tested** | XFS / ext4, CI |
| macOS | CLI: **Unverified** / GUI: **Cannot build** | A `getattrlistbulk` implementation exists; the release workflow currently excludes macOS |

Minimum Rust versions:

- `hyperdu-core`, `hyperdu-gui`: **Rust 1.75+**
- `hyperdu` (CLI + MCP): **Rust 1.88+**
- Workspace-wide build/test: **Rust 1.88+**

## Experimental: persisted Linux snapshots

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

This is not a watcher. `show` returns the stored value and always marks freshness as `stale`.

See [Linux directory snapshots](docs/en/index-snapshots.md) for details.

## Documentation

- [Setup and build environment](docs/en/setup.md)
- [Documentation index](docs/en/README.md)
- [Performance design](docs/en/performance.md)
- [Benchmark plan / remeasurement checklist](docs/en/benchmarks.md)
- [Architecture](docs/en/architecture.md)
- [Linux persisted snapshots](docs/en/index-snapshots.md)
- [Historical / old documents](docs/old/README.md)
- [Agent Plugin / Skill](plugin/README.en.md)

## Development

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

Use Rust 1.88+ for workspace-wide development. See [Setup and build environment](docs/en/setup.md) for platform-specific prerequisites and how to check optional features.

For pull requests that change a performance path, check the correctness gate and remeasurement conditions in the [Benchmark plan](docs/en/benchmarks.md) in addition to the regular tests.

## Known limitations

- This is a beta release. CLI options, MCP tool schemas, and output formats may change.
- Public benchmarks include the Linux and Windows measurements above. OS, accounting method, file count, and run count differ, so the Linux headline and Windows diagnostic ratios do not extend to unmeasured conditions.
- macOS performance and compatibility validation is not complete.
- On network filesystems or HDDs, I/O latency can dominate and reduce the gains from parallelism.
- Symbolic links are not followed by default. Take care with cycles when using `--follow-links`.

## License

[MIT License](LICENSE)

## Acknowledgements

- [ripgrep](https://github.com/BurntSushi/ripgrep) — high-performance filesystem/search implementation
- [fd](https://github.com/sharkdp/fd) — parallel filesystem traversal
- [dust](https://github.com/bootandy/dust) — disk usage UX

---

**HyperDU — fast disk analysis for humans and agents.**

[POSIX du compatibility audit](docs/en/posix-compatibility.md)
