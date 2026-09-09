---
name: disk-space-triage
description: Find out why a disk is full and what can safely be freed, using the HyperDU scanner. Use when a drive is out of space, a build fails with "no space left on device", or the user asks what is using their disk, which directories are largest, or what is safe to delete. Covers per-volume free space, largest-directory scans, and separating regenerable build output (Cargo target, node_modules, virtualenvs) from data that cannot be rebuilt.
license: MIT
compatibility: Requires the hyperdu binary. Run scripts/setup-hyperdu.sh (or setup-hyperdu.ps1 on Windows) to install it; that builds from source, so a Rust toolchain is needed. Prebuilt archives and crates.io packages exist too, listed in the repository README. Works on Windows, Linux, and macOS.
metadata:
  project: HyperDiskUsage
  repository: https://github.com/automationjp/HyperDiskUsage
---

# Disk space triage

Answer "why is the disk full and what can I delete" in a fixed order. Skipping
a step is what produces confident wrong answers.

This skill drives the `hyperdu` command. It does not need the MCP server —
if that server is available, its `list_volumes`, `scan_path`, and
`find_reclaimable` tools do the same work with typed arguments and structured
results, and you should prefer them. This file is the fallback that works with
nothing but a shell.

## Setup

The command is `hyperdu` (from the `hyperdu-cli` crate). If it is not on PATH,
install it before
doing anything else:

```bash
scripts/setup-hyperdu.sh
```

On Windows:

```powershell
.\scripts\setup-hyperdu.ps1
```

The script installs `hyperdu` and the `hyperdu-mcp` server, then prints the
command to register the MCP server with Claude Code or Codex. It only performs
that registration when passed `--register`, because rewriting an agent's
configuration should be something the user asked for.

The script builds from source and needs a Rust toolchain. If `cargo` is missing
it will say so and point at <https://rustup.rs> rather than failing halfway
through. Prebuilt archives and published crates do exist — the repository README
lists both — but the script needs two binaries on whatever platform it finds
itself on, and picking the right archive for each is not something it should
guess at.

To check without installing anything, use `--check`.

## The one rule

**Never delete anything without the user's explicit confirmation.** Report what
you found, say what deleting it would cost, and wait. Build output is
recoverable by rebuilding; a wrong `rm -rf` on someone's data is not.

## Step 1 — Which volume is short?

A size means nothing without capacity. 200 GiB is unremarkable on a 4 TiB disk
and an emergency on a 250 GiB one.

```bash
df -h
```

Identify the volume under pressure before scanning anything. If several are
tight, handle the one closest to zero first — a volume at 100% blocks writes
now, while one at 85% does not.

## Step 2 — What is large on it?

```bash
hyperdu /path/to/volume --top 20
```

Read the output as a tree: HyperDU rolls sizes up, so a parent's size includes
its children. The interesting rows are the ones where a parent is large but its
listed children are not — that is where the space hides.

Useful flags:

| Flag | Use it when |
|---|---|
| `--max-depth N` | The tree is huge and you only need the top levels |
| `--json PATH` | You want to compute on the result instead of reading it |
| `--csv PATH` | You want to sum a subset with `awk` |

`--json` emits objects with `path`, `logical`, `physical`, and `files`. Prefer
it over parsing the human-readable table, which is formatted for people and
will change.

Scanning a full 930 GB disk with 4 million files takes roughly 47 seconds. Use
`--max-depth` if you need an answer sooner.

Check `hyperdu --help` before inventing a flag. Guessing at one costs a full
scan to discover it does not exist.

## Step 3 — Drill into the largest subtree

Re-run the scan rooted at the biggest directory from step 2. Repeat until the
rows stop being surprising. Two or three rounds is normal.

## Step 4 — Separate what can be rebuilt

This is the step that turns a report into an action. Sort what you found into:

**Regenerable** — deleting costs only rebuild time:

| Directory | Rebuild with |
|---|---|
| `target/` (next to a `Cargo.toml`) | `cargo build` |
| `.cargo-target/` | `cargo build` |
| `node_modules/` (next to a `package.json`) | `npm install` or the project's package manager |
| `.venv/` | recreate the virtualenv, reinstall requirements |
| `__pycache__/`, `.mypy_cache/`, `.pytest_cache/` | regenerated on next run |
| `.gradle/` | `gradle build` |

**Not regenerable** — never propose deleting without asking what it is:
datasets, model caches, virtual disk images, media, anything under a path you
do not recognise.

`target` is an ordinary English word. A `target/` directory with no
`Cargo.toml` beside it is someone's data, not build output. Check for the
sibling manifest before classifying it.

## Step 5 — Exclude anything still in use

Before proposing a deletion, filter out directories a build is currently
writing to. Deleting the output directory of a running `cargo build` breaks
that build.

Check a single candidate — this exits at the first recent file, so it stays
fast on active directories:

```bash
find /path/to/target -mtime -1 -print -quit
```

Empty output means nothing was touched in the last day, so the directory is
idle and safe to propose.

When the user gives an age threshold ("anything unused for a day"), apply it
literally and report both what qualified and what you held back, so they can
see the filter worked.

## Step 6 — Report, then wait

Give the user:

1. Free space before, per volume
2. What you propose deleting, with sizes and a total
3. What rebuilding it would cost
4. What you deliberately left alone, and why

Then stop and ask. After they confirm, delete, and report the recovered space
by re-running `df -h`.

## Recurring cause worth naming

If you find many `target/` directories across git worktrees, say so. Each
worktree builds independently and produces its own copy — 20 to 50 GiB each —
so the same space is consumed again with every new worktree. Setting a shared
`CARGO_TARGET_DIR` prevents the recurrence, which is worth more than the
one-time cleanup.
