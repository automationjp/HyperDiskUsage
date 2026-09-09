# 架构

[日本語](../architecture.md) | [English](../en/architecture.md) | [简体中文](../zh-CN/architecture.md)

HyperDU 采用**一个高速 scanner core，并在其上提供 CLI / GUI / MCP**的结构。

性能相关处理集中在 `hyperdu-core` 中，而不是为每个 interface 重复实现，因此 CLI、GUI 和 AI agent 共享相同的扫描语义与 platform fast path。

## 概览

```text
       hyperdu CLI        hyperdu mcp        hyperdu-gui
            \                  |                  /
             +-----------------+-----------------+
                               |
                         hyperdu-core
                    scan / model / domain
                         /     |     \
                      Linux Windows macOS
```

## 组件

| Component | Responsibility | Performance role |
|---|---|---|
| `hyperdu-core` | tree scan、汇总和 platform abstraction | hot path 的中心 |
| `hyperdu` | command parsing、显示和 export | 调用 scanner 的薄 interface |
| `hyperdu-gui` | desktop UI、drill-down | 使用相同的 core scanner |
| `hyperdu mcp`（CLI 内的 subcommand） | structured MCP tools | 面向 agent 公开相同的 core scanner |
| `plugin/` | Agent Skill / Plugin | 提供 CLI / MCP 的使用流程 |
| `scripts/bench/` | benchmark harness | 保证 performance claim 的可复现性 |

## 扫描数据流

普通扫描大致按照以下流程进行：

1. CLI / GUI / MCP 构建 scan option
2. `hyperdu-core` 判断目标 filesystem 和 platform
3. 选择 platform-specific enumeration path
4. 将 directory work 放入 worker queue
5. worker 处理 subtree，并在需要时进行 work stealing
6. 汇总 file / directory 的 logical・physical usage
7. 处理 hardlink 等重复项
8. 将 subtree totals roll up 到父级
9. interface 将结果转换为 human-readable / JSON / CSV / structured MCP result

## Interactive mode

`hyperdu-core::scan_directory_mode` 提供 `ScanMode::Interactive` 和 `ScanMode::Batch`。在 GUI interactive mode 中，HyperDU 首先枚举 root 直下的内容，然后让所有 worker 一次处理一个子文件夹并报告结果。即使剩余扫描仍在继续，已经完成的文件夹也可以查看。

选项只根据原始 root 准备一次；hardlink / link-cycle detection state、filesystem boundaries、progress、error counts 和 cancellation 在所有阶段共享。由于子文件夹深度从 1 开始，depth limit 也以原始 root 为基准。GUI 不会另外实现文件枚举或重复判定。

root 直下的文件会以 core 应用 filter 后得到的汇总值进行报告。不会创建可能与 directory 结果混淆的 virtual path。直接 MFT scan 成功时，事件会明确表明结果是一次性批量返回的。被取消的子文件夹不会被报告为已完成。

两种模式中的 hardlink 总量一致，但重复名称的大小归属到哪个文件夹取决于扫描顺序。因此，一次性扫描与按子文件夹划分的明细不保证在每个细节上都一致。

## Platform boundary

### Linux

```text
Directory
   |
   +--> getdents64  -- names / inode / type --> worker
                                             |
                                             +--> statx --> size metadata
```

在 Linux 中，directory enumeration 和 metadata 获取是分开的。既然精确大小需要 metadata syscall，就必须重视枚举效率、并行化和 filesystem strategy。

### Windows

```text
Directory
   |
   +--> NtQueryDirectoryFile
          |
          +--> name
          +--> file size
          +--> allocation size
          +--> file ID
```

在 Windows 中，一次 batch enumeration 就能同时取得 size 和 file ID，因此减少额外的 handle open 是加速的核心。

### Optional MFT path

```text
--mft + supported NTFS volume root
              |
              v
        read / parse $MFT
              |
       complete + valid ?
          /          \
        yes           no
         |             |
         v             v
      result      directory enumeration
```

MFT backend 是 optional fast path。对于无法解析的 layout，不会强行进行部分汇总，而是 fallback 到普通路径。

## 并发模型

简单的固定 range 分割会因为 directory tree 的大小不均衡而增加 worker idle。

HyperDU 为每个 worker 提供 LIFO deque，并允许其他 worker steal work。

目标包括：

- 在深入处理当前 subtree 的同时保持 locality
- 让 idle worker 接手大型未处理 subtree
- 避免把 queue 瞬间为空误认为扫描完成

目标不是单纯增加 thread 数量，而是跟随 **tree shape 的不均衡**。

## 正确性优先于速度

HyperDU 的 fast path 以不改变结果语义为前提。

以下内容不会为了性能优化而省略：

- hardlink accounting
- logical / physical size 的区别
- 不支持 fast path 时的 fallback
- 拒绝不完整的 MFT parse
- scan error / cancellation 的处理

即使要发布速度值，也要先确认 scan parity。步骤请参见 [Benchmark plan](benchmarks.md)。

## 持久化的 Linux snapshots

`index refresh` / `index show` 是复用明确保存的 directory aggregate 的独立功能，并不是普通 scan 的替代品。

它不是 automatic watcher，`show` 的结果会明确标记为 `stale`。详情请参见 [Linux directory snapshots](index-snapshots.md)。

## 历史设计记录

过去按 Issue 划分的设计、MFT 验证记录和旧 persistent-index 方案已移至[old/README.md](../old/README.md)。

要理解当前架构，请从本文和 [Performance design](performance.md) 开始。
