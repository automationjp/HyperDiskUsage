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

### GUI 后台处理与显示

专用的 `hyperdu-gui-scan` 线程调用核心，并准备目录索引、缓存的汇总值和四种排序。接收扫描结果和切换显示顺序时，不把文件枚举或全部结果的重新排序放在UI线程上。

```text
core ScanEvent -> background index / sorting
                       |
                node chunks (256)
                       |
                 bounded queue (2)
                       |
           UI ingestion budget -> tree / visible table rows

core progress counter -----------------> status / elapsed time
```

节点更新按最多256个节点分块，队列容量为2条消息。UI 每帧最多处理1024个更新工作单位，并以3ms为时间预算，将剩余工作留到后续帧。时间检查发生在更新之间，因此是协作式限制；单次内存分配或旧模型销毁不保证在3ms内完成。后台线程也保留完整索引，总内存不会仅由队列容量决定。

父目录的排序索引在其引用的节点发送后才发布，避免引用不存在的ID；但这也意味着第一块数据到达时，表格不一定立即增加行。表格只绘制可见行，树视图有深度和行数预算。更深的目录仍可通过表格和面包屑导航访问。

开始扫描时立即显示工作状态。共享计数器独立于结果队列更新进度和经过时间，事件到达时请求重绘，扫描中也会定期刷新。取消通过共享标志传递给核心和索引准备过程，不会把部分结果标记为完成。同步I/O或已经开始的单次排序不能保证立即中断。接收端被释放时，等待队列的发送端会被解除阻塞；更换扫描句柄也会隔离旧扫描的结果。

终止事件在之前的更新处理完毕后才处理。读取错误、取消、没有终止事件的发送端断开，均与成功完成区分。用户触发的JSON/CSV导出会复制所有行并按路径排序。这些操作以及模型替换等仍在UI线程同步执行。

实现位置：[GUI传输与索引](../../hyperdu-gui/src/scan.rs)、[UI接收与绘制](../../hyperdu-gui/src/app.rs)、[核心模式与事件](../../hyperdu-core/src/lib.rs)。

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
shared eligibility guard
 Windows MSVC / volume root / administrator / supported options
                         |
                   open NTFS volume
                         |
       extent-limited raw window -> copied record -> fixups / parse
                         |
              parent-record-ID aggregation
                         |
               directory paths / rollup

ineligible or incomplete required read/parse -> directory enumeration
```

MFT是Windows MSVC上的实验性可选路径。CLI、GUI和核心直接API共用资格检查。除了卷根目录与管理员权限，原始或已编译的排除条件、深度限制、最小文件大小、链接跟随、硬链接单独计数、近似大小、外部传入的去重缓存也会使MFT路径被拒绝。普通扫描API此时回退到目录枚举，直接API返回 `None`，使验证程序能够区分实际MFT扫描与回退。

reader通过默认上限1MiB的原始字节窗口批量读取连续MFT记录。预读限制在物理extent内；跨extent边界的记录由必需片段组成。fixup只应用于记录副本，因此重新读取和扩展记录查询不会把已修改的缓存字节当作原始数据。预读失败时清空缓存，并仅重试必需片段；不会接受必需读取或解析不完整的结果。

集计先按父记录ID累加大小，再生成目录路径并在核心中向父级汇总。硬链接身份、缺失父级和循环父级的处理保留已有集计规则。读取窗口有上限，但所有条目和汇总结果仍保存在内存中，并非对整个MFT使用固定内存的流式扫描。

reader在开始前、最多每256条记录、正常结束时检查进度和取消。记录数包含未使用的slot，与最终文件数或完成百分比不同。同步扩展记录读取不能保证立即中断。Interactive模式请求的MFT扫描成功后，通过 `BatchFallback(Mft)` 和 `BatchCompleted` 明确表示结果批量交付；这与MFT失败后回退到普通枚举是不同的情况。

满足资格条件并成功解析必需记录，并不能证明与目录枚举完全一致。仍存在已知集计差异，性能验证必须分别记录实际使用的路径和集计差异。

实现位置：[共用资格检查](../../hyperdu-core/src/platform/windows_impl/mod.rs)、[reader](../../hyperdu-core/src/platform/windows_impl/mft_reader.rs)、[原始字节窗口](../../hyperdu-core/src/platform/windows_impl/mft_reader/window.rs)、[ID集计](../../hyperdu-core/src/platform/windows_impl/mft_aggregate.rs)。

## CLI / MCP 进度通知

普通CLI把结果写入stdout，把进度和诊断写入stderr。stderr连接终端时自动显示工作状态；重定向时可用 `--progress` 显式开启。开始后立即显示状态，结束时唤醒等待中的状态线程，无需等待下一个刷新周期。

`hyperdu mcp` 是基于rmcp的stdio服务器。`scan_path` 收到MCP的 `_meta.progressToken` 时，通过标准 `notifications/progress` 发送初始值0以及后续递增的文件数。没有token时不发送进度通知。每个请求独立保存token和取消状态，不混淆并发调用。

核心同步扫描通过 `spawn_blocking` 执行。回调只更新watch channel中的最新计数，不让扫描线程等待协议传输。异步发送端合并更新，将中途通知限制为大约250ms一次。如果最终计数尚未发送，则在结果前发送该计数。计数不是百分比；成功完成应以最终工具结果为准。

请求取消、请求销毁或通知传输失败会传递到核心的取消标志。这是协作式取消，并不保证立即中断操作系统的同步I/O。这个进度adapter用于 `scan_path`，并不代表所有MCP工具都提供进度通知。

实现位置：[CLI](../../hyperdu-cli/src/main.rs)、[MCP工具](../../hyperdu-cli/src/mcp.rs)、[MCP进度adapter](../../hyperdu-cli/src/mcp/progress.rs)。参数和输出条件见 [CLI参考（日语）](../cli-reference.md)。

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
