# HyperDU 与 du 比较

正在准备最新版AWS EC2测量。只发布与GNU du直接一致的目录字节总量：物理分配量、不追踪符号链接、硬链接去重、单一filesystem、输出全部目录行。旧WSL2测量需要外部统计补正，已撤回，不再作为速度比较依据。

## 验收条件

在AWS EC2 Linux x86_64上从最新源码snapshot构建release。记录源码／二进制hash、GNU du版本、取得时间、实例／CPU／RAM／EBS／filesystem／kernel。两工具采用相同不变数据、权限、统计与输出条件，不允许Python等外部字节补正。

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

初次检查和每次计时均要求所有目录行及字节值直接一致。错误、不一致或数据变化使测量无效。性能设置先在独立pilot中选择，再固定设置交替预热运行8次，记录包含启动与输出的中位数、全部样本及不利结果。不得用概算或逻辑大小替代物理分配量。

Linux的filesystem挂载根与目录使用同一枚举引擎，没有Linux MFT快速路径。可选择专用EBS挂载根及代表目录，排除正在变化的操作系统根。最终结果将完整列出性能参数。

## 当前状态

本地Linux回归测试已复现差异并验证修正后与GNU du直接一致；这仅是正确性测试，不是AWS性能测量。AWS目标与有效认证尚待确定，暂时没有最新版AWS速度结果。

[Reproducible benchmark runner](../../scripts/bench/du_same_conditions.py)

[AWS runbook and provenance records](../benchmarks/aws-protocol.md)
