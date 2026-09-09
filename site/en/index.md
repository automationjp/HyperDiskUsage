---
layout: default
lang: en
home_path: /en/
lang_switch_label: Language
permalink: /en/
title: "HyperDU — fast disk usage analysis"
description: "A Rust disk-usage analyzer built around platform-specific fast paths and parallel traversal. Public benchmark numbers are being remeasured."
---

# Fast disk usage analysis

<p class="lede">
HyperDU is a Rust disk-usage analyzer built to <strong>find what is filling a disk quickly</strong>.
</p>

```bash
cargo install hyperdu-cli --version {{ site.versions.cli }}
hyperdu /path --top 20
```

## Performance

HyperDU is not just `du` rewritten in Rust. Its hot path includes platform-specific directory enumeration, metadata acquisition, and parallel traversal.

### Public benchmarks are being remeasured

Historical numbers are archived and are not used as current performance claims. Until the new measurements are complete, the result fields remain `TBD`.

| Scenario | HyperDU | Baseline | Ratio |
|---|---:|---:|---:|
| Windows / NTFS | TBD | TBD | TBD |
| Linux / ext4 | TBD | TBD | TBD |
| Linux / XFS | TBD | TBD | TBD |

See the [benchmark plan](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md) for the test matrix and publication checklist.

## Why it is fast

- **Linux** — `getdents64` + `statx` for low-overhead enumeration and metadata retrieval
- **Windows** — `NtQueryDirectoryFile` batches name, size, allocation size, and file ID
- **macOS** — `getattrlistbulk` for bulk metadata retrieval
- **Work stealing** — redistributes directory work between workers as tree sizes diverge
- **Optional NTFS `$MFT` path** — direct volume-root path when supported, with safe fallback to normal enumeration

See [Performance design](https://github.com/{{ site.repository }}/blob/main/docs/performance.md) for details.

## Usage

```bash
# largest entries
hyperdu /path --top 20

# JSON output
hyperdu /path --json out.json

# GNU du compatibility mode
hyperdu --compat gnu -sh /var/log
```

## AI agent integration

Claude, Codex, and other agents can inspect disk pressure through structured tools.

| Tool | Answers |
|---|---|
| `list_volumes` | Which volume is short on space |
| `scan_path` | What is large inside it |
| `find_reclaimable` | Which candidates can be rebuilt |

**There is deliberately no delete tool.**

```bash
cargo install hyperdu-mcp --version {{ site.versions.mcp }}
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
```

## Install

HyperDU is available through crates.io, prebuilt GitHub Release assets, and Scoop. winget submission is in progress.

See the [README](https://github.com/{{ site.repository }}#installation) for installation options.

## Current status

- Beta
- Windows and Linux are tested targets
- macOS CLI is not hardware-verified; GUI is not currently a release target
- Public performance benchmarks are being remeasured

## Documentation

- [Performance design](https://github.com/{{ site.repository }}/blob/main/docs/performance.md)
- [Benchmark plan](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md)
- [Architecture](https://github.com/{{ site.repository }}/blob/main/docs/architecture.md)
- [Documentation index](https://github.com/{{ site.repository }}/blob/main/docs/README.md)

Historical benchmarks and issue-specific design records are kept under `docs/old/`.
