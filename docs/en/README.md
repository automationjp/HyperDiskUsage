# HyperDU documentation

[日本語](../README.md) · **English** · [简体中文](../zh-CN/README.md)

Installation, scan-engine design, and verification guidance.

| Goal | Document |
|---|---|
| CLI defaults, output modes, and platform restrictions | [CLI parameter reference (Japanese)](../cli-reference.md) |
| Install, prepare Rust/OS dependencies, build and test | [Setup](setup.md) |
| Understand optimizations and tradeoffs | [Performance design](performance.md) |
| Inspect Windows/Linux results, every sample and conditions | [Benchmarks](benchmarks.md) |
| Understand CLI, GUI, MCP, the core and Interactive mode | [Architecture](architecture.md) |
| Use saved Linux directory information | [Snapshots](index-snapshots.md) |
| Connect AI agents | [Plugin / Skill / MCP](../../plugin/README.en.md) |

## Prepare the environment

Running a prebuilt binary does not require Rust. Building the CLI (including MCP)
or the whole workspace requires Rust 1.88+; core/GUI declare a minimum of 1.75.
Windows source builds use MSVC and the Windows SDK. Linux needs native build tools,
and the Linux GUI needs X11/Wayland development libraries. See [setup](setup.md)
for commands and verification boundaries.

## Read performance results

See [performance design](performance.md) for native enumeration and parallel traversal. The current Linux result is from GitHub Actions Ubuntu 24.04/ext4: 1M regular 256 B files in three shapes, eight alternating warm runs per tool and 48 raw samples, with direct allocated-byte parity for every directory row. GNU du / HyperDU speed ratios are 1.187x flat, 3.256x wide and 3.183x deep; the conservative 3.25x headline is Linux only. Windows 11 supplemental results and the unexecuted AWS runbook are documented in the [comparison results and method](benchmarks.md). WSL2 timings are not current speed evidence.

## Current documentation and historical records

`docs/` contains Japanese, `docs/en/` English, and `docs/zh-CN/` Simplified Chinese.
Commands, versions, and measurement tables are shared. `docs/old/` preserves older
benchmarks and issue-specific design/verification records in their original language.
Do not use them as evidence of current behavior or speed. See the
[historical index](../old/README.md).

[POSIX du compatibility audit](posix-compatibility.md)
