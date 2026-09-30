# HyperDU

[日本語](README.md) · [English](README.en.md) · **简体中文**

> **找出磁盘空间去向，让人、应用和 AI 助手共用一个扫描引擎。**
> HyperDU 使用 Rust，将操作系统专用元数据读取与并行遍历结合起来。通过 CLI 检查、GUI 逐层探索，或通过 MCP 向 AI 助手提供结构化结果。

[![CI](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml/badge.svg)](https://github.com/automationjp/HyperDiskUsage/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/hyperdu.svg)](https://crates.io/crates/hyperdu)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

[网站](https://hyperdu.automation.jp/zh-CN/) · [环境设置](docs/zh-CN/setup.md) · [开发者指南（日语）](docs/developer-guide.md) · [CLI 参考（日语）](docs/cli-reference.md) · [文档](docs/zh-CN/README.md)

## 使用已发布的 Beta 版

**v0.5.0-beta.5 已经发布**，可从 [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5) 和 crates.io 获取。它不是尚未发布的预览，也不是稳定版：CLI 参数、MCP schema 和输出格式仍可能变化。

```bash
cargo install hyperdu --locked --version 0.5.0-beta.5
hyperdu . --top 20
```

crate 和可执行文件的名称都是 **`hyperdu`**，不是已废弃的 `hyperdu-cli`。一次安装包含 CLI 和 MCP 服务；只有执行 `hyperdu mcp` 时才启动服务。

**运行 Windows / Linux 预构建二进制文件不需要 Rust。** 源码构建请使用较新的 stable Rust 和对应平台的构建工具。当前源码声明 core 最低1.82、GUI 最低1.85、CLI 最低1.88，并在 Linux / Windows CI 中逐项检查。此修正不会改写已发布 Beta 包的 manifest；安装公开包时请使用较新的 stable Rust。

## 与常规实现有什么不同？

区别不只是“用 Rust 编写”，而是同时设计**元数据获取方式、任务分配方式和结果复用方式**。

| 问题 | HyperDU 的实现 | 对开发者的意义 |
|---|---|---|
| 列出名称后逐个查询文件会增加系统调用 | Windows `NtQueryDirectoryFile` 批量获取名称、大小、分配大小和文件 ID | 减少额外的文件打开。Linux 使用 `getdents64` 与 `statx`，但不会消除必要的元数据读取 |
| 固定任务分配可能使一个线程独自处理大子树 | 每个工作线程使用 LIFO 队列和 work stealing | 空闲线程接手可用任务，处理不均衡的目录树 |
| CLI、GUI、AI 各自实现扫描容易造成统计差异 | 都调用独立的 `hyperdu-core` 库 | 共享硬链接处理、汇总规则和平台优化 |
| 解析面向人的终端文本，难以建立稳定的集成契约 | 内置 MCP 工具提供类型化参数和结构化结果 | 便于程序使用，且没有删除工具 |
| GUI 等到整个目录树完成后才显示结果 | Interactive 模式逐步交付已完成子目录 | 剩余扫描仍在进行时即可查看结果；它不是持续监控 |

这是实现模式的比较，不表示其他所有工具都不支持批量读取或并行处理。性能声明仅限于下方已测工作负载。[开发者指南](docs/developer-guide.md)与[架构文档](docs/zh-CN/architecture.md)提供代码入口和取舍说明。

## CLI

```bash
hyperdu /path/to/data --top 20
hyperdu /path/to/data --json usage.json
hyperdu /path/to/data --csv usage.csv
hyperdu --compat gnu -k /var/log
hyperdu --compat gnu -b --time /usr/share
hyperdu --help
```

默认的上位列表按**目录的物理大小**排序，即使指定 `--apparent-size` 也不改变 `--top` 的排序依据。时间参数依赖默认启用的 `time-format` feature。排除、显示深度、扫描限制和链接跟随是不同设置，详见 [CLI 参考](docs/cli-reference.md)。

**不是 GNU / POSIX `du` 的完全替代品。** `--compat posix-strict` 选择512字节等默认值，但尚未实现 POSIX 必需的 `-a`、`-s`、`-H`、`-L`。请勿无条件为 `du` 设置别名。[兼容性审计](docs/posix-compatibility.md)

## GUI

```bash
cargo install hyperdu-gui --locked --version 0.5.0-beta.5
hyperdu-gui
```

Windows / Linux 桌面应用基于 `egui` / `eframe`。Interactive 模式逐步交付子目录结果，Batch 模式接收整体结果。支持目录树、面包屑、可排序列表、筛选、进度、错误与取消状态，以及 JSON / CSV 导出。界面文字为日语。

估算模式不保证精确的分配大小。取消或读取错误不会被报告为成功完成。[GUI 操作与限制](hyperdu-gui/README.zh-CN.md)

## AI 助手

```bash
hyperdu mcp
# MCP 客户端注册示例：
claude mcp add --transport stdio hyperdu -- hyperdu mcp
codex mcp add hyperdu -- hyperdu mcp
```

| 工具 | 用途 |
|---|---|
| `list_volumes` | 查看卷容量和剩余空间 |
| `scan_path` | 查看指定路径的使用量 |
| `find_reclaimable` | 寻找可能重新生成、值得人工检查的整理候选项 |

**三个工具均为只读，不删除文件。** 候选项不代表删除一定安全，仍需人工判断。MCP 服务、使用 CLI 的 Agent Skill 和分发它们的 Plugin 是独立入口；Skill 不依赖 MCP。[配置说明](plugin/README.zh-CN.md)

## 实测性能及其边界

以下是已发布的测量记录，**不是对当前 HEAD 或所有环境重新测量的结果**。测量源码为 `2645689515ab2e608a78b7492637e55179e4739a`，应与当前发行包版本区分。

### Linux：每种结构100万文件

GitHub-hosted Ubuntu 24.04 / ext4，AMD EPYC 9V74（4 vCPU、15.6 GiB RAM），每种结构包含1,000,000个256 B普通文件。热缓存、分配字节，每个工具交替运行8次的中位数；成功的目录行与独立 oracle 一致。

| 结构 | HyperDU | GNU `du` | du / HyperDU |
|---|---:|---:|---:|
| flat | 2327.49 ms | 2763.52 ms | 1.187倍 |
| wide | 745.96 ms | 2428.80 ms | 3.256倍 |
| deep | 764.67 ms | 2434.28 ms | 3.183倍 |

“最高3.25倍”是这组 Linux 测量的标题，不适用于冷缓存、其他文件系统、HDD 或网络存储。

### Windows：100万文件单独测量与1万文件诊断

Windows 11 / NTFS / NVMe，Ryzen 9 3900X（12核心 / 24线程、128 GiB RAM），热缓存、逻辑字节。100万文件 flat 的 HyperDU 单独运行8次，中位数为589.91 ms。GNU `du` 8.32（Git for Windows / MSYS）预热时达到600秒超时，**没有可接受的100万文件比较倍率**。wide / deep 的100万文件工作负载未运行。

| 1万文件诊断，每个工具2次的中位数 | HyperDU | GNU `du` (MSYS) | du / HyperDU |
|---|---:|---:|---:|
| flat | 32.98 ms | 704.17 ms | 21.35倍 |
| wide | 27.46 ms | 712.33 ms | 25.94倍 |
| deep | 32.32 ms | 836.73 ms | 25.89倍 |

不能把诊断倍率推广至100万文件或一般 Windows 环境；与 Linux 的统计方式、环境和运行次数也不同。

### Windows：与 dua-cli 和 tokei 的比较

Windows 11 / NTFS / NVMe，Ryzen 9 3900X，热缓存，逻辑字节。HyperDU（开发版 d4ebdb8）与 dua-cli 2.45.0 的总数在全部数据集上都与独立遍历一致。每个工具12次运行的中位数。

| 数据集 | 文件数 | HyperDU | dua-cli | dua / HyperDU |
|---|---:|---:|---:|---:|
| 宽目录树（合成） | 99,856 | 67.57 ms | 187.37 ms | 2.77× |
| 深层目录树（合成） | 100,000 | 179.68 ms | 194.62 ms | 1.08× |
| 平铺目录（合成） | 100,000 | 98.80 ms | 997.51 ms | 10.10× |
| Cargo registry（真实文件） | 107,953 | 478.55 ms | 554.61 ms | 1.16× |
| node_modules（真实文件） | 27,940 | 72.26 ms | 156.60 ms | 2.17× |

HyperDU 在5项中均更快，但差距取决于目录结构（除平铺外为 1.08–2.77 倍）。深层目录没有可并行的宽度，基本相当。平铺目录的 10.10x 包含 dua-cli 输出 100,001 行的时间，不具代表性。

tokei 读取内容并统计行数，是不同的工作，因此**不给出速度比**。仅供参考：Cargo registry（107,953 个文件）HyperDU 用时 478.55 ms，tokei 用时 9,407.19 ms；node_modules（27,940 个文件）分别为 72.26 ms 和 782.31 ms。测量期间其他任务占用了约60–90%的 CPU，绝对值偏大。

[测量方法与局限](docs/zh-CN/benchmarks.md) · [记录 JSON](docs/benchmarks/2026-09-30-windows-vs-dua-tokei.json)

[运行记录](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086) · [原始数据](https://hyperdu.automation.jp/benchmarks.json) · [方法与限制](docs/zh-CN/benchmarks.md) · [性能设计](docs/zh-CN/performance.md)

## 分发与平台状态

[已发布版本](https://github.com/automationjp/HyperDiskUsage/releases/tag/v0.5.0-beta.5)提供 Windows x86_64、Linux x86_64（glibc / musl）和 aarch64 的 CLI / GUI 二进制文件。可用格式以 Assets 为准。仓库中存在打包 manifest 并不等于已经在应用商店注册。

```powershell
scoop bucket add hyperdu https://github.com/automationjp/HyperDiskUsage
scoop install hyperdu
```

winget 请通过 `winget search --id automationjp.HyperDU --exact` 检查实际目录，不以本仓库的 manifest 作为可安装的证明。

<a id="platform-status"></a>

| OS | 当前验证与分发范围 |
|---|---|
| Windows | 原生 CI、NTFS fixture、CLI / GUI 分发 |
| Linux | CI、CLI / GUI 分发、上方 ext4 基准测试 |
| macOS | 存在 `getattrlistbulk` 实现，但不在当前 CI / Release 范围内。CLI 验证未完成，不保证 GUI 可用 |

Windows `--mft` 是实验性的手动选择路径，需要 NTFS 卷根目录、管理员权限和受支持的参数。必要读取或解析不完整时回退到普通枚举。**MFT 解析成功不代表统计结果与普通枚举完全一致。**[MFT 使用边界](docs/zh-CN/architecture.md)

## 实验性 Linux 快照

```bash
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

这不是 watcher。`show` 读取保存值，freshness 始终标为 `stale`。[快照契约](docs/zh-CN/index-snapshots.md)

## 开发

```bash
git clone https://github.com/automationjp/HyperDiskUsage.git
cd HyperDiskUsage
cargo check --workspace --locked
cargo test --workspace --locked
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run -p hyperdu -- --help
```

[开发者指南](docs/developer-guide.md)介绍 crate 职责、Rust 集成、CI / 发布权限边界和按 feature 验证的方法。Tracy 和 Puffin 不能同时启用，不要把 `--all-features` 当作统一验证命令。

[架构](docs/zh-CN/architecture.md) · [环境设置](docs/zh-CN/setup.md) · [历史文档](docs/old/README.md) · [文档目录](docs/zh-CN/README.md)

## License / Acknowledgements

[MIT License](LICENSE)。设计参考：[ripgrep](https://github.com/BurntSushi/ripgrep)、[fd](https://github.com/sharkdp/fd)、[dust](https://github.com/bootandy/dust)。
