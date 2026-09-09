# hyperdu

[日本語](README.md) · [English](README.en.md) · **简体中文**

跨平台磁盘使用分析工具，使用操作系统专用的快速枚举接口和并行遍历。

## 性能

查看 Windows / NTFS 与 Linux / WSL2 / ext4 的[测量结果及限制](../docs/zh-CN/benchmarks.md)。
冷缓存及 XFS 尚未测量。实现原理见[性能设计](../docs/zh-CN/performance.md)。

## 安装

```bash
cargo install hyperdu --version 0.5.0-beta.3
```

crate 和可执行命令均名为 `hyperdu`。一次安装包含 CLI 和 MCP；只有运行 `hyperdu mcp` 时
才会启动 MCP 服务器。需要 Rust 1.88+。

此版本正在准备发布。现在请从仓库根目录运行 `cargo install --locked --path hyperdu-cli`。
上面的 registry 安装命令在发布后可用。运行预编译包无需 Rust。
MSVC/SDK 及 Linux 依赖见[环境配置](../docs/zh-CN/setup.md)。

## 使用

```bash
hyperdu /path --top 20
hyperdu /path --json out.json
hyperdu --compat gnu -sh /var/log
hyperdu mcp
hyperdu -- mcp
```

依次为显示最大目录、导出 JSON、GNU du 兼容模式、启动 MCP 服务器，以及扫描名称恰好为 `mcp` 的目录。

## 快速扫描的实现

- Linux：`getdents64` + `statx`
- Windows：使用 `NtQueryDirectoryFile` 批量获取分配大小和文件 ID
- macOS：`getattrlistbulk`
- 每个工作线程的 LIFO 队列与工作窃取
- 满足条件时直接读取 NTFS `$MFT`

MFT 无法安全解析出完整结果时，会回退到常规目录枚举。
安装形式、GUI、平台状态和限制见[项目 README](../README.zh-CN.md)。

## 许可证

MIT
