# hyperdu

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

Fast cross-platform disk usage analyzer. HyperDU is designed around platform-specific
filesystem fast paths and parallel traversal rather than being a direct rewrite of `du`.

## Performance

See [measured results and limitations](../docs/en/benchmarks.md) for Windows / NTFS
and Linux / WSL2 / ext4. Cold cache and XFS remain unmeasured. The scanner design
is described in [performance.md](../docs/en/performance.md).

## Install

```bash
cargo install hyperdu --version 0.5.0-beta.3
```

Both the crate and command are `hyperdu`. One installation includes the CLI and
the MCP server, which starts only when you run `hyperdu mcp`. Rust 1.88+ is required.

This release is being prepared. Install this checkout now with
`cargo install --locked --path hyperdu` from the repository root; the registry
command above becomes available after publication.

Prebuilt packages do not require a Rust toolchain. For source builds, Rust versions,
Windows MSVC/SDK requirements, Linux native build tools, and development setup, see
[docs/setup.md](../docs/en/setup.md).

## Use

See the [CLI parameter reference (Japanese)](../docs/cli-reference.md) for defaults, platform restrictions, progress, and output modes. The `--time` options require the `time-format` feature, enabled by default.

```bash
hyperdu /path --top 20            # largest directories
hyperdu /path --json out.json     # structured output
hyperdu --compat gnu -k /var/log # du compatibility mode
hyperdu mcp                     # MCP server over stdio
hyperdu -- mcp                  # scan a directory literally named mcp
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
