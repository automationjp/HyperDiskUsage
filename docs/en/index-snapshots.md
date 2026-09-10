# Directory index v2 (experimental)

[日本語](../index-snapshots.md) · **English** · [简体中文](../zh-CN/index-snapshots.md)

Save native identities and sizes, query directory totals, and monitor supported local filesystems in the foreground on Windows, Linux and macOS.

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx" --once
```

`refresh` scans and replaces a v2 snapshot. `show` reads it without walking the tree (it still checks ROOT identity). `watch` applies notifications until Ctrl-C. `--once` catches up, saves and exits, with a 30-second limit. Rebuild v1 files with refresh; the v1 Rust API remains available.

## Native sources

| Platform | Source | Restart |
| --- | --- | --- |
| Windows | Existing NTFS/ReFS USN journal; requires volume read permission. Never creates or changes a journal | Validates volume, journal ID and retained USN range |
| macOS | Per-device FSEvents on local APFS/HFS | Validates device UUID and event ID before history replay |
| Linux | Filesystem fanotify; falls back to recursive inotify when capability or file handles are unavailable | Always rebuilds because queues do not survive restart |

Linux accepts `HYPERDU_INDEX_LINUX_BACKEND=auto|fanotify|inotify`; unavailable explicitly selected fanotify returns an error. Watch limits, overflow, journal gaps and incarnation changes require a rebuild. Root replacement requires explicit refresh. Unsupported native sources, including remote SMB/NFS monitoring, return a reason; explicit refresh/show remain available.

## Accounting and freshness

Totals include regular-file logical bytes, allocated bytes and unique file count. Directory storage, symlinks/reparse points, special files and named alternate streams are excluded. Hardlinks are deduplicated by volume and full file identity, owned by the smallest parent-ID/name link. Windows retains 128-bit IDs; native filename encoding is preserved.

`unknown` means unobserved; `stale` means persisted, updating or unvalidated. `observed` means all events delivered through a catch-up barrier were applied, not an atomic snapshot. Notifications may miss mmap, writes through outside hardlinks, or remote changes. Mandatory reconciliation runs every `--reconcile-seconds` (default 900, range 1–86400). Saved show output is always stale.

## Cost and persistence

A file mutation observes that file and adjusts ancestors. Moving a known directory retains descendants without enumeration. New subtrees and reconciliation enumerate their scope. Initial scanning and snapshot loading/saving cost O(files + links + directories); loaded root lookup is O(1).

Changes apply in memory immediately. Whole-file checkpoints are limited by `--checkpoint-seconds` (default 60, range 1–3600), plus initial catch-up and normal termination. After a crash, the last checkpoint is resumed or rebuilt. `--poll-ms` defaults to 100 (range 10–60000). JSON Lines include sizes, count, freshness, journal, checkpoint status, notifications, observed entries, directories read and rebuild status. Work counters describe one poll and exclude the startup baseline.

Create the parent first and store the database outside ROOT, so `/` cannot be a CLI root. Each index covers one filesystem; index mounted subtrees separately. Database and sibling `.lock` must be regular files, not symlinks. The stable OS lock excludes writers and releases on process exit; the lock file is retained.

The format validates root identity, native names, graph, size limits and checksum. The checksum detects accidental corruption, not hostile modification. Snapshot and cursor share one atomic replacement; incomplete updates are not checkpointed.

Normal scan flags do not change fixed index semantics. To scan a directory named index, use `hyperdu ./index` or `hyperdu -- index`.

[Architecture](architecture.md) · [Performance](performance.md) · [Historical design](../old/README.md)