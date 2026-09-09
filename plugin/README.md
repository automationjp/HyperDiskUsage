# HyperDU for agents

Disk usage an agent can act on: which volume is short, what is large on it, and
which of that can be deleted without losing anything.

## What is in here

| Path | Format | What it is |
|---|---|---|
| `plugin.json` | [Agent Plugins](https://agent-plugins.org/) | The bundle manifest |
| `mcp.json` | [Agent Plugins](https://agent-plugins.org/) | Declares the `hyperdu-mcp` stdio server |
| `skills/disk-space-triage/` | [Agent Skills](https://agentskills.io/) | The triage procedure, driving the CLI |
| `skills/disk-space-triage/scripts/` | — | Setup for both binaries and the MCP server |

The three surfaces are independent on purpose. The MCP server runs without the
plugin, and the skill drives `hyperdu` rather than the server, so **you can
adopt any one of them without the other two**.

## Setup

Everything here needs at least one HyperDU binary. The bundled script builds
the binaries it needs with `cargo`, so a [Rust toolchain](https://rustup.rs) is
required for that script.

Prebuilt release archives currently cover the CLI/GUI, while `hyperdu-mcp` is
published as a crate but is not attached to the release as a prebuilt binary.
If you only use the skill, installing the CLI archive is sufficient. For the
full bundled setup, either use Cargo or provision both `hyperdu` and
`hyperdu-mcp` yourself and re-run the script with `--check`. The root README
carries the pinned installation commands; a pre-release needs an explicit
`--version`.

The bundled script installs both binaries and shows you how to register the MCP
server:

```bash
skills/disk-space-triage/scripts/setup-hyperdu.sh
```

```powershell
.\skills\disk-space-triage\scripts\setup-hyperdu.ps1
```

| Flag | Effect |
|---|---|
| *(none)* | Install what is missing, print the registration command |
| `--check` | Report what is installed, change nothing |
| `--register` | Install, then register with any detected client |

Registration is opt-in because it rewrites an agent's configuration. Installing
a binary is undone with `cargo uninstall`; quietly editing someone's client
config is not something a setup script should do uninvited.

### Doing it by hand

```bash
cargo install --git https://github.com/automationjp/HyperDiskUsage hyperdu-cli
cargo install --git https://github.com/automationjp/HyperDiskUsage hyperdu-mcp
```

The crate is `hyperdu-cli`; the command it installs is `hyperdu`. Same shape as
ripgrep installing `rg`, and it matches the deb and rpm packages.

Then register the MCP server with your client:

```bash
claude mcp add --transport stdio hyperdu -- hyperdu-mcp
```

```bash
codex mcp add hyperdu -- hyperdu-mcp
```

For any other MCP client, add a stdio server whose command is `hyperdu-mcp` —
which is exactly what `mcp.json` already declares.

## The MCP tools

| Tool | Answers |
|---|---|
| `list_volumes` | Which volume is short on space |
| `scan_path` | What is large inside a directory tree |
| `find_reclaimable` | Which of it can be rebuilt rather than lost |

`find_reclaimable` takes `unused_for_days`. Pass 1 or more before proposing any
deletion: a directory an in-progress build is writing to is never idle, so that
filter is what keeps a suggestion from destroying a running job.

**There is no delete tool, deliberately.** These report; a human decides. A
wrong report can be corrected, a wrong `rm -rf` cannot.

## Using only the skill

Copy `skills/disk-space-triage/` anywhere your agent looks for skills. It needs
`hyperdu` and nothing else from this directory — not the MCP server, not the
plugin manifest.

## Using only the MCP server

Install `hyperdu-mcp`, register it, and ignore the rest of this directory. Any
MCP client works: the server speaks stdio and holds no state between calls.
