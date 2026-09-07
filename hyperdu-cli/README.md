# hyperdu-cli

Fast cross-platform disk usage analyzer. Answers "what is filling this disk"
in a fraction of the time `du` takes.

Measured on a Ryzen 9 3900X with NVMe SSDs, 2026-09-07:

| Platform | Tree | HyperDU | Comparison |
|---|---|---|---|
| Windows | 101,368 files | **251 ms** | robocopy `/L /S`: 14,362 ms (**57x**) |
| Windows | 101,368 files | **251 ms** | GNU du 8.32 (MSYS2): 12,871 ms (**51x**) |
| Linux | 712,374 files | **191 ms** | du (uutils 0.8.0): 2,900 ms (**15x**) |

Scan volumes were verified identical between tools. Full methodology, including
the results that are less flattering, is in
[docs/benchmarks.md](https://github.com/automationjp/HyperDiskUsage/blob/main/docs/benchmarks.md).

## Install

```bash
cargo install hyperdu-cli --version 0.5.0-beta.2
```

The crate is `hyperdu-cli`; the command it installs is `hyperdu`. Same shape as
ripgrep installing `rg`, and it matches what the deb and rpm packages have
always called it.

## Use

```bash
hyperdu /path --top 20            # largest directories
hyperdu /path --json out.json     # structured output
hyperdu --compat gnu -sh /var/log # drop-in for du
```

Windows reads the NTFS `$MFT` directly when run elevated against a volume root,
which is faster still. Otherwise it uses `NtQueryDirectoryFile` batches.

See the [project README](https://github.com/automationjp/HyperDiskUsage#readme)
for the full option list.

## License

MIT
