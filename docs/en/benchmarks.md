# Benchmarks

[日本語](../benchmarks.md) | **English** | [简体中文](../zh-CN/benchmarks.md)

This document defines the current reproducible warm benchmark and its evidence boundaries. The source under measurement is pinned to commit `0a089c90c0e2995ef702ff053820d129265c3659`. Result tables and raw samples are generated from the completed JSON reports.

## Scope and status

The benchmark covers **Windows / NTFS** and **Linux / WSL2 / ext4**. Each dataset is run exactly **8 times**, alternating which tool runs first, after the initial correctness run has warmed the cache. The reported time is the median process time, including process startup and output. The ratio is `baseline / HyperDU`; a ratio below 1 means that HyperDU was slower for that row.

The comparison tools are Windows `robocopy` and Linux **uutils `du` 0.8.0**. The Linux comparison is not GNU `du`, and it is not a bare-metal Linux measurement.

## Environment

- **Windows:** Windows 11 Pro 10.0.26200 on an AMD Ryzen 9 3900X (12C/24T) with 128 GiB RAM. Synthetic fixtures are stored on the F: WD_BLACK SN850X HS 4 TB and the registry tree is on the C: GIGABYTE GP-ASM2NE6100TTTD; both are NTFS/NVMe. The `robocopy` version is 10.0.26100.1 (WinBuild.160101.0800).
- **Linux:** WSL2 on the same PC, Ubuntu 26.04, kernel 6.18.33.2-microsoft-standard-WSL2, 16 vCPUs, approximately 90 GiB RAM, and ext4. The comparator is uutils `du` 0.8.0.

The release build and source provenance are recorded separately in the [build record](../benchmarks/build-record.md).

## Results

### Windows

`hyperdu 0.5.0-beta.3` / SHA-256 `b4c4c4ca78119d61319c9b0a342ab7b7e7dfed3992a44fe56d88ddb5b6a9c3d8`

Comparator: `10.0.26100.1 (WinBuild.160101.0800)`

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 30.721 | 57.424 | 1.87x |
| deep | 2,000 | 77.123 | 72.041 | 0.93x |
| flat | 20,000 | 34.081 | 42.825 | 1.26x |
| registry | 111,813 | 332.236 | 1838.460 | 5.53x |

```text
wide hyperdu 34.665 30.334 33.340 29.218 31.107 30.203 29.661 32.721
wide baseline 60.855 55.954 58.894 62.127 53.976 65.117 55.036 55.126
deep hyperdu 80.579 82.515 92.383 76.418 75.246 72.922 77.827 75.048
deep baseline 72.060 81.065 76.229 72.021 79.658 71.715 69.163 71.954
flat hyperdu 35.492 29.856 33.223 30.974 35.251 34.938 44.015 31.479
flat baseline 42.960 42.689 43.561 50.821 37.715 39.821 73.456 41.414
registry hyperdu 329.521 321.446 325.137 330.354 334.119 356.102 343.325 514.561
registry baseline 1691.376 1823.197 1937.345 1853.723 1592.403 1687.168 2615.618 2950.394
```

[JSON: commands, gates, raw samples](../benchmarks/2026-09-09-windows.json)

### Linux

`hyperdu 0.5.0-beta.3` / SHA-256 `a43fff5f5b7a7663b15f081c1361256ef111461848f1fe49419b0e829bd32eb4`

Comparator: `du (uutils coreutils) 0.8.0`

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 47.343 | 208.596 | 4.41x |
| deep | 2,000 | 89.107 | 143.359 | 1.61x |
| flat | 20,000 | 130.465 | 184.377 | 1.41x |
| registry | 37,814 | 144.584 | 978.928 | 6.77x |

```text
wide hyperdu 24.960 19.303 27.865 57.863 83.760 57.849 77.518 36.838
wide baseline 219.142 186.493 265.028 186.324 211.074 259.606 206.119 145.009
deep hyperdu 88.301 82.276 80.782 89.913 103.993 111.027 110.154 84.604
deep baseline 161.231 157.352 143.158 148.519 141.402 114.486 143.561 133.878
flat hyperdu 118.474 105.591 140.027 130.245 151.339 181.407 89.872 130.685
flat baseline 173.565 138.248 179.544 193.533 189.211 237.937 189.578 157.039
registry hyperdu 115.010 142.254 146.913 107.350 150.306 153.823 178.075 112.644
registry baseline 944.617 947.984 908.689 1558.686 1043.559 1187.297 1009.872 839.885
```

[JSON: commands, gates, raw samples](../benchmarks/2026-09-09-linux.json)


## Method

The `scripts/bench/remeasure.py` harness creates three synthetic workloads:

- **wide:** 500 directories × 40 files
- **deep:** 400 directory levels × 5 files
- **flat:** one directory containing 20,000 files

Every synthetic file contains 4,096 bytes. An additional Cargo registry tree may be supplied, but it is an OS-specific dataset and is not used as a cross-OS speed comparison.

The harness invokes HyperDU with:

```text
hyperdu <root> --top 1 --exclude "" --one-file-system
```

On Windows the baseline is `robocopy` with `/L /S /XJ /BYTES` plus no-copy, no-header, no-progress, and zero-retry options. On Linux the baseline is:

```text
du -sx --block-size=1 <root>
```

The first gate pass warms the cache. The eight timing repetitions then alternate the order: HyperDU first on one repetition, the baseline first on the next, for four repetitions in each order. The harness does not drop caches or delete the dataset.

## Correctness and stability gates

Before timing, the harness runs an independent Python `lstat` walk on the same device. It counts regular files, folds hardlinks by device/inode identity, and checks directory count, file count, and logical bytes against HyperDU. On Linux it also checks physical bytes. The `du` comparison includes directory and link allocation bytes when validating the allocated total.

Windows physical-size validation is **not performed**. The Windows gate checks the file and logical-byte workload against `robocopy`, while the physical column remains unverified.

After timing, the harness verifies that:

- the independent oracle still describes the same dataset;
- HyperDU's JSON totals are unchanged;
- the release binary's SHA-256 is unchanged; and
- the output JSON reaches `complete: true`.

The build record linked above must establish that the binary was built from the source commit named in this document.

## Limitations

- Cold-cache performance has not been measured.
- XFS has not been measured.
- macOS has not been measured.
- The direct NTFS MFT path has not been measured.
- GUI rendering and GUI Interactive mode performance have not been measured. These are CLI scan timings.
- Windows physical-size parity is unverified.
- `robocopy` measures a native enumeration/copy-plan operation, while `du` reports allocated bytes. Matching totals in the gate do not mean that the tools implement the same complete feature set.
- Error handling, links, compression, sparse files, and other special cases require separate semantic verification; this speed report does not guarantee them.
- Host background work and WSL2 behavior can affect the result. Small differences should not be generalized.

Do not publish a single speed multiplier without the workload, comparator, cache state, build provenance, and correctness gate that produced it.

## Reproduce

Build the pinned source in release mode with the lockfile:

```powershell
cargo build --release --locked -p hyperdu
```

Run the eight alternating warm trials:

```powershell
python scripts/bench/remeasure.py --bin <release-binary> --commit 0a089c90c0e2995ef702ff053820d129265c3659 --output <results.json> --dataset-parent <scratch> --runs 8
```

Use `--synthetic-root <directory>` to reuse existing `wide`, `deep`, and `flat` fixtures. Add `--tree <directory>` for an additional real tree. Inspect the JSON and continue only when `complete: true`; retain the raw samples and the [build record](../benchmarks/build-record.md) together.

## Related documentation

- [Performance design](performance.md)
- [Setup and build environment](setup.md)
- [Architecture](architecture.md)
- [Linux directory snapshots](index-snapshots.md)

Measurements ran on a shared development PC; other tasks, including builds, may affect timings. The machine was not isolated from background load.
