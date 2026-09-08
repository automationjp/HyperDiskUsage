# Old documentation

このディレクトリには、過去の benchmark、Issue 固有の設計、実装・検証記録を保存します。

**現在の仕様・現在の性能値を確認する場合は `docs/` 直下を参照してください。**

## Archived documents

| Document | Meaning |
|---|---|
| [benchmarks-2026-09-07.md](benchmarks-2026-09-07.md) | 2026-09-07 時点の旧 benchmark。現在の性能主張には使用しない |
| [design/persistent-index.md](design/persistent-index.md) | Issue #16 に対する旧 persistent-index 設計案 |
| [design/issue-16-41-implementation.md](design/issue-16-41-implementation.md) | Issue #16 / #41 の当時の実装・受入記録 |
| [design/mft-parity-verification.md](design/mft-parity-verification.md) | MFT backend の過去の検証記録 |

## Why these are archived

これらは削除せず、設計判断の経緯や再発防止の evidence として残しています。

一方で、次の理由から current documentation と同じ階層には置きません。

- 当時の commit や実装状態を前提としている
- Issue の途中段階の提案を含む
- 過去の benchmark 数値を含む
- 現在仕様と将来案が混在する文書がある

現在の入口:

- [Documentation index](../README.md)
- [Performance design](../performance.md)
- [Benchmark plan](../benchmarks.md)
- [Architecture](../architecture.md)
