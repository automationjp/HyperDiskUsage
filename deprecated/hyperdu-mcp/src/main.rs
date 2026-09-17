//! Retired stub. `hyperdu-mcp` is now the `hyperdu mcp` subcommand.

const NOTICE: &str = "\
hyperdu-mcp no longer exists as a separate binary. The MCP server is now a
subcommand of `hyperdu`.

    cargo install hyperdu
    hyperdu mcp

Register it with an agent by running `hyperdu mcp` over stdio; the bundled
plugin/mcp.json declares exactly that.

    https://github.com/automationjp/HyperDiskUsage
";

// Exits non-zero so an MCP client that still launches this fails its handshake
// visibly instead of hanging on a server that will never speak.
fn main() {
    eprint!("{NOTICE}");
    std::process::exit(1);
}
