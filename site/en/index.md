---
layout: default
lang: en
home_path: /en/
lang_switch_label: Language
permalink: /en/
title: "HyperDU — find what is filling the disk"
description: "Fast cross-platform disk usage analysis, measured on Windows and Linux. Answers what is using space and which of it is safe to delete."
---

# Find what is filling the disk

<p class="lede">
When a disk fills up, the useful question is not just what is large. It is <strong>which of it is safe to delete</strong>. HyperDU answers both.
</p>

```bash
hyperdu /path --top 20
```

## Speed

Measured on one machine (Ryzen 9 3900X, NVMe SSD) on 2026-09-07.

### Windows — 101,368 files / 2.96 GB

| Tool | Time | Ratio |
|---|---|---|
| **HyperDU** | **251 ms** | — |
| robocopy `/L /S` | 14,362 ms | **57×** |
| GNU du 8.32 (MSYS2) | 12,871 ms | **51×** |

### Linux (WSL2, ext4) — 712,374 files

| Tool | Time | Ratio |
|---|---|---|
| **HyperDU** | **191 ms** | — |
| du (uutils coreutils 0.8.0) | 2,900 ms | **15×** |

Two comparisons on purpose. MSYS2's `du` goes through a POSIX compatibility
layer, which puts it at a structural disadvantage on Windows, so **robocopy**
was measured too: Microsoft's own tool, native API, no emulation. It is still
57× slower.

On Linux, `du` is Ubuntu 26.04's default, which is uutils coreutils — a Rust
implementation. That makes it a **Rust-to-Rust** comparison.

> **Scan volumes were verified identical every time.** robocopy and HyperDU
> agreed exactly: 101,368 files, 2,956,477,017 bytes. Neither side is skipping
> work to look fast.

Ratios depend heavily on tree shape and storage. Deep, narrow trees narrow the
gap, and on spinning disks or network filesystems the work becomes I/O bound.
**Full numbers, method, and the less flattering results** are in
[docs/benchmarks.md](https://github.com/{{ site.repository }}/blob/main/docs/benchmarks.md).

## Why it is fast

- **Per-platform bulk enumeration** — `getdents64` + `statx` on Linux,
  `NtQueryDirectoryFile` on Windows (allocation size and file id arrive with the
  batch, so hardlink dedupe costs no extra syscalls), `getattrlistbulk` on macOS
- **Reads the NTFS `$MFT` directly** when run elevated against a volume root
- **Work stealing** across per-worker LIFO deques, taking from the shallow end so
  a thief gets a large subtree

## Install

### crates.io

```bash
cargo install hyperdu-cli --version {{ site.versions.cli }}
```

Prereleases are not selected by default, so `--version` is required. **The crate
is `hyperdu-cli`; the command it installs is `hyperdu`** — the same shape as
ripgrep installing `rg`.

### Windows (not yet available)

> **Not available yet.** There is no published release. Even after one exists,
> winget needs a pull request to
> [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) and Scoop
> needs a bucket entry; neither has been submitted. Use the crates.io route
> above until then.

```powershell
winget install automationjp.HyperDU
scoop install hyperdu
```

### From source

```bash
git clone https://github.com/{{ site.repository }}.git
cd HyperDiskUsage
cargo install --path hyperdu-cli
```

## For agents

Three surfaces let an agent such as Claude or Codex receive disk usage as
structured data and decide what is safe to remove. **They do not depend on each
other, so you can adopt any one alone.**

| Surface | Usable alone |
|---|---|
| MCP server | Any MCP client |
| Agent Skill | Drives the CLI, so no MCP needed |
| Agent Plugin | Bundles the other two |

```bash
cargo install hyperdu-mcp --version {{ site.versions.mcp }}
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
```

Three tools, in the order the questions actually arrive:

| Tool | Answers |
|---|---|
| `list_volumes` | Which volume is short on space |
| `scan_path` | What is large inside it |
| `find_reclaimable` | Which of that can be **rebuilt**, with idle days |

**There is no delete tool, deliberately.** Exposing one would create a path for
an agent to destroy data without a human in the loop. A wrong report can be
corrected; a wrong `rm -rf` cannot.

`find_reclaimable` does not classify `target/` by name alone. Without a
`Cargo.toml` beside it, that directory is someone's data, and reporting it as
reclaimable would be the failure that makes the tool unusable.

## Where this stands

- **Beta.** The API and the tool surface may still change
- **No GitHub Releases yet.** Today the paths in are `cargo install` or a source
  build
- **macOS is unverified on hardware** — it builds, but has not been run
- The Linux figures come from WSL2 and may differ from bare metal

## Usage

```bash
# largest directories
hyperdu /path --top 20

# structured output
hyperdu /path --json out.json

# drop-in for du
hyperdu --compat gnu -sh /var/log
alias du='hyperdu --compat gnu'
```

See the [README](https://github.com/{{ site.repository }}#readme) for the full
option list.
