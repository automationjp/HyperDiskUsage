# POSIX du compatibility audit

[日本語](../posix-compatibility.md) | [English](../en/posix-compatibility.md) | [简体中文](../zh-CN/posix-compatibility.md)

Verdict: the current CLI is not fully POSIX du compatible.

The default mode prints a HyperDU-specific table and summary. The name `--compat posix-strict` does not guarantee full conformance.

| Item | Observed result |
|---|---|
| Required `-a`, `-s`, `-H`, `-L` options | Not implemented. On the Linux release, `--compat posix-strict -s <path>` exits 2 with unexpected argument. |
| `-k`, `-x` | Accepted by the CLI; this is not certification of every boundary case. |
| Default units | The posix-strict code selects 512-byte units. Default HyperDU mode has its own output. |
| Missing path | Linux release prints a stderr diagnostic and exits 1. |
| Complete semantics | Full conformance, including file operands, symlinks, directory allocation and option precedence, has not been established. |
| Allocation mismatch | With a 4096-byte file, a hard link, two allocated symlinks and two directories, du reported 40 root blocks while HyperDU reported 8. Directory and non-followed symlink allocation is omitted. |
| Symlink behavior | A symlink operand failed with errno20. With --follow-links, a dangling link was silently skipped with exit0; du -L diagnosed it and exited1. |
| Output separator | posix-strict uses TAB rather than the specified space. |

The nonworking -sh / -ak examples were corrected in the README and site, and the du alias recommendation was removed. This audit does not implement full compatibility.

[POSIX.1-2017 / Open Group du specification](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/du.html)

Audit: 2026-09-09 / release source `0a089c90c0e2995ef702ff053820d129265c3659` / WSL2 Ubuntu 26.04.
