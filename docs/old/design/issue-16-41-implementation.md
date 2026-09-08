# Issue #16 / #41: bounded implementation and acceptance

## Decisions for this change

- Keep the default scanner, `--mft` opt-in and named-stream accounting policy unchanged.
- Expose the existing Linux directory index through explicit `index refresh ROOT --database FILE`
  and `index show ROOT --database FILE` commands. No service installation or automatic startup.
- A scan is not an atomic filesystem snapshot. A persisted value is always shown as `stale`;
  `show` reads the index and validates the root identity, but never walks the target subtree.
- An explicit database path must be outside the scanned root. Each root has an independent file.
- Read ordinary-file `$ATTRIBUTE_LIST` references using stream name, instance, VCN and sequence
  identity. Reject unresolved required data instead of substituting stale `$FILE_NAME` sizes.
- Do not add all alternate streams to physical totals: the measured 16.4 GB of streams is not
  proof that it explains the 6.1 GB discrepancy with directory enumeration.

## Implementation plan

1. Add failing tests for persisted freshness, stale ancestors, real CLI refresh/read calls,
   named-before-unnamed data, per-file extension data, and extension record de-duplication.
2. Make index reloads conservative; propagate invalidation to ancestors; connect exact scans
   and explicit snapshot commands. Refuse interrupted/incomplete scans and preserve old files.
3. Resolve named and unnamed MFT streams explicitly, validate extension ownership, aggregate
   sparse runs without summing repeated whole-stream headers, and fail over to enumeration
   when a required list/extent cannot be resolved. Run parser fixtures on Linux and Windows.
4. Run core/CLI tests, workspace Clippy, formatting, and native Windows CI; review final diff.

## Acceptance and boundaries

- A created file is invisible to `show` until explicit `refresh`; output says `stale` in both
  cases. A different root is refused; missing roots and self-indexing do not replace a snapshot.
- Sparse files and hardlinks retain the existing scanner's accounting. Bind-mount aliases that
  cannot be represented uniquely by `(dev, ino)` are refused rather than silently merged.
- Reused extension records, duplicate/overlapping VCNs, missing extents and truncated attribute
  data must not be reported as a complete MFT result. Named streams remain diagnostic only.
- The foreground/background watcher, overflow recovery and watch-budget policy of #16 remain
  outside this change pending the resident-process decision. This PR does not close #16.
- The ADS/WOF product-policy choice and real-volume physical parity of #41 remain separate
  acceptance gates. Keep the existing 8% live-volume physical tolerance; do not lower it on
  the basis of synthetic fixtures or claim the full residual is eliminated.

## Additional regression findings

The new named-stream fixture exposed a separate aggregation bug: `entries()` omits NTFS
metadata records including the volume root (record 5), while `to_stat_map()` required every
file parent to appear in `paths_for()`. Files directly below the volume root therefore lost
both their byte totals and file count. Treat that known root explicitly; genuine orphans
still remain excluded. This was observed as a failing 0-versus-4096-byte fixture before
the fix, not inferred from the original live-volume discrepancy.

Stream aggregation also retains the existing CompressionUnit-based compressed-size rule.
A sparse flag without a COMPRESSED flag must not erase a valid compressed-size header.
