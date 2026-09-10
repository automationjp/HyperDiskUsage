# bench-env — AWS measurement environment (project-independent)

[日本語](README.md) | [English](README.en.md) | [简体中文](README.zh-CN.md)

This is the EC2 environment that was used for HyperDiskUsage performance measurements, extracted so that other projects can use it as well. **This directory has no dependency on HyperDiskUsage.** It has no external references so that it can be split directly into another repository with `git subtree split`.

## Core constraint: do not share an instance

One instance cannot be shared concurrently for measurement. There are four reasons, one of which has already caused a real impact.

| Factor | What happens |
|---|---|
| Burst CPU (T series) | A neighboring workload consumes CPU credits and degrades the measurement |
| **Burst EBS bandwidth (T series)** | The same cold run changed from 0.495s to 0.81–0.89s in an observed incident (recorded in `scripts/bench/vs_du.sh`) |
| Page cache | `drop_caches`, which is required for cold measurements, **affects the entire system** and destroys warm measurements belonging to other tenants |
| Few vCPUs | With concurrent load, thread-scaling measurements become meaningless |

Accordingly, the policy for this environment is **“share the environment definition, not the instance.”** One measurement run uses one instance. Terminate it after use. The dedicated instance itself provides exclusivity, so no locking mechanism is needed.

## Why change the instance type from the T series

| | Old | New (default) |
|---|---|---|
| Type | `t3.large` | `m5d.large` |
| vCPU / RAM | 2 / 8 GiB | 2 / 8 GiB (**unchanged**) |
| CPU performance | Burstable (credit-based) | Fixed |
| EBS bandwidth | Burstable | Fixed |
| Storage | EBS gp3 | **Local NVMe** (instance store) |

vCPU and RAM remain unchanged so that **past measurements remain comparable in shape**. Only the sources of nondeterminism were changed.

Local NVMe was chosen for two reasons:

1. Remove the network-based EBS component from the measurement
2. **Allow `mkfs` to be used freely**, so xfs / ext4 / btrfs can be placed side by side (required for Linux-specific feature verification)

To measure thread scaling, specify `--instance-type m5d.2xlarge` (8 vCPUs).
**Values cannot be compared directly with values from the two-vCPU era**, so measure the comparison target with the same type as well.

### Do not use Spot instances

If an interruption occurs during a measurement, the run can only be discarded. If only uninterrupted runs remain, the results acquire survivorship bias. Use on-demand instances for measurement.

## Usage

```bash
# 1. Launch (one dedicated instance is started)
./aws/launch.sh --project myproject --ttl 120

# 2. Connect using the printed ssh command. /mnt/xfs /mnt/ext4 /mnt/btrfs are prepared
#    Source protocol/lib.sh to use the measurement protocol

# 3. Terminate (it is also automatically terminated after the TTL)
./aws/terminate.sh --project myproject
```

`--ttl` is a hard limit in minutes. Even if it is omitted, the default of 180 minutes always terminates the instance.
**The judgment is that letting charges continue because an instance was forgotten is more harmful than having it terminate during a measurement.**

## Measurement protocol (protocol/lib.sh)

Rules that HyperDiskUsage has **broken once** are enforced in **code**, rather than in comments. The protocol accepts comparison commands as arguments so that other projects can use the same generic form.

| Rule | Rationale |
|---|---|
| Minimum for warm, **median for cold** | Using the minimum for cold would report a burst state as the steady state |
| Burn in before alternating cold runs | Present both tools with the same steady state |
| Always record environment metadata | The measurement has no meaning if the conditions for a number cannot be reconstructed later |

`protocol/verify.sh` checks whether the environment can be measured before starting. If it cannot, it **aborts** (warnings are easy to skip).

## Cost

Cost allocation can be tracked with the `--project` tag. View the breakdown by project with a tag filter in Cost Explorer. Refer to the AWS pricing page for exact rates (they vary by instance type and region, so rates are not listed here).

Because TTL automatically terminates the instance, runaway charges from forgetting to delete it do not occur.

## Untested items

The scripts in this directory **have not yet been execution-verified in a session with valid AWS credentials** (`sts get-caller-identity` failed with `InvalidClientTokenId` in the session in which they were created). The following items must be checked on the first run:

- Whether `launch.sh` finds the intended Amazon Linux 2023 image
- Whether the instance-store device name (`/dev/nvme1n1`) is correct for `m5d.large`
- Whether cloud-init completion detection works
- Whether the instance-store capacity of `m5d.large` is sufficient for the target tree
