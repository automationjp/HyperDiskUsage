# hyperdu-core

[日本語](README.md) · [English](README.en.md) · **简体中文**

[HyperDU](https://github.com/automationjp/HyperDiskUsage) 的扫描引擎。
并行遍历目录树，返回各目录的逻辑大小、磁盘分配空间和文件数。
一般用户可使用 [`hyperdu`](https://crates.io/crates/hyperdu) 命令；
此 crate 用于将扫描功能嵌入自己的应用。

## 批量扫描

```rust
use hyperdu_core::{scan_directory, Options};

let stats = scan_directory("/path", &Options::default())?;
let root = stats.get(std::path::Path::new("/path")).unwrap();
println!("{} bytes across {} files", root.physical, root.files);
# Ok::<(), anyhow::Error>(())
```

目录汇总包含其后代。`scan_directory_mode` 的 `ScanMode::Interactive` 会先通知根目录
的直接子目录及直接文件的汇总，然后逐个通知已完成的子树结果。所有阶段共享选项、硬链接去重、
链接循环检测、文件系统边界、进度、错误和取消状态。MFT 扫描成功时会明确通知批量结果回退。
事件契约见[架构文档](../docs/zh-CN/architecture.md)。

## 实现

- Linux：`getdents64` + `statx`。
- Windows：使用 `NtQueryDirectoryFile` 批量获取分配大小和文件 ID；满足权限及卷根目录
  条件时，可直接读取 NTFS `$MFT`。
- macOS：`getattrlistbulk`；尚未完成实机验证。
- 每个工作线程使用 LIFO 队列，并通过工作窃取分配任务。
- 按文件系统标识去除重复硬链接，与默认 `du` 的计数方式一致。

`volume::list()` 提供文件系统容量，`index` 提供可持久化的目录汇总。
`reclaimable` 识别可重新生成的候选产物；采样修改时间不能证明目录未被使用或可以安全删除。
不执行删除操作。

## 许可证

MIT
