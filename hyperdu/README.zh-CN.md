# hyperdu

[日本語](README.md) · [English](README.en.md) · **简体中文**

结合原生元数据读取和并行遍历的磁盘使用量分析器。**一个命令同时提供 CLI 和只读 MCP 服务**，与 GUI 和 Rust 应用共用 `hyperdu-core`。

## 安装

本 README 对应 `0.5.0-beta.7`。[网站](https://hyperdu.automation.jp/) · [GitHub Releases](https://github.com/automationjp/HyperDiskUsage/releases)

```bash
cargo install hyperdu --locked --version 0.5.0-beta.7
```

crate 和命令名称都是 `hyperdu`，不是已废弃的 `hyperdu-cli`。仅执行 `hyperdu mcp` 时才启动 MCP。源码构建需要 Rust 1.88 或更高版本及对应平台构建工具；运行预构建二进制文件不需要 Rust。[环境设置](../docs/zh-CN/setup.md)

要安装开发 checkout，请在仓库根目录运行 `cargo install --locked --path hyperdu`。这与安装已发布版本是两种不同选择。

## 用法

```bash
hyperdu /path --top 20
hyperdu /path --json out.json
hyperdu /path --csv out.csv
hyperdu --compat gnu -k /var/log
hyperdu mcp
hyperdu -- mcp
```

依次为目录排行、JSON / CSV 导出、GNU 兼容输出、启动 MCP，以及扫描名称为 `mcp` 的目录。默认 `--top` 按物理大小排序。参数默认值、平台范围和进度 / 输出位置见 [CLI 参考（日语）](../docs/cli-reference.md)。时间参数需要默认启用的 `time-format` feature。


### 运行中与结束后的显示

扫描期间只有终端最下方两行每 20ms 刷新，显示完成比例、已处理文件数、速度、已用时间，以及正在扫描的文件。

```text
progress:  42% | 612880 files | 201865 f/s | 3.0s
scan: photo_0412.jpg (2.31 MiB)
```

结束后这两行被清除，留下按物理大小排序的结果（路径为示例）：

```text
Top 3 under D:\data (physical desc):
  1. D:\data | phys=204.39 GiB | log=201.76 GiB | files=1641117
  2. D:\data\videos | phys=72.24 GiB | log=71.81 GiB | files=247520
  3. D:\data\photos | phys=32.95 GiB | log=32.44 GiB | files=360626
```

## 原理与边界

Windows `NtQueryDirectoryFile` 批量获取名称、分配大小、文件 ID 等信息，减少额外的文件打开。Linux 使用 `getdents64` 和 `statx`，并行处理必要的元数据读取。各工作线程的 LIFO 队列与 work stealing 向不均衡目录树分配工作。macOS 的 `getattrlistbulk` 实现目前不在 CI 和 Release 范围内。

`--mft` 是有条件的实验性 Windows NTFS 路径。必要读取或解析不完整时回退到普通枚举，但解析成功并不保证统计完全一致。HyperDU 也没有实现 GNU / POSIX `du` 的全部参数。[比较与限制](../README.zh-CN.md)

MCP 的 `list_volumes`、`scan_path`、`find_reclaimable` 均为只读，不删除文件。Skill 也可以只通过 CLI 使用。[AI 助手配置](../plugin/README.zh-CN.md)

## 开发者资料

[开发者指南（日语）](../docs/developer-guide.md) · [性能设计](../docs/zh-CN/performance.md) · [测量与限制](../docs/zh-CN/benchmarks.md) · [GUI](../hyperdu-gui/README.zh-CN.md)

请区分基准测试的测量 commit、已发布版本和当前 HEAD。

## 许可证

MIT
