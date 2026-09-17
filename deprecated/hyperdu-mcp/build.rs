// The crates.io description and README only reach someone who visits the page.
// A build-script warning is the one message `cargo install hyperdu-mcp` prints
// on the terminal it was typed into.
fn main() {
    println!(
        "cargo:warning=hyperdu-mcp is now the `hyperdu mcp` subcommand. Run: cargo install hyperdu"
    );
}
