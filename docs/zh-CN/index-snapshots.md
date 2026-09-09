# Linux directory snapshots（实验性）

[日本語](../index-snapshots.md) | [English](../en/index-snapshots.md) | [简体中文](../zh-CN/index-snapshots.md)

HyperDU 的 `index` 子命令是面向 Linux 的实验性功能，用于**保存一次扫描得到的 directory aggregate，并在下次读取时不重新扫描文件树**。

> 这不是 watcher。它不会自动更新，也不会作为常驻监视器运行。保存的值始终按 `stale` 处理。

## 快速开始

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

- `refresh`：使用普通 scanner 扫描 tree，并保存每个 directory 的 snapshot
- `show`：读取已保存的 snapshot，不重新扫描目标 tree 即返回结果

## 适用场景

适合：

- 需要反复查看文件数量非常多的 tree 的概算
- 接受“上次明确更新时的值”
- 不想安装自动监视 daemon

不适合：

- 始终需要最新值
- 需要 filesystem 的 atomic snapshot
- 需要实时追踪文件变化

## `stale` 的含义

`show` 只读取保存的数据。`refresh` 之后创建、修改或删除的文件不会反映出来。

此外，`refresh` 本身也不是 filesystem 的 atomic snapshot。扫描过程中 tree 可能发生变化。

因此，输出会明确包含：

```text
freshness: "stale"
monitoring: false
```

`stale` 不是错误，而是指**不保证与当前 filesystem 完全一致的保存值**。

## Database rules

为安全地保存和复用，database 有以下限制：

- 父 directory 必须预先存在
- database 必须是普通文件
- 拒绝 symlink
- 将 database 放在 scan root 外部
- 每个 tree 使用单独的 database

因此，`/` 本身不能用作 snapshot root。

## Root identity

保存和读取时，HyperDU 会确认 mount root 的 device / inode identity 一致。

如果因为 remount、restore、root replacement 等原因 identity 发生变化，请重新执行 `refresh`。

inode 可能被复用，因此这是 consistency check，而不是 freshness 保证。

## 失败行为

如果 `refresh` 未能完全成功，HyperDU 会优先避免用不完整的值替换已有 snapshot。

例如：

- cancellation
- scan error
- 无法安全表示的 bind-mount alias
- 无法确定 root identity

## Cost model

`show` 的读取成本为 **O(indexed directories)**，不会重新扫描所有文件。

读取后的 root lookup 为 O(1)。

## CLI note

`index` 是子命令名称。如果需要对名为 `index` 的普通 directory 执行扫描，请明确指定。

```sh
hyperdu ./index
# 或
hyperdu -- index
```

普通扫描的 option 不会原样应用于具有固定语义的 `index refresh` / `index show`。

## 尚未实现的功能

当前不包含：

- inotify watcher
- background daemon
- automatic refresh
- overflow recovery
- watch budget policy

这些是未来另行考虑的功能。过去的 persistent-index / watcher 设计保存在[旧文档](../old/README.md)中。

## 相关文档

- [Architecture](architecture.md)
- [Performance design](performance.md)
- [历史设计记录](../old/README.md)
