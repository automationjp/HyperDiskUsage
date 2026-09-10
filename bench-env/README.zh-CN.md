# bench-env — AWS 测量环境（项目无关）

[日本語](README.md) | [English](README.en.md) | [简体中文](README.zh-CN.md)

这是曾用于 HyperDiskUsage 性能测量的 EC2 环境，现已拆分出来以便其他项目使用。**此目录完全不依赖 HyperDiskUsage。** 它不包含外部引用，因此可以直接通过 `git subtree split` 拆分到其他仓库。

## 核心限制：不共享实例

测量时不能同时共享一台实例。原因有 4 个，其中 1 个已经造成了实际影响。

| 因素 | 会发生什么 |
|---|---|
| 突发型 CPU（T 系列） | 相邻 workload 会消耗 CPU credit，使测量值变差 |
| **突发型 EBS 带宽（T 系列）** | 同一次 cold run 从 0.495s 变为 0.81–0.89s，已经观察到实际影响（记录在 `scripts/bench/vs_du.sh`） |
| 页面缓存 | cold 测量所需的 `drop_caches` **作用于整个系统**，会破坏其他租户的 warm 测量 |
| vCPU 较少 | 存在并行负载时，线程扩展性测量会失去意义 |

因此，本环境的方针是**“共享环境定义，而不是共享实例”**。一次测量使用一台实例。使用完毕后销毁。专用实例本身提供排他性，因此不需要锁机制。

## 为什么从 T 系列改用其他实例类型

| | 旧 | 新（默认） |
|---|---|---|
| 类型 | `t3.large` | `m5d.large` |
| vCPU / RAM | 2 / 8 GiB | 2 / 8 GiB（**保持不变**） |
| CPU 性能 | 突发型（基于 credit） | 固定 |
| EBS 带宽 | 突发型 | 固定 |
| 存储 | EBS gp3 | **本地 NVMe**（instance store） |

保持 vCPU 和 RAM 不变，是为了让**过去的测量值在形态上仍然可比较**。改变的只有非确定性来源。

选择本地 NVMe 有两个原因：

1. 从测量中排除基于网络的 EBS 因素
2. **可以自由执行 `mkfs`**，因此能够并列放置 xfs / ext4 / btrfs（验证 Linux 专用功能时需要）

如果要测量线程扩展性，请指定 `--instance-type m5d.2xlarge`（8 vCPU）。
**它不能与 2 vCPU 时代的数值直接比较**，因此比较对象也必须使用相同类型重新测量。

### 不使用 Spot 实例

如果测量过程中发生中断，只能丢弃该 run。如果最终只保留未中断的 run，结果会产生生存者偏差。测量时使用 on-demand 实例。

## 使用方法

```bash
# 1. 启动（启动一台专用实例）
./aws/launch.sh --project myproject --ttl 120

# 2. 使用输出的 ssh 命令连接。/mnt/xfs /mnt/ext4 /mnt/btrfs 已准备好
#    source protocol/lib.sh 以使用测量协议

# 3. 销毁（超过 TTL 后也会自动 terminate）
./aws/terminate.sh --project myproject
```

`--ttl` 是以分钟为单位的硬限制。即使忘记指定，默认 180 分钟后也一定会 terminate。
**这里的判断是：测量过程中实例掉线虽然麻烦，但忘记销毁导致持续计费更加麻烦。**

## 测量协议（protocol/lib.sh）

HyperDiskUsage **曾经违反过一次**的规则，如今不是写在注释里，而是通过**代码强制执行**。为了让其他项目也能直接使用，协议通过参数接收比较对象的 command，采用通用形式。

| 规则 | 依据 |
|---|---|
| warm 取最小值，**cold 取中位数** | cold 取最小值会把突发状态当成稳定状态报告 |
| cold 先 burn-in，再交替执行 | 让两个工具看到相同的稳定状态 |
| 必须记录环境 metadata | 如果之后无法还原某个数值对应的条件，测量就没有意义 |

`protocol/verify.sh` 会在测量前检查环境是否处于可测量状态；如果不满足就**中断**（警告很容易被跳过）。

## 成本

可以通过 `--project` tag 跟踪成本分配。在 Cost Explorer 中使用 tag filter 查看各项目的明细。准确的单价请参见 AWS pricing page（单价会随实例类型和 region 变化，因此这里不列出）。

由于 TTL 会自动 terminate，忘记删除实例不会造成无限增长的费用。

## 尚未验证的项目

此目录中的脚本**尚未在 AWS 凭据有效的 session 中完成执行验证**（创建时的 session 中，`sts get-caller-identity` 以 `InvalidClientTokenId` 失败）。首次执行时需要确认以下项目：

- `launch.sh` 是否会找到预期的 Amazon Linux 2023 image
- instance store 的设备名称（`/dev/nvme1n1`）对于 `m5d.large` 是否正确
- cloud-init 完成检测是否正常工作
- `m5d.large` 的 instance store 容量是否足以放置目标 tree
