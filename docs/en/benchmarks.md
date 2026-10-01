# HyperDU compared with du

Successful measurements match an independent oracle, and completed comparisons also match GNU `du` directly. No external byte adjustment is applied, and WSL2 timings are not current speed evidence. The measured source was `2645689515ab2e608a78b7492637e55179e4739a`; detailed hashes, all raw samples and corpus fingerprints are in the [published measurement result JSON](https://automationjp.github.io/HyperDiskUsage/benchmarks.json).

## Linux results (GitHub Actions)

The GitHub-hosted runner used Ubuntu 24.04/ext4 with 1,000,000 regular 256 B files in flat, wide and deep shapes. Each tool ran eight alternating warm trials, for 48 raw samples total. Timings include process startup and directory output; allocated-byte values matched for every directory row.

Linux medians (HyperDU / GNU `du`; GNU `du` / HyperDU speed ratio) are Flat 2327.49 / 2763.52 ms (1.187x), Wide 745.96 / 2428.80 ms (3.256x), and Deep 764.67 / 2434.28 ms (3.183x).

The conservative headline is 3.25x (wide, Linux only). The [GitHub Actions Linux run](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) contains the matching source, build, corpus and parity evidence.

### Linux command

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

Both tools emit every directory row. Row order is normalized only for comparison; totals and byte values are not adjusted.

## Windows supplemental results (local)

The local machine was Windows 11 on NTFS/NVMe with a Ryzen 9 3900X (12 cores / 24 threads, 128 GiB). GNU `du` was GNU coreutils 8.32 from Git for Windows MSYS. Both tools used `--apparent-size` for logical-byte accounting, with warm timings.

For 1M flat, HyperDU ran standalone for eight trials with a 589.91 ms median. GNU `du` timed out after 600 seconds during warmup, so there is no accepted 1M comparison or 1M ratio. The wide and deep 1M workloads were not run.

A separate 10K diagnostic used three shapes and two alternating trials per tool, retaining 12 measured samples plus 6 warmups. All directory rows matched, but these ratios apply only to this small diagnostic and are not a general 1M headline.

The 10K diagnostic medians (HyperDU / GNU `du`; GNU `du` / HyperDU speed ratio) are Flat 32.98 / 704.17 ms (21.35x), Wide 27.46 / 712.33 ms (25.94x), and Deep 32.32 / 836.73 ms (25.89x).

### Windows command

```powershell
hyperdu --compat gnu-strict --apparent-size --block-size 1 --one-file-system --io-profile balanced ROOT
du -x --apparent-size --block-size=1 ROOT
```

The AWS runbook is retained as an unexecuted plan; it contributes no current result or speed figure. See the [AWS runbook (unexecuted plan)](../benchmarks/aws-protocol.md).

[Reproducible Linux benchmark runner](../../scripts/bench/du_same_conditions.py)


## Comparison with dua-cli and tokei (local Windows)

HyperDU built from `d4ebdb85577c53e881af292b549bc784f05bec60` (a development snapshot of v0.5.0-beta.5) with `cargo build --locked --release -p hyperdu` (rustc 1.98.0) was compared with dua-cli 2.45.0 and tokei 15.0.0, both built by `cargo install --locked` with the same rustc. The machine was Windows 11 / NTFS / NVMe, Ryzen 9 3900X (12 cores / 24 threads, 128 GiB RAM). Warm cache, tools alternating, 12 runs each; every raw sample is in the [record JSON](../benchmarks/2026-09-30-windows-vs-dua-tokei.json). Timings run from process start to exit and include writing to stdout. Each dataset was checked with an independent lstat walk before and after timing to confirm it had not changed.

### dua-cli (same job)

HyperDU and dua-cli logical byte totals equalled the independent walk on all five datasets. No external byte adjustment was applied.

| Dataset | Files | HyperDU | dua-cli | dua / HyperDU |
|---|---:|---:|---:|---:|
| Wide directory tree (synthetic) | 99,856 | 67.57 ms | 187.37 ms | 2.77x |
| Deep directory tree (synthetic) | 100,000 | 179.68 ms | 194.62 ms | 1.08x |
| Flat directory (synthetic) | 100,000 | 98.80 ms | 997.51 ms | 10.10x |
| Cargo registry (real files) | 107,953 | 478.55 ms | 554.61 ms | 1.16x |
| node_modules (real files) | 27,940 | 72.26 ms | 156.60 ms | 2.17x |

HyperDU was faster on all five, but the gap depends on tree shape: 1.08-2.77x excluding the flat tree.

- The deep tree (1.08x) has almost no width to parallelize. The spreads overlap (HyperDU 159-264 ms, dua-cli 170-312 ms), so read it as roughly equal.
- The flat tree's 10.10x is not a representative ratio. dua-cli prints one row per direct entry (100,001 rows) and that output time is included. `--depth 1` does not reduce the output and its total does not equal the independent walk, so it was not used.
- Real files: 1.16x on the Cargo registry and 2.17x on node_modules.

### tokei (reference, different job)

tokei counts source lines: it reads file contents and parses each language. HyperDU only totals sizes and does not read contents. These are not the same job, so **no speed ratio is shown**; the times are reference only. The synthetic fixtures have no extensions, so tokei parses nothing there and only the two real trees were timed.

| Dataset | Files | Files tokei parsed | HyperDU | tokei |
|---|---:|---:|---:|---:|
| Cargo registry (real files) | 107,953 | 80,897 | 478.55 ms | 9,407.19 ms |
| node_modules (real files) | 27,940 | 16,576 | 72.26 ms | 782.31 ms |

### Limits

- During measurement, unrelated jobs (a densel GC and Python workers) held the 24 logical CPUs at roughly 60-90% busy. Tools alternated so each saw the same load, but absolute times are inflated and single runs vary widely (HyperDU on the flat tree: 68-1,096 ms). This is not an isolated benchmark host.
- Cold cache was not measured. tokei reads file contents, so warm numbers must not be applied to cold runs.
- The synthetic trees are extension-less 4 KiB regular files and interact with dua-cli's output format (it lists every direct entry). Do not generalize beyond the trees shown.
- A third real tree (Rust toolchains, 345,102 files) was planned but dropped after one tokei run exceeded 10 minutes; the cause was not investigated.
- HyperDU is origin/main at d4ebdb8, not the published v0.5.0-beta.5 binary.

### Commands

```powershell
hyperdu ROOT --top 1 --exclude "" --one-file-system
dua aggregate -A -x -f bytes ROOT
tokei ROOT --output json
```

[Comparison benchmark runner](../../scripts/bench/compare_tools.py)
