# 面向 AI 助手的 HyperDU

[日本語](README.md) · [English](README.en.md) · **简体中文**

一次安装即可使用 CLI 和 MCP 服务器。发布的 crate 和可执行命令均名为 `hyperdu`，
MCP 服务器通过 `hyperdu mcp` 启动。0.5.0-beta.3 将取代独立的 `hyperdu-mcp` 可执行文件。

## 从当前源码安装

```sh
cargo install --locked --path hyperdu-cli
hyperdu --help
hyperdu mcp --help
```

需要 Rust 1.88+。发布后可使用 `cargo install hyperdu --version 0.5.0-beta.3`。
安装脚本优先使用当前源码；找不到源码时从 GitHub 安装。

```sh
plugin/skills/disk-space-triage/scripts/setup-hyperdu.sh
```

```powershell
.\plugin\skills\disk-space-triage\scripts\setup-hyperdu.ps1
```

`--check` / `-Check` 仅检查安装状态。`--register` / `-Register` 会向检测到的客户端注册。
默认只显示注册命令，不进行注册。

## 在客户端中启用 MCP

```sh
codex mcp add hyperdu -- hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
```

其他 stdio 客户端可按 `mcp.json` 指定 `command: "hyperdu"`、`args: ["mcp"]`。
服务器只在请求时启动，不改变常规 CLI 扫描的使用方式。

| 工具 | 用途 |
|---|---|
| `list_volumes` | 查看哪些卷的空间不足 |
| `scan_path` | 查看目录内哪些项目最大 |
| `find_reclaimable` | 查找可能可以重新生成的产物 |

`unused_for_days` 根据采样的修改时间过滤候选项，不能证明构建已停止或删除是安全的。
应检查候选项并由用户决定是否删除。**不提供删除工具。**

## 各入口可独立使用

- MCP 无需 Plugin 或 Skill 即可运行。
- `skills/disk-space-triage/` 使用 CLI，无需注册 MCP。
- `plugin.json` 和 `mcp.json` 为助手客户端提供打包配置。
- `hyperdu-core` 保持为独立、可复用的扫描与领域逻辑库。
