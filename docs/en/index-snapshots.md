# Linux directory snapshots (experimental)

[日本語](../index-snapshots.md) | [English](../en/index-snapshots.md) | [简体中文](../zh-CN/index-snapshots.md)

HyperDU's `index` subcommand is an experimental Linux feature that **stores a directory aggregate after one scan and reads it next time without rescanning the file tree**.

> This is not a watcher. It does not update automatically or run as a resident monitor. Stored values are always treated as `stale`.

## Quick start

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

- `refresh`: scan the tree with the ordinary scanner and save a snapshot for each directory
- `show`: read the saved snapshot and return the result without rescanning the target tree

## When to use it

Good fits:

- You want to repeatedly check an approximate total for a tree with a very large number of files
- The value from the last explicit refresh is sufficient
- You do not want to install an automatic monitoring daemon

Poor fits:

- You always need the latest value
- You need an atomic filesystem snapshot
- You want to track file changes in real time

## What `stale` means

`show` reads only saved data. Files created, changed, or deleted after `refresh` are not reflected.

`refresh` itself is also not an atomic filesystem snapshot. The tree may change while it is being scanned.

For that reason, the output explicitly includes:

```text
freshness: "stale"
monitoring: false
```

`stale` is not an error. It means **a stored value for which an exact match with the current filesystem is not guaranteed**.

## Database rules

The database has the following constraints for safe storage and reuse:

- The parent directory must already exist
- The database must be a regular file
- Symlinks are rejected
- Place the database outside the scan root
- Use a separate database for each tree

These constraints mean that `/` itself cannot be used as a snapshot root.

## Root identity

HyperDU confirms that the device/inode identity of the mount root matches between saving and loading.

If the identity changes because of a remount, restore, root replacement, or similar operation, run `refresh` again.

Because inodes may be reused, this is a consistency check rather than a freshness guarantee.

## Failure behavior

When `refresh` does not complete successfully, HyperDU prioritizes avoiding replacement of an existing snapshot with incomplete values.

Examples:

- Cancellation
- A scan error
- A bind-mount alias that cannot be represented safely
- The root identity cannot be established

## Cost model

The read cost of `show` is **O(indexed directories)**. It does not rescan every file.

The root lookup after loading is O(1).

## CLI note

`index` is a subcommand name. To scan an ordinary directory actually named `index`, be explicit.

```sh
hyperdu ./index
# or
hyperdu -- index
```

Options for a normal scan do not apply unchanged to the fixed semantics of `index refresh` / `index show`.

## What is not implemented

The current implementation does not include:

- inotify watcher
- background daemon
- automatic refresh
- overflow recovery
- watch budget policy

These are separate features for future consideration. The historical persistent-index / watcher design is kept in [old documentation](../old/README.md).

## Related docs

- [Architecture](architecture.md)
- [Performance design](performance.md)
- [Historical design records](../old/README.md)
