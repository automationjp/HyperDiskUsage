# HyperDU 文档

[日本語](../README.md) · [English](../en/README.md) · **简体中文**

安装方法、扫描引擎设计和验证指南。

| 目标 | 文档 |
|---|---|
| CLI 默认值、输出模式及平台限制 | [CLI 参数参考（日语）](../cli-reference.md) |
| 安装、准备 Rust/系统依赖、构建及测试 | [环境配置](setup.md) |
| 了解优化原理与取舍 | [性能设计](performance.md) |
| 查看 Windows/Linux 结果、全部样本和条件 | [基准测试](benchmarks.md) |
| 了解 CLI、GUI、MCP、核心及交互模式 | [架构](architecture.md) |
| 使用已保存的 Linux 目录信息 | [快照](index-snapshots.md) |
| 连接 AI 助手 | [Plugin / Skill / MCP](../../plugin/README.zh-CN.md) |

## 准备环境

运行预编译包无需 Rust。构建 CLI（包含 MCP）或整个 workspace 需要 Rust 1.88+；
core/GUI 声明的最低版本为 1.75。Windows 源码构建使用 MSVC 和 Windows SDK。
Linux 需要原生构建工具，Linux GUI 还需要 X11/Wayland 开发库。
命令和验证范围见[环境配置](setup.md)。

## 如何阅读性能结果

原生枚举与并行处理见[性能设计](performance.md)。当前Linux结果来自GitHub Actions Ubuntu 24.04/ext4：每种结构100万个256 B普通文件，三种结构各工具交替warm运行8次，共48个raw sample，所有目录行的物理分配字节直接一致。GNU du / HyperDU中位数倍率为平铺1.187x、宽目录3.256x、深目录3.183x；保守的3.25x标题值仅适用于Linux。Windows 11补充结果和未执行的AWS runbook见[比较结果与方法](benchmarks.md)。旧WSL2倍率不作为当前速度依据。

## 现行文档与历史记录

`docs/` 为日语，`docs/en/` 为英语，`docs/zh-CN/` 为简体中文。
命令、版本和测量表格保持一致。`docs/old/` 按原文保留旧基准测试以及特定 Issue 的设计和验证记录，
不能作为当前行为或速度的依据。可查阅[历史资料目录](../old/README.md)。

[POSIX du compatibility audit](posix-compatibility.md)
