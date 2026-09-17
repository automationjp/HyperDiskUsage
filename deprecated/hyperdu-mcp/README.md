# hyperdu-mcp is deprecated

The MCP server is now a subcommand of
[`hyperdu`](https://crates.io/crates/hyperdu) rather than a separate binary.

```sh
cargo install hyperdu
hyperdu mcp
```

`0.5.0-beta.2` is the last release with working code. This version installs a
`hyperdu-mcp` binary that prints this notice and exits non-zero, so an agent
still configured to launch it fails visibly instead of waiting on a server that
never answers.

Source, releases and prebuilt packages:
<https://github.com/automationjp/HyperDiskUsage>
