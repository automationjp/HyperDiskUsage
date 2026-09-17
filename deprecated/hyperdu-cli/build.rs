// The crates.io description and README only reach someone who visits the page.
// A build-script warning is the one message `cargo install hyperdu-cli` prints
// on the terminal it was typed into.
fn main() {
    println!("cargo:warning=hyperdu-cli has been renamed to `hyperdu`. Run: cargo install hyperdu");
}
