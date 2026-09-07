# hyperdu-mcp

A [Model Context Protocol](https://modelcontextprotocol.io/) server that lets an
agent answer "why is this disk full, and what can I safely delete" with typed
tools instead of parsing `du` output.

## Install

```bash
cargo install hyperdu-mcp --version 0.5.0-beta.2

claude mcp add --transport stdio hyperdu -- hyperdu-mcp   # Claude Code
codex mcp add hyperdu -- hyperdu-mcp                      # Codex
```

Any MCP client works; the server speaks stdio and holds no state between calls.

## Tools

| Tool | Answers |
|---|---|
| `list_volumes` | Which volume is short on space |
| `scan_path` | What is large inside a directory tree |
| `find_reclaimable` | Which of that can be rebuilt rather than lost |

`find_reclaimable` recognises Cargo target directories, `node_modules`,
virtualenvs and caches, and reports how many days each has sat untouched. Pass
`unused_for_days` before proposing a deletion: a directory an in-progress build
is writing to is never idle, so the filter is what keeps a suggestion from
destroying a running job.

It does not classify `target/` by name alone. Without a `Cargo.toml` beside it,
that directory is someone's data, and reporting it as reclaimable is the failure
that would make the tool unusable.

## There is no delete tool

Deliberately. Exposing one would create a path for an agent to destroy data
without a human in the loop. These tools report; a person decides. A wrong
report can be corrected, a wrong `rm -rf` cannot.

## License

MIT
