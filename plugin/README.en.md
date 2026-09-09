# HyperDU for agents

[日本語](README.md) · **English** · [简体中文](README.zh-CN.md)

Install **one command**, use it as a CLI or an MCP server. The published crate
and executable are both named `hyperdu`; the MCP server is `hyperdu mcp`.
The standalone `hyperdu-mcp` binary is replaced in 0.5.0-beta.3.

## Install from this checkout

```sh
cargo install --locked --path hyperdu-cli
hyperdu --help
hyperdu mcp --help
```

Rust 1.88+ is required. Once this release is published, use
`cargo install hyperdu --version 0.5.0-beta.3` instead.

The setup scripts install from the checkout when present, otherwise from GitHub:

```sh
plugin/skills/disk-space-triage/scripts/setup-hyperdu.sh
```

```powershell
.\plugin\skills\disk-space-triage\scripts\setup-hyperdu.ps1
```

`--check` / `-Check` only reports readiness. `--register` / `-Register` additionally
registers with detected clients. By default registration commands are only printed.

## Enable MCP in your client

```sh
codex mcp add hyperdu -- hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
```

Other stdio clients use `command: "hyperdu"`, `args: ["mcp"]`, as in `mcp.json`.
The server starts only when requested and leaves ordinary CLI scans unchanged.

| Tool | Question |
|---|---|
| `list_volumes` | Which volume is running out of space? |
| `scan_path` | What is large inside the directory? |
| `find_reclaimable` | Which outputs might be regenerated? |

`unused_for_days` filters sampled modification timestamps. It is an activity
heuristic, not proof that a build is stopped or deletion is safe. Inspect candidates
and confirm with the user. **There is no delete tool.**

## Independent interfaces

- MCP works without the plugin or Skill.
- `skills/disk-space-triage/` uses the CLI and needs no MCP registration.
- `plugin.json` and `mcp.json` package the interfaces for agent clients.
- `hyperdu-core` remains the reusable scanning/domain library.
