# hyperdu-core

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

The scanning engine behind [HyperDU](https://github.com/automationjp/HyperDiskUsage).
Walks a directory tree in parallel and reports logical size, physical size on disk,
and file counts per directory. Most users want the [`hyperdu`](https://crates.io/crates/hyperdu)
command; this crate embeds the scanner in your application.

For an unpublished checkout, depend on the core crate directly:

```toml
[dependencies]
hyperdu-core = { path = "../hyperdu-core" }
```

The current checkout's generated Rust API documentation covers the batch and
interactive scan APIs, shared cancellation and progress hooks, report writers,
the persistent index, and volume helpers. Open it with `cargo doc --locked -p hyperdu-core --no-deps --open`.
Published releases have separate [Rust API documentation](https://docs.rs/hyperdu-core/latest/hyperdu_core/).
The core crate can be used without the `hyperdu` CLI, its MCP server, or the GUI.

## Batch scan

```rust
use hyperdu_core::{scan_directory, Options};

let stats = scan_directory("/path", &Options::default())?;
let root = stats.get(std::path::Path::new("/path")).unwrap();
println!("{} bytes across {} files", root.physical, root.files);
# Ok::<(), anyhow::Error>(())
```

Directory totals include descendants. `scan_directory_mode` also offers
`ScanMode::Interactive`: root listing and direct-file totals arrive first, followed
by each completed child subtree. Options, hardlink deduplication, link-cycle tracking,
filesystem boundaries, progress, errors, and cancellation are shared across phases.
A successful MFT scan explicitly reports batch fallback. See the
[architecture](../docs/en/architecture.md) for the event contract.

## Implementation

- Linux: `getdents64` + `statx`.
- Windows: batched `NtQueryDirectoryFile` with allocation size and file ID;
  optional NTFS `$MFT` reading when privileges and volume-root conditions apply.
- macOS: `getattrlistbulk`; hardware verification remains outstanding.
- Per-worker LIFO queues and work stealing.
- Hardlink deduplication by filesystem identity, matching default `du` accounting.

`volume::list()` exposes filesystem capacity. `index` provides persistent directory
aggregates. `reclaimable` identifies generated-output candidates; sampled timestamps
do not prove inactivity or deletion safety. No deletion is performed.

## License

MIT
