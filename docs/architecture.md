# Architecture

HyperDU は、**高速な scanner core を 1 つ持ち、その上に CLI / GUI / MCP を載せる**構成です。

性能に関わる処理を interface ごとに重複実装せず、`hyperdu-core` に集約することで、CLI・GUI・AI Agent が同じ走査 semantics と platform fast path を共有します。

## Overview

```text
                 +------------------+
                 |   hyperdu-cli    |
                 +------------------+
                          |
                 +------------------+
                 |   hyperdu-gui    |
                 +------------------+
                          |
                 +------------------+
                 |   hyperdu-mcp    |
                 +------------------+
                          |
                          v
                 +------------------+
                 |  hyperdu-core    |
                 | scanner / model  |
                 +------------------+
                    /      |       \
                   /       |        \
                  v        v         v
             Linux      Windows     macOS
          getdents64   NtQuery...  getattrlistbulk
            + statx      + MFT
```

## Components

| Component | Responsibility | Performance role |
|---|---|---|
| `hyperdu-core` | tree scan、集計、platform abstraction | hot path の中心 |
| `hyperdu-cli` | command parsing、表示、export | scanner を呼ぶ薄い interface |
| `hyperdu-gui` | desktop UI、drill-down | 同じ core scanner を利用 |
| `hyperdu-mcp` | structured MCP tools | 同じ core scanner を agent 向けに公開 |
| `plugin/` | Agent Skill / Plugin | CLI / MCP の利用手順を提供 |
| `scripts/bench/` | benchmark harness | performance claim の再現性を担保 |

## Scan data flow

通常 scan は概ね次の流れです。

1. CLI / GUI / MCP が scan option を構築する
2. `hyperdu-core` が target filesystem と platform を判定する
3. platform-specific enumeration path を選ぶ
4. directory work を worker queue に投入する
5. worker が subtree を処理し、必要に応じて work stealing する
6. file / directory の logical・physical usage を集計する
7. hardlink 等の重複を処理する
8. subtree totals を親へ roll up する
9. interface が human-readable / JSON / CSV / structured MCP result に変換する

## Platform boundary

### Linux

```text
Directory
   |
   +--> getdents64  -- names / inode / type --> worker
                                             |
                                             +--> statx --> size metadata
```

Linux では directory enumeration と metadata 取得が分かれます。正確なサイズを取る以上 metadata syscall 自体は必要なので、列挙効率・並列化・filesystem strategy が重要になります。

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

Windows では 1 回の batch enumeration からサイズと file ID まで取得できるため、追加 handle open を減らせることが高速化の中心です。

### Optional MFT path

```text
--mft + supported NTFS volume root
              |
              v
        read / parse $MFT
              |
       complete + valid ?
          /          \
        yes           no
         |             |
         v             v
      result      directory enumeration
```

MFT backend は optional fast path です。解析できない layout を無理に部分集計せず、通常経路へ fallback します。

## Concurrency model

単純な固定 range 分割では、directory tree のサイズ偏りによって worker の idle が増えます。

HyperDU は worker ごとの LIFO deque を持ち、他 worker が work を steal できる構造にしています。

狙いは以下です。

- locality を保ちながら現在の subtree を深掘りする
- idle worker が大きな未処理 subtree を引き取る
- queue の瞬間的な空状態を scan 完了と誤認しない

thread 数そのものではなく、**tree shape の偏りに追従すること**が目的です。

## Correctness before speed

HyperDU の fast path は、結果 semantics を変えないことを前提にします。

特に以下は performance optimization のために省略しません。

- hardlink accounting
- logical / physical size の区別
- unsupported fast path の fallback
- incomplete MFT parse の拒否
- scan error / cancellation の扱い

速度値を公開する場合も、先に scan parity を確認します。手順は [Benchmark plan](benchmarks.md) を参照してください。

## Persisted Linux snapshots

`index refresh` / `index show` は通常 scan の置き換えではなく、明示的に保存した directory aggregate を再利用する別機能です。

自動 watcher ではなく、`show` の結果は `stale` と明示されます。詳細は [Linux directory snapshots](index-snapshots.md) を参照してください。

## Historical design records

過去の Issue 固有設計、MFT 検証記録、旧 persistent-index 案は [old/README.md](old/README.md) へ移動しています。

現在の architecture を理解する入口としては、この文書と [Performance design](performance.md) を使用してください。
