# MFT backend verification after #47

## Scope

This follow-up retains the product decisions recorded in #16 and #41: no resident
Linux watcher is introduced and named DATA streams are not added to unnamed-file
disk usage. It fixes two rejected NTFS layouts and makes backend comparisons
prove which backend produced the result.

The work starts at main `983bb01b8d6cae6d53eff47705438e2504477e68`.

## Why the old green comparison was insufficient

`mft_backend_applies` checks platform, root and elevation. It does not open the
volume or prove that DATA resolution succeeded. `scan_directory` deliberately
falls back to enumeration when the MFT backend declines a volume. Using that
function for both sides of a parity test can compare two enumeration scans.
Consequently, a low drift and a green test do not by themselves establish MFT
parity. This qualification also applies to the 0.71% result previously cited in
#41; it must not be treated as execution evidence for the MFT backend.

A read-only probe of the hosted Windows runner reproduced a DATA rejection:

- [First no-fallback probe](https://github.com/automationjp/HyperDiskUsage/actions/runs/34123710058).
- [Attribute-reference trace](https://github.com/automationjp/HyperDiskUsage/actions/runs/34124093265).
- [Accounting-invariant trace](https://github.com/automationjp/HyperDiskUsage/actions/runs/34124566961).
- [Portable regression RED](https://github.com/automationjp/HyperDiskUsage/actions/runs/34124891163):
  103 existing tests passed; the two new fixtures failed before the parser fix.

The first rejected base record was 159135. Its named `$J` stream had a first
extent covering VCN 0..609439 and a continuation covering 609440..871839. Both
carried the sparse flag. The initial extent used the whole-stream physical-size
header; the continuation did not repeat its local CompressionUnit value. The
old equality check between these local interpretations rejected the stream.
These record numbers are evidence from that runner, not hard-coded runtime rules.
No file contents or paths from the runner are needed in the permanent fixture.

## Parser contract

The lowest-VCN-zero extent selects whole-stream size accounting. A continuation
may omit the corresponding size header. Its local CompressionUnit does not
select a second accounting mode. When the first extent requires counting sparse
runs, the parser still reads and validates the continuation's run list; when the
first extent supplies the physical total, that total is used once.

An empty nonresident stream may have HighestVCN = -1. This sentinel is accepted
only for LowestVCN = 0, zero allocated/data/initialized sizes, a zero derived
physical size, and a terminated empty run list. Nonzero sizes, nonempty runs,
and continuation extents cannot use the sentinel to hide allocation.

Record sequence, owner, attribute instance and name, VCN continuity, matching
flags, bounds and run-list validation remain in place. Invalid or unsupported
DATA layouts still decline the MFT result rather than reporting a partial sum.

Primary layout references:

- [Microsoft ATTRIBUTE_RECORD_HEADER](https://learn.microsoft.com/en-us/windows/win32/devnotes/attribute-record-header)
  documents the initial-extent scope of nonresident size fields.
- [Linux NTFS attribute implementation](https://linux.googlesource.com/linux/kernel/git/torvalds/linux/+/1f57f68c4dd101e5e8ffc9ffa6428f45bcdd776a/fs/ntfs/attrib.c)
  constructs an empty nonresident attribute with LowestVCN = 0, HighestVCN = -1,
  zero size fields and a zero run-list terminator.

## API and test contract

`try_scan_directory_via_mft(root, options)` returns `Some(StatMap)` only from the
MFT backend and applies the same child-to-parent rollup as `scan_directory`.
It returns `None` instead of enumerating when that backend declines. `use_mft`
must still be enabled. It does not add support for per-file MFT filters or link
traversal.

`scan_directory` calls this shared path and retains its existing enumeration
fallback. Ordinary CLI and GUI callers need not change. The disabled path does
not gain a second scan or a new per-file syscall.

The parity test now obtains the MFT result first and requires `Some`. An explicit
`HYPERDU_MFT_PARITY_ROOT` that cannot use MFT fails, rather than silently skipping.
A normal unelevated local invocation may still skip with a reason. With
`HYPERDU_MFT_DIAG=1`, DATA rejection is written to stderr even when no logger was
initialized. A completed comparison prints `parity backend: mft (no fallback)`.

## Controlled Windows integration fixture

`scripts/test-ntfs-fixture.ps1` is restricted to an elevated GitHub Actions runner.
It creates a new 256 MiB growable VHDX at a fresh GUID-named path in RUNNER_TEMP,
selects an unused drive letter, and verifies that the resulting NTFS volume
belongs to that image. It never selects or cleans a physical disk by number.
After populating the fixture it detaches and reattaches that same image read-only,
then runs the Core and CLI tests. The `finally` block restores environment values
and detaches the owned image.

The two backends must independently match these known unnamed-file totals:

| Fixture subtree | Logical bytes | Physical bytes | Files |
| --- | ---: | ---: | ---: |
| `hyperdu-fixture/plain` | 8,388,608 | 8,388,608 | 128 |
| `hyperdu-fixture/sparse` | 1,048,576 | 0 | 2 |
| `hyperdu-fixture` | 9,437,184 | 8,388,608 | 130 |
| Files directly below the volume root | 65,536 | 65,536 | 1 |

An extra hardlink remains counted once, an 8 KiB named stream does not inflate
the unnamed-file totals, and an empty directory must remain present. Sparse
fixtures include both a fully unallocated 1 MiB stream and a zero-length stream.
The test subtracts immediate child subtree totals to check root-direct files;
it does not sum an already-rolled-up map.

This verifies actual NTFS I/O and the production MFT path, rather than only a
byte-array parser. It is not an exhaustive test of all NTFS layouts, WOF,
compression, live races, permissions or large-volume performance. The existing
live-volume 5% logical/file and 8% physical thresholds are not relaxed; the
controlled subtrees additionally require exact byte and file counts.

## Manual live-volume verification

From an elevated PowerShell session in the repository:

```powershell
$env:HYPERDU_MFT_PARITY_ROOT = 'C:\'
$env:HYPERDU_MFT_DIAG = '1'
cargo test -p hyperdu-core --test mft_parity -- --nocapture
```

Do not set HYPERDU_MFT_PARITY_FIXTURE for a live volume. Prefer a quiescent test
volume and record the commit, actual-backend marker, totals and diagnostics.
Live-volume failures must be investigated rather than replaced with enumeration
results or hidden by increasing the tolerance.

## Feature configurations

`cargo test --workspace --all-features` and the corresponding all-feature Clippy
command fail in `profiling` 1.0.17 because Tracy and Puffin export the same macros.
The dependency [explicitly supports one backend at a time](https://docs.rs/crate/profiling/1.0.17).
This is not a successful all-feature test and is not changed by the MFT patch.
The baseline check and separate configuration checks are recorded in the
[verification workflow](https://github.com/automationjp/HyperDiskUsage/actions/runs/34126090430).
Use the default workspace tests, the optional parallel-feature tests, and one
profiler configuration per Cargo invocation; see AGENTS.md for exact commands.

## Risk and rollback

The changed parser now accepts the validated continuation and empty-stream
layouts. It still falls back for unsupported layouts in ordinary scans. The
strict API is additive; callers requiring compatibility keep `scan_directory`.
The fixture changes only CI-created disposable storage. No release, publication,
resident service, deletion tool or default MFT enablement is included.
