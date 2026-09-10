# Repository Guidelines

## Project Structure & Module Organization
HyperDiskUsage is a Rust workspace with three crates: `hyperdu-core/` hosts the scanning engine and domain logic; `hyperdu/` publishes the `hyperdu` CLI with its built-in `mcp` subcommand and integration fixtures in `tests/`; `hyperdu-gui/` packages the eGUI front-end.
`plugin/` carries the agent-facing packaging: an Agent Plugin manifest, its `mcp.json`, and an Agent Skill under `skills/`. These are data files, not build targets. The three agent surfaces are deliberately independent — the MCP server runs without the plugin, and the skill drives the CLI rather than the server — so keep a change to one from requiring changes to the others.
Distribution artifacts sit in `dist/`, while `packaging/`, `scripts/`, and `snap/` capture installer specs and automation. Keep generated output in `target/` out of version control.

## Build, Test, and Development Commands
`cargo check --workspace` gives a fast compile sanity pass. Run `cargo fmt --check` to enforce formatting and `cargo clippy --workspace --all-targets` to lint. Clippy takes the MSRV from each crate's own `rust-version`, since `hyperdu` needs 1.88 for its built-in MCP server while core and GUI declare 1.75. Every other package field (version, edition, license, repository, homepage) and the `hyperdu-core` dependency pin are inherited from `[workspace.package]` / `[workspace.dependencies]` in the root `Cargo.toml`, so a release bumps `version` there (twice: the package field and the dependency pin) and in no crate manifest. Scripts that need the version ask `cargo pkgid -p hyperdu` instead of grepping a manifest. Run `cargo update -w` in the same commit as a version bump so `Cargo.lock` stays synchronized. Publish order is `hyperdu-core` first, then `hyperdu`, `hyperdu-gui`.

The packaging and documentation tree also carries version literals. `scripts/lint/versions.sh` is the authoritative inventory: files describing the current tree (man page, snap manifest/generator, plugin manifest, winget generator) track the workspace version, while the Scoop bucket tracks the latest published release and may legitimately lag between a version bump and release publication. The current README and site describe the workspace release under an explicit publication-pending notice; their examples track the workspace version. Run `bash scripts/lint/versions.sh --fix` for the workspace group only; never bump the published-release group until the corresponding assets exist. `scripts/lint/run.sh` executes this gate before Cargo linting so a stale `Cargo.lock` cannot be silently refreshed first. `scripts/release/tag.sh` also rejects tags that do not equal the locked workspace version.

Use `cargo test --workspace` for the default suite. Also test the parallel configuration with `cargo test --workspace --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch`. The `profiling` dependency supports only one backend at a time, so `--all-features` is not a valid configuration: it enables both Tracy and Puffin and produces duplicate macro definitions. Check them separately with `cargo check --workspace --features hyperdu-core/prof-tracy` and `cargo check --workspace --features hyperdu-core/prof-puffin`; never report that as a successful `--all-features` run. See `docs/old/design/mft-parity-verification.md` for the reproduced baseline conflict.
Manual checks include `cargo run -p hyperdu -- --help` and `cargo run -p hyperdu-gui --release`. Package builds via `pwsh scripts/package/release.ps1 -Profile release` should remain idempotent and reproducible.

For developer machine prerequisites, Rust versions, Windows MSVC/SDK setup, Linux native build tools, GUI desktop dependencies, and MCP-specific requirements, see `docs/setup.md`.

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
