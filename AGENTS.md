# Repository Guidelines

## Project Structure & Module Organization
HyperDiskUsage is a Rust workspace with three crates: `hyperdu-core/` hosts the scanning engine with shared benchmarks in `benches/`; `hyperdu-cli/` exposes the CLI with fixtures in `tests/`; `hyperdu-gui/` packages the eGUI front-end.
Distribution artifacts sit in `dist/`, while `packaging/`, `scripts/`, and `snap/` capture installer specs and automation. Keep generated output in `target/` out of version control.

## Build, Test, and Development Commands
`cargo check --workspace` gives a fast compile sanity pass. Run `cargo fmt --check` to enforce formatting and `cargo clippy --workspace --all-targets --all-features` to lint against the MSRV pinned in `clippy.toml`.
Use `cargo test --workspace --all-features` for the full suite. Manual checks include `cargo run -p hyperdu-cli -- --help` and `cargo run -p hyperdu-gui --release`. Package builds via `pwsh scripts/release.ps1 -Profile release` should remain idempotent and reproducible.

## Coding Style & Naming Conventions
Follow Rust 2021 defaults with four-space indentation and a 100-character line limit from `rustfmt.toml`.
Use `snake_case` for modules, functions, and files; `PascalCase` for types and traits; `SCREAMING_SNAKE_CASE` for constants and feature flags.
Group `use` imports by crate, std, then third-party crates and let `cargo fmt` normalize ordering. Public API docs stay in English, while inline comments may mirror the bilingual tone when it clarifies OS-specific behavior.

## Testing Guidelines
Keep fast unit tests beside implementation files and broader integration coverage under each crate's `tests/` directory.
Name test files after the behavior under scrutiny (e.g. `cli_outputs.rs`, `walker_windows.rs`) to keep reports readable.
When touching the filesystem, favor the `tempfile` or `assert_fs` crates to stay hermetic. Guard platform-specific logic with `cfg(target_os = "...")` and document assumptions in the test header.
Note manual verification steps in PRs when automation is not practical.

## Commit & Pull Request Guidelines
Commits use short, imperative summaries—recent history mixes English and Japanese, so follow the same concise style (e.g. `fix: adjust bundle flags`).
Provide context in the body when it aids reviewers and reference issues with `Refs #123` or `Fixes #123`.
Pull requests should cover: 1) change overview, 2) validation steps or command output, 3) screenshots or GIFs for GUI updates, and 4) risk or rollback notes for core scanning touches.
Update `dist/` metadata and packaging manifests whenever binaries or installers change.
