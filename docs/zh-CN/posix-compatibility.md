# POSIX du 兼容性审查

[日本語](../posix-compatibility.md) | [English](../en/posix-compatibility.md) | [简体中文](../zh-CN/posix-compatibility.md)

结论：当前 CLI 尚未完全兼容 POSIX du。

默认模式显示 HyperDU 专用列表和摘要。`--compat posix-strict` 的名称不代表完整符合规范。

| 项目 | 确认结果 |
|---|---|
| 必需的 `-a`、`-s`、`-H`、`-L` | 尚未实现。Linux release 运行 `--compat posix-strict -s <path>` 返回代码2，提示 unexpected argument。 |
| `-k`、`-x` | CLI 接受这些选项，但这不是所有边界条件的认证。 |
| 默认单位 | posix-strict 代码选择512字节单位；默认 HyperDU 模式使用专用输出。 |
| 不存在的路径 | Linux release 向 stderr 输出诊断并返回代码1。 |
| 完整语义 | 文件参数、符号链接、目录自身分配空间和选项优先级等完整符合性尚未实现。 |
| 分配空间差异 | 一个4096字节文件、硬链接、两个已分配空间的符号链接和两个目录的fixture中，du根合计为40块，HyperDU为8块。未包含目录和未追踪链接自身的分配空间。 |
| 符号链接行为 | 符号链接参数报errno20。--follow-links静默跳过悬空链接并返回0；du -L诊断该链接并返回1。 |
| 输出分隔符 | posix-strict使用TAB，未使用规范中的空格。 |

README 和网站中无法使用的 -sh / -ak 示例已修正，并删除了 du 别名建议。本次审查未实现完整兼容模式。

[POSIX.1-2017 / Open Group du specification](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/du.html)

Audit: 2026-09-09 / release source `0a089c90c0e2995ef702ff053820d129265c3659` / WSL2 Ubuntu 26.04.
