# hyperdu-cli is deprecated

This crate was renamed to [`hyperdu`](https://crates.io/crates/hyperdu).

```sh
cargo install hyperdu
```

The command is still called `hyperdu`; only the crate name changed. The MCP
server that used to ship as a separate `hyperdu-mcp` binary is now the
`hyperdu mcp` subcommand.

`0.5.0-beta.2` is the last release with working code. This version installs a
binary named `hyperdu-cli` that prints this notice and exits non-zero — it is
deliberately not called `hyperdu`, so installing it cannot overwrite a working
`hyperdu`.

Source, releases and prebuilt packages:
<https://github.com/automationjp/HyperDiskUsage>
