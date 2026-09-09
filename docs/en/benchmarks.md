# HyperDU compared with du

A fresh AWS EC2 benchmark is being prepared. Only results with directly matching GNU du directory totals will be published: allocated bytes, no symlink following, hardlink deduplication, one filesystem and all directory rows. The previous WSL2 timings required an accounting adjustment and have been withdrawn as evidence of comparative speed.

## Acceptance protocol

Build the latest source snapshot in release mode on AWS EC2 Linux x86_64. Record source/binary hashes, GNU du version, acquisition time, instance/CPU/RAM/EBS/filesystem/kernel. Both tools use the same immutable data, privileges, accounting and output granularity. No Python byte adjustment is permitted.

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

Every directory row and byte total must match directly before and during timing. Errors, mismatches or dataset changes invalidate the measurement. Select performance settings in a separate pilot and freeze them before eight alternating warm trials per tool. Include startup/output costs, all samples, medians and unfavorable results. Approximate or logical-only sizes cannot substitute for allocated bytes.

Linux uses the same enumeration engine for a mounted filesystem root and a directory; there is no Linux MFT fast path. A dedicated EBS mount root and representative directory are possible scopes. A changing operating-system root is excluded. Final commands will disclose all selected performance flags.

## Status

Local Linux regression tests reproduced the discrepancy and verified direct GNU du parity after the fix; these are correctness tests, not AWS performance measurements. AWS target and valid credentials are still pending, so there are no current AWS speed figures yet.

[Reproducible benchmark runner](../../scripts/bench/du_same_conditions.py)

[AWS runbook and provenance records](../benchmarks/aws-protocol.md)
