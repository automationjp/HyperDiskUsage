# Directory index v2（实验功能）

[日本語](../index-snapshots.md) · [English](../en/index-snapshots.md) · **简体中文**

Windows、Linux、macOS 可保存文件 identity 和大小、复用目录汇总，支持的本地 filesystem 还可在前台监控变更。

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index watch /srv/data --database "$HOME/.cache/hyperdu/data.idx" --once
```

`refresh` 扫描并替换 v2 snapshot；`show` 读取保存值，仅检查 ROOT identity，不遍历文件树。`watch` 处理 native 通知，Ctrl-C 退出。`--once` 追上 journal、保存并退出，上限 30 秒。v1 文件需显式 refresh 重建，v1 Rust API 保留。

## Native source

| 平台 | Source | 重启 |
| --- | --- | --- |
| Windows | NTFS/ReFS 已有 USN journal，需要 volume 读取权限；不会创建或修改 journal | 验证 volume、journal ID 和保留的 USN 范围 |
| macOS | 本地 APFS/HFS 的 per-device FSEvents | 验证 device UUID 和 event ID 后重放历史 |
| Linux | filesystem fanotify，权限或 file handle 不支持时回退 recursive inotify | queue 不持久化，必须重新扫描 |

Linux 使用 `HYPERDU_INDEX_LINUX_BACKEND=auto|fanotify|inotify` 选择方式；显式指定 fanotify 而不可用时返回错误。watch 上限、overflow、journal 缺失或重建会触发重新扫描。ROOT 被替换时需要显式 refresh。SMB/NFS 等不支持的 native 监控返回原因，仍可使用 refresh/show。

## 汇总和 freshness

统计普通文件 logical bytes、allocation bytes 和去重后的文件数。不包含目录自身 storage、symlink/reparse point、特殊文件或命名 alternate stream。hardlink 按 volume 和完整 file ID 去重，归属 parent ID/name 排序首个链接。保留 Windows 128-bit ID 和 native 文件名编码。

`unknown` 表示尚未观测；`stale` 表示保存值、更新中或未验证；`observed` 表示已处理 catch-up barrier 之前配送的事件，不代表 atomic snapshot 或完整最新状态。mmap、监控范围外 hardlink 写入、远程变化可能没有通知。因此定期全量核对是必需的：`--reconcile-seconds` 默认 900，范围 1–86400 秒。show 始终 stale。

## 成本和保存

普通文件变更只观测该文件并更新祖先汇总。已知目录改名保留子孙，不枚举整个 subtree。新增 subtree 和定期核对按对应范围枚举。首次扫描、snapshot 读取和保存为 O(文件、链接、目录数)，加载后的 root 汇总查询为 O(1)。

内存即时更新；完整 checkpoint 按 `--checkpoint-seconds` 保存，默认 60，范围 1–3600 秒。首次追上 journal 和正常退出时也保存。强制终止后从最后 checkpoint 恢复或重建。`--poll-ms` 默认 100，范围 10–60000 ms。stdout 为 JSON Lines，包含大小、文件数、freshness、journal cursor、checkpoint 状态、通知数、观测次数、目录枚举数和 rebuilt。工作量仅指该 poll，不包括启动 baseline。

提前创建父目录，database 放在 ROOT 外，因此 CLI 不支持以 `/` 为 ROOT。一个 index 只覆盖一个 filesystem，子 mount 单独建立。database 和旁边的 `.lock` 必须为普通文件，拒绝 symlink。OS lock 拒绝同时写入，在进程结束时释放，保留 lock 文件。

校验 root identity、文件名、图结构、长度上限和 checksum。checksum 仅检测意外损坏，不是认证。snapshot 和 cursor 一起 atomic replacement，不保存未完成更新。

普通 scan flags 不改变 index 的固定汇总规则。扫描名为 index 的目录可用 `hyperdu ./index` 或 `hyperdu -- index`。

[Architecture](architecture.md) · [Performance](performance.md) · [Historical design](../old/README.md)