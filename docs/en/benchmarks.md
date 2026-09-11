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
