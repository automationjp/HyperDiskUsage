//! Retired stub. `hyperdu-cli` was renamed to `hyperdu`.

const NOTICE: &str = "\
hyperdu-cli has been renamed to `hyperdu`.

    cargo install hyperdu

The command is still called `hyperdu`. The MCP server that used to ship as a
separate `hyperdu-mcp` binary is now the `hyperdu mcp` subcommand.

    https://github.com/automationjp/HyperDiskUsage
";

// Exits non-zero so a script that still calls this stops instead of carrying on
// with no output.
fn main() {
    eprint!("{NOTICE}");
    std::process::exit(1);
}
