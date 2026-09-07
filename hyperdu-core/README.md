# hyperdu-core

The scanning engine behind [HyperDU](https://github.com/automationjp/HyperDiskUsage).
Walks a directory tree in parallel and reports logical size, physical size on
disk, and file counts per directory.

Most users want the [`hyperdu-cli`](https://crates.io/crates/hyperdu-cli) binary
instead. This crate is for embedding the scan in your own program.

```rust
use hyperdu_core::{scan_directory, Options};

let stats = scan_directory("/path", &Options::default())?;
// Totals roll up, so the root entry holds the whole subtree.
let root = stats.get(std::path::Path::new("/path")).unwrap();
println!("{} bytes across {} files", root.physical, root.files);
# Ok::<(), anyhow::Error>(())
```

## What it does differently

- **Platform enumeration APIs**: `getdents64` + `statx` on Linux,
  `NtQueryDirectoryFile` with `FileIdFullDirectoryInformation` on Windows
  (physical size and file id arrive with the batch, so hardlink dedupe costs no
  extra syscalls), `getattrlistbulk` on macOS
- **NTFS `$MFT` reading** on Windows when elevated against a volume root
- **Work-stealing** across per-worker LIFO deques, stealing from the shallow end
  so a thief gets a large subtree
- **Hardlink dedupe by `(device, inode)`**, matching GNU du's default

Also exposes `volume::list()` for per-filesystem capacity, and `index` for
directory-level aggregates with persistence.

## License

MIT
