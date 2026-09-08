# hyperdu-cli

Fast cross-platform disk usage analyzer. HyperDU is designed around platform-specific
filesystem fast paths and parallel traversal rather than being a direct rewrite of `du`.

## Performance status

Public benchmark numbers are currently being remeasured. Historical measurements are
archived in the repository, but are not used as current performance claims.

| Scenario | HyperDU | Baseline | Ratio |
|---|---:|---:|---:|
| Windows / NTFS | TBD | TBD | TBD |
| Linux / ext4 | TBD | TBD | TBD |
| Linux / XFS | TBD | TBD | TBD |

The current methodology and remeasurement checklist are in
[docs/benchmarks.md](https://github.com/automationjp/HyperDiskUsage/blob/main/docs/benchmarks.md).
The implementation strategy is described in
[docs/performance.md](https://github.com/automationjp/HyperDiskUsage/blob/main/docs/performance.md).

## Install

```bash
cargo install hyperdu-cli --version 0.5.0-beta.2
```

The crate is `hyperdu-cli`; the command it installs is `hyperdu`. Same shape as
ripgrep installing `rg`, and it matches the deb, release binary, and Scoop command name.

Prebuilt packages do not require a Rust toolchain. For source builds, Rust versions,
Windows MSVC/SDK requirements, Linux native build tools, and development setup, see
[docs/setup.md](https://github.com/automationjp/HyperDiskUsage/blob/main/docs/setup.md).

## Use

```bash
hyperdu /path --top 20            # largest directories
hyperdu /path --json out.json     # structured output
hyperdu --compat gnu -sh /var/log # du compatibility mode
```

## Why it is designed to be fast

- Linux: `getdents64` + `statx`
- Windows: batched `NtQueryDirectoryFile` enumeration with allocation size and file ID
- macOS: `getattrlistbulk`
- per-worker LIFO queues with work stealing
- optional NTFS `$MFT` path when the required conditions are met

The MFT path is optional. If it cannot safely produce a complete result, HyperDU falls
back to directory enumeration.

See the [project README](https://github.com/automationjp/HyperDiskUsage#readme) for
installation options, GUI/MCP integration, platform status, and limitations.

## License

MIT
