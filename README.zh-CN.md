# HyperDU

> **快速找出是什么占满了磁盘。**<br>
> 用 Rust 编写的高速跨平台磁盘使用量分析器。提供 CLI、GUI、GNU `du` 兼容模式，以及面向 AI agent 的 MCP / Skill 接口。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.88%2B-black?logo=rust)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Windows%20%7C%20Linux-tested-blue)](#platform-status)

[日本語](README.md) · [English](README.en.md) · **简体中文**

[安装与环境](docs/zh-CN/setup.md) · [性能](docs/zh-CN/performance.md) · [基准测试计划](docs/zh-CN/benchmarks.md) · [文档](docs/zh-CN/README.md) · [网站](https://automationjp.github.io/HyperDiskUsage/) · [日文网站](https://automationjp.github.io/HyperDiskUsage/)

---

## 快速开始

参数默认值、输出模式及平台限制见 [CLI 参数参考（日语）](docs/cli-reference.md)。`--time` 系列参数需要默认启用的 `time-format` feature。

> **0.5.0-beta.3 正在准备发布。** 在当前分支可以使用 `cargo install --locked --path hyperdu` 安装。下方的 crates.io 命令和新名称发行文件将在版本正式发布后可用。

```bash
cargo install hyperdu --version 0.5.0-beta.3
hyperdu . --top 20
```

crate 名称和可执行文件名称都是 **`hyperdu`**。它会同时安装 CLI 和 MCP 服务器。

工作区有三个面向用户的 crate：`hyperdu-core` 是共享扫描引擎，`hyperdu` 是内置 MCP 服务器（`hyperdu mcp`）的 CLI 二进制包，`hyperdu-gui` 是独立的 GUI 包。

```bash
# 分析当前目录
hyperdu .

# 显示最大的 20 个项目
hyperdu /path/to/data --top 20

# 导出为 JSON
hyperdu /path/to/data --json result.json

# GNU du 兼容模式
hyperdu --compat gnu -k /var/log
```

运行 `hyperdu --help` 查看全部选项。

## 性能

HyperDU 的核心目标是 **高速磁盘使用量分析**。

它并非只是用 Rust 重写 `du`，而是针对各操作系统的目录枚举、元数据获取、并行遍历以及物理大小统计等热路径进行优化。

### HyperDU 与 du 比较

正在准备最新版AWS EC2测量。只发布与GNU du直接一致的目录字节总量：物理分配量、不追踪符号链接、硬链接去重、单一filesystem、输出全部目录行。旧WSL2测量需要外部统计补正，已撤回，不再作为速度比较依据。 [Details](docs/zh-CN/benchmarks.md)

### Why it is fast

| 平台 | 主要路径 | 优化方式 |
|---|---|---|
| Linux | `getdents64` + `statx` | 批量获取目录项，并高效并行汇总元数据 |
| Windows | `NtQueryDirectoryFile` / `FileIdFullDirectoryInformation` | 批量获取名称、大小、分配大小和文件 ID |
| macOS | `getattrlistbulk` | 批量获取元数据 |

此外，`hyperdu-core` 使用每个 worker 的 LIFO 双端队列和 work stealing，根据目录树的不均衡程度重新分配工作。

在 Windows 上，当满足 NTFS 卷根目录和权限等条件时，指定 `--mft` 可启用直接读取 `$MFT` 的路径。如果无法安全分析，则回退到目录枚举。

优化设计详情见[性能设计](docs/zh-CN/performance.md)，组件之间的关系见[架构](docs/zh-CN/architecture.md)。

## HyperDU 提供的能力

### 快速磁盘分析

- 统计逻辑大小 / 物理分配大小
- 硬链接去重
- 多线程扫描 + work stealing
- 按文件系统选择扫描策略
- 排除 / 最大深度 / 最小文件大小
- JSON / CSV 输出
- basic / deep 分类
- 进度显示

### GNU `du` 兼容模式

```bash
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -k /home --max-depth=2
hyperdu --compat gnu -b --time /usr/share
```

HyperDU 尚未完全兼容 POSIX `du`。`--compat posix-strict` 选择512字节等默认输出设置，但尚未实现 POSIX 必需的 `-a`、`-s`、`-H` 和 `-L`。请勿将其作为 `du` 的直接替代或别名。

我们会持续测试兼容性，但不保证无条件、完整复现 GNU coreutils 的全部行为。

### 结构化输出

```bash
hyperdu /srv/data --json usage.json
hyperdu /srv/data --csv usage.csv
```

## AI agent：MCP / Skill / Plugin

HyperDU 也可以供 Claude、Codex 等 AI agent 使用。

| 接口 | 用途 | 是否需要 MCP？ |
|---|---|---|
| MCP server | 类型化参数 / 结构化结果 | 是 |
| Agent Skill | 使用 `hyperdu` CLI 的 triage 工作流 | 否 |
| Agent Plugin | 将 MCP + Skill 一起分发 | 可选 |

MCP server 提供以下 3 个工具：

| 工具 | 回答的问题 |
|---|---|
| `list_volumes` | 哪个 volume 快要耗尽空间？ |
| `scan_path` | 其中什么项目占用空间最多？ |
| `find_reclaimable` | 哪些候选内容可以重新生成？ |

**这里有意不提供删除工具。** 这样可以避免 agent 在没有人工确认的情况下破坏数据。

```bash
cargo install hyperdu --version 0.5.0-beta.3
claude mcp add --transport stdio hyperdu -- hyperdu mcp
# Codex：
codex mcp add hyperdu -- hyperdu mcp
```

Agent Skill / Plugin 请参阅 [plugin/README.zh-CN.md](plugin/README.zh-CN.md)。

## GUI

`hyperdu-gui` 是基于 `egui` / `eframe` 的桌面 UI。

它提供 Interactive（默认）和 Batch 两种扫描模式，共享相同的核心语义，并提供高级过滤与性能选项以及 JSON/CSV 导出。

<!-- 父任务：发布前核对最终 GUI 功能表述。 -->

- 实时扫描
- 交互式树视图
- 目录下钻
- 吞吐量显示
- 结果导出

```bash
cargo install hyperdu-gui --version 0.5.0-beta.3
hyperdu-gui
```

## 安装

**运行预构建二进制文件、使用 Scoop 或安装 `.deb` 包都不需要 Rust。** 源码构建所需的 Rust 版本、Windows MSVC / Windows SDK、Linux 原生构建工具、GUI 依赖和 MCP 环境见[安装与构建环境](docs/zh-CN/setup.md)。

### crates.io

```bash
# CLI + MCP（Rust 1.88+）
cargo install hyperdu --version 0.5.0-beta.3

# GUI（Rust 1.75+）
cargo install hyperdu-gui --version 0.5.0-beta.3

# 仅在需要时启动 MCP server
hyperdu mcp
```

### 预构建二进制文件

发布后可从 [Releases](https://github.com/automationjp/HyperDiskUsage/releases) 获取。以下是下一版本计划使用的文件名。

| 平台 | CLI | GUI |
|---|---|---|
| Windows x86_64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.zip) / [exe](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-windows-x86_64-generic.exe) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-windows-x86_64-generic.zip) |
| Linux x86_64 (glibc) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-generic.zip) |
| Linux x86_64 (musl) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-x86_64-musl-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-x86_64-musl-generic.zip) |
| Linux aarch64 | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-linux-aarch64-generic.zip) | [zip](https://github.com/automationjp/HyperDiskUsage/releases/download/v0.5.0-beta.3/hyperdu-gui-linux-aarch64-generic.zip) |

同一 release 中也会提供 Debian / Ubuntu 使用的 `.deb` 包。

### Scoop（Windows）

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

### winget（Windows）— 正在申请

```powershell
winget install automationjp.HyperDU
```

manifest 已通过 `winget validate`，但在完成向 winget-pkgs 注册之前无法使用。

### 从源码安装

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo install --path hyperdu
```

详细的构建环境见 [docs/zh-CN/setup.md](docs/zh-CN/setup.md)。

<a id="platform-status"></a>
## 平台状态

| 平台 | 状态 | 备注 |
|---|---|---|
| Windows | **已测试** | NTFS、CI、原生枚举、可选 MFT 路径 |
| Linux | **已测试** | XFS / ext4、CI |
| macOS | CLI：**未验证** / GUI：**无法构建** | 已有 `getattrlistbulk` 实现；当前 release workflow 不包含 macOS |

最低 Rust 版本：

- `hyperdu-core`、`hyperdu-gui`：**Rust 1.75+**
- `hyperdu`（CLI + MCP）：**Rust 1.88+**
- 整个 workspace 的构建/测试：**Rust 1.88+**

## 实验性功能：持久化 Linux 快照

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

这不是 watcher。`show` 返回保存的值，并始终将 freshness 标记为 `stale`。

详情见 [Linux 目录快照](docs/zh-CN/index-snapshots.md)。

## 文档

- [安装与构建环境](docs/zh-CN/setup.md)
- [文档索引](docs/zh-CN/README.md)
- [性能设计](docs/zh-CN/performance.md)
- [基准测试计划 / 重新测量清单](docs/zh-CN/benchmarks.md)
- [架构](docs/zh-CN/architecture.md)
- [Linux 持久化快照](docs/zh-CN/index-snapshots.md)
- [历史 / 旧文档](docs/old/README.md)
- [Agent Plugin / Skill](plugin/README.zh-CN.md)

## 开发

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
```

整个 workspace 的开发使用 Rust 1.88+。平台特定的前置条件和可选 feature 的检查方法见[安装与构建环境](docs/zh-CN/setup.md)。

修改性能路径的 PR 除了常规测试外，还应检查[基准测试计划](docs/zh-CN/benchmarks.md)中的正确性门槛和重新测量条件。

## 已知限制

- 当前为 beta 版本。CLI 选项、MCP 工具 schema 和输出格式可能变化。
- 公开 benchmark 正在重新测量。更新性能数据前必须通过 benchmark gate。
- macOS 的性能与兼容性验证尚未完成。
- 在网络文件系统或 HDD 上，I/O 延迟可能占主导地位，从而减小并行化带来的收益。
- 默认不跟踪 symbolic link。使用 `--follow-links` 时请注意 cycle。

## 许可证

[MIT License](LICENSE)

## 致谢

- [ripgrep](https://github.com/BurntSushi/ripgrep) — 高性能文件系统/搜索实现
- [fd](https://github.com/sharkdp/fd) — 并行文件系统遍历
- [dust](https://github.com/bootandy/dust) — 磁盘使用量 UX

---

**HyperDU — 为人类和 agent 提供快速磁盘分析。**

[POSIX du compatibility audit](docs/zh-CN/posix-compatibility.md)
