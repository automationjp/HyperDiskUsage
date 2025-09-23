# HyperDiskUsage (HyperDU)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=flat&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/yourusername/HyperDiskUsage)

**HyperDU** は、高速なディスク使用量分析を目指した Rust 製ツールです。並列処理とOS固有APIの最適化により、従来のツールより高速に動作する可能性があります。

> **⚠️ 動作確認について**: 現在 Windows 環境でのみ動作確認済みです。Linux/macOS での動作は未確認のため、これらの環境では動作しない可能性があります。

## 🚀 特徴

- **高速スキャン**: 並列処理とプラットフォーム最適化による高速化を目指しています
- **du 互換モード**: `--compat gnu` オプションで GNU du 風の出力形式に対応
- **マルチプラットフォーム対応予定**: Windows (動作確認済み), Linux (未確認), macOS (未確認)
- **並列処理**: ワークスティーリングアルゴリズムによる効率的な並列化
- **プラットフォーム最適化**:
  - Linux: `getdents64` システムコール、io_uring（実験的）
  - Windows: `FindFirstFileExW` + NT Query API
  - macOS: `getattrlistbulk` による一括取得
- **リアルタイムチューニング**: 実行中にパフォーマンスパラメータを自動調整
- **多様な出力形式**: du互換出力、CSV、JSON、独自の詳細表示
- **GUI版も提供**: CLI版に加えて、直感的なGUIアプリケーション

## 📊 パフォーマンスについて

本ツールは OS 固有の列挙 API と並列処理を最適化し、高速なスキャンを目指しています。実測の性能はストレージ（NVMe/SSD/HDD/ネットワーク）、ファイル構成、除外条件、オプション設定に大きく依存します。再現可能な比較が必要な場合は、同一環境・同条件で `hyperdu-cli --progress` と `du` などを用いてベンチマークを取得してください。

### パフォーマンスの秘密

1. **プラットフォーム最適化**
   - Linux: `getdents64` システムコール + `statx`（io_uring は現状実験的で未統合）
   - Windows: `FindFirstFileExW` with `FIND_FIRST_EX_LARGE_FETCH`
   - macOS: `getattrlistbulk` による名称・型・サイズのバルク取得

2. **並列処理**
   - ワークスティーリングによる動的負荷分散
   - マルチスレッドでディレクトリを並行スキャン
   - 二段キュー（High/Normal）による優先度制御

3. **メモリ最適化**
   - `mimalloc` アロケータ（オプション機能）
   - `ahash` による高速ハッシュマップ
   - Aho-Corasick による高速パターンマッチング

## 📦 インストール

### 事前ビルド（推奨）

以下は GitHub Releases の最新版への直接リンクです。ダウンロードして展開するだけで実行できます。

- Windows (x86_64)
  - CLI: [hyperdu-cli-windows-x86_64-generic.zip](releases/latest/download/hyperdu-cli-windows-x86_64-generic.zip)
  - GUI: [hyperdu-gui-windows-x86_64-generic.zip](releases/latest/download/hyperdu-gui-windows-x86_64-generic.zip)
- Linux (x86_64, glibc)
  - CLI: [hyperdu-cli-linux-x86_64-generic.zip](releases/latest/download/hyperdu-cli-linux-x86_64-generic.zip)
  - GUI: [hyperdu-gui-linux-x86_64-generic.zip](releases/latest/download/hyperdu-gui-linux-x86_64-generic.zip)
- Linux (x86_64, musl・CLIのみ)
  - CLI: [hyperdu-cli-linux-x86_64-musl.zip](releases/latest/download/hyperdu-cli-linux-x86_64-musl.zip)
- Linux (aarch64, glibc)
  - CLI: [hyperdu-cli-linux-aarch64-generic.zip](releases/latest/download/hyperdu-cli-linux-aarch64-generic.zip)
  - GUI: [hyperdu-gui-linux-aarch64-generic.zip](releases/latest/download/hyperdu-gui-linux-aarch64-generic.zip)

その他のアセット（チェックサム等）は [最新リリース一覧](releases/latest) を参照してください。

### ソースからビルド

```bash
# リポジトリのクローン
git clone https://github.com/yourusername/HyperDiskUsage.git
cd HyperDiskUsage

# リリースビルド（最高パフォーマンス）
RUSTFLAGS="-C target-cpu=native" cargo build --release --all

# インストール
cargo install --path hyperdu-cli
cargo install --path hyperdu-gui  # GUI版（オプション）
```

### 必要な環境

- Rust 1.75 以降
- **Windows**: Visual Studio 2019 以降または MinGW-w64 (動作確認済み)
- **Linux**: 未確認（io_uring 機能を使用する場合はカーネル 5.6+ が必要）
- **macOS**: 未確認

## 🎯 使い方

### du コマンドの代替として（互換モード）

```bash
# 従来の du を HyperDU に置き換え
alias du='hyperdu-cli --compat gnu'

# du と同じオプションがそのまま使える
hyperdu-cli --compat gnu -sh /var/log
hyperdu-cli --compat gnu -ak /home --max-depth=2
hyperdu-cli --compat gnu -b --time /usr/share

# du 互換の出力形式で高速動作を目指しています
```

### HyperDU 独自の高速スキャン

```bash
# カレントディレクトリをスキャン（デフォルトは高速モード）
hyperdu-cli .

# ターボモードで最速スキャン
hyperdu-cli --perf turbo /large/directory

# 進捗表示とライブチューニング付き
hyperdu-cli /large/directory --progress --tune-log

# 特定のディレクトリを除外して高速化
hyperdu-cli . --exclude ".git,node_modules,target,build"

# CSV/JSON形式で出力
hyperdu-cli . --csv output.csv --json output.json
```

### コマンドラインオプション

```
USAGE:
    hyperdu-cli [OPTIONS] <ROOT>

ARGS:
    <ROOT>    スキャンするディレクトリパス

OPTIONS:
    -t, --top <N>                上位N個のディレクトリを表示 [default: 30]
    -e, --exclude <PATTERNS>     除外するパターン（カンマ区切り）
    -d, --max-depth <DEPTH>      最大再帰深度（0 = 無制限）
    -m, --min-file-size <BYTES>  最小ファイルサイズ（バイト）
    -f, --follow-links           シンボリックリンクを追跡
        --threads <N>            ワーカースレッド数 [default: CPU数]
        --csv <PATH>             CSV形式で出力
        --json <PATH>            JSON形式で出力
        --progress               スキャン進捗を標準出力に表示
            --progress-every N   進捗をNファイルごとに表示（既定: 8192）
        --no-uring               Linuxでio_uringを無効化（WSL/ネットワークFS向け）
        --uring-sqpoll           io_uringのSQPOLLを有効化
        --uring-sqpoll-idle-ms   SQPOLLスレッドのアイドル時間（ms）
        --uring-sqpoll-cpu       SQPOLLスレッドのCPU固定
        --uring-coop             cooperative taskrun を有効化
        --classify MODE          種別分類: basic|deep
        --class-report PATH      分類結果をJSONへ出力
        --class-report-csv PATH  分類結果をCSVへ出力
        --incremental-db PATH    スナップショットDB（sled）
        --compute-delta          DBと比較して差分件数を表示
        --update-snapshot        現在状態をDBへ反映し、削除キーを自動prune
        --watch                  変更監視（create/modify/removeを出力）
        --verbose, -v            冗長モード（進捗/ログ詳細 + 既定ファイル名でレポート自動保存）
        --tune-log               ライブチューニングログを表示
        --tune-threshold <N>     チューニング閾値（デフォルト: 0.05 = 5%）
        --tune-only              チューニングのみ実行（推奨値を表示）
        --tune-secs <N>          チューニング実行時間（秒）
    -h, --help                   ヘルプを表示
    -V, --version                バージョンを表示
```

### 高度な使用例

```bash
# 1GB以上のファイルのみをカウント
hyperdu-cli / --min-file-size 1073741824

# 3階層までの深さでスキャン
hyperdu-cli . --max-depth 3

# 複数の除外パターンを指定
hyperdu-cli ~/projects --exclude ".git,node_modules,target,build,dist"

# 物理サイズの計算をスキップして高速化（論理サイズのみ）
hyperdu-cli / --logical-only

# 推定サイズモードで高速スキャン（精度とトレードオフ）
hyperdu-cli / --approximate

# ライブチューニングのログを表示しながらスキャン
hyperdu-cli /large/directory --progress --tune-log

# 最適なパラメータを2秒間で測定
hyperdu-cli /large/directory --tune-only --tune-secs 2
```

### 環境変数による詳細設定

```bash
# getdents バッファサイズ調整（Linux）
export HYPERDU_GETDENTS_BUF_KB=256

# 大規模ディレクトリの分割処理（ライブ調整で上書きされる）
export HYPERDU_DIR_YIELD_EVERY=10000

# CPUピニング有効化（Linux）
export HYPERDU_PIN_THREADS=1

# io_uring（Linux）
# 実験的→安定化済み。WSL/ネットワークFSでは無効化を推奨
export HYPERDU_DISABLE_URING=0            # 1 で完全無効化（または CLI --no-uring）
export HYPERDU_STATX_BATCH=256           # 初期バッチ
export HYPERDU_URING_SQ_DEPTH=512        # SQ/CQ 深さ（動的に一部調整）
export HYPERDU_URING_SQPOLL=1            # カーネルポーリングを有効化
export HYPERDU_URING_SQPOLL_IDLE_MS=1000 # SQPOLLアイドル時間
export HYPERDU_URING_SQPOLL_CPU=0        # SQPOLLスレッドをCPU固定
export HYPERDU_URING_COOP_TASKRUN=1      # cooperative taskrun 有効

# FS自動最適化（Linux）
# 自動でFSを検出して戦略を適用。無効化は HYPERDU_FS_AUTO=0
export HYPERDU_FS_AUTO=1
export HYPERDU_GETDENTS_BUF_KB=128       # 戦略により 64/128 の推奨を適用
export HYPERDU_PREFETCH=1                # prefetch-advise（posix_fadvise/readahead）を有効/無効
```

CLI から直接指定する場合（環境変数の代替）

- Linux: `--getdents-buf-kb <KiB>`（HYPERDU_GETDENTS_BUF_KB）、`--dir-yield-every <N>`（HYPERDU_DIR_YIELD_EVERY）、`--prefetch`（HYPERDU_PREFETCH=1）、`--pin-threads`（HYPERDU_PIN_THREADS=1）
- io_uring: `--no-uring`（HYPERDU_DISABLE_URING=1）、`--uring-batch <N>`（HYPERDU_STATX_BATCH）、`--uring-depth <N>`（HYPERDU_URING_SQ_DEPTH）、`--uring-sqpoll`/`--uring-sqpoll-idle-ms <MS>`/`--uring-sqpoll-cpu <CPU>`、`--uring-coop`
- Windows: `--win-ntquery`（HYPERDU_WIN_USE_NTQUERY=1）
- チューニング: `--tune`（HYPERDU_TUNE=1）、`--tune-interval-ms <MS>`（HYPERDU_TUNE_INTERVAL_MS）、`--tune-log`
- FS自動最適化: `--no-fs-auto`（HYPERDU_FS_AUTO=0）
- macOS: `--galb-buf-kb <KiB>`（HYPERDU_GALB_BUF_KB）

## 🖼️ GUI版

GUI版は `egui` フレームワークを使用した直感的なインターフェースを提供：

```bash
# GUI版の起動
hyperdu-gui

# 特定のディレクトリを開いて起動
hyperdu-gui ~/Documents
```

### GUI機能

- リアルタイムスキャン表示
- インタラクティブなツリービュー
- files/s（平均/直近）とyield値の表示
- ディレクトリのドリルダウン
- 結果のエクスポート

## 🔥 パフォーマンス目標

### 期待される性能向上

高速化を目指していますが、実際の性能はストレージ種類、ファイルシステム、ファイル構成に大きく依存します。

| 想定環境 | 期待される改善 |
|---------|---------------|
| NVMe SSD (多数の小ファイル) | 大幅な高速化の可能性 |
| HDD (大規模ディレクトリ) | 中程度の高速化 |
| ネットワークドライブ | 環境依存 |

> **注**: 上記は理論値であり、実環境での性能は異なる場合があります。Windows 環境でのみ動作確認済みです。

### du 互換モードでの動作確認

```bash
# 従来の du コマンド
du -sh /path/to/directory

# HyperDU で完全互換動作
hyperdu-cli --compat gnu -sh /path/to/directory

# du 互換の出力形式を目指しています
```

### プラットフォーム別最適化

- Linux: `getdents64` + `statx`。io_uring は安定化済みの高速経路（WSL/ネットワークFSでは自動抑制/フォールバック）

#### FS戦略の振る舞い（Linux）

- Ext4/XFS/ZFS
  - getdents_buf_kb=128、prefetch=1（posix_fadvise+readahead/madvise）
  - io_uring: 有効（環境・オプションに依存）
- Btrfs
  - compute_physical=false（論理サイズ優先）
  - getdents_buf_kb=128、prefetch=0
  - io_uring: 有効（環境・オプションに依存）
- DrvFS（WSL）/Network（NFS/SMB/SSHFS/9p/fuse）
  - compute_physical=false、getdents_buf_kb=64、prefetch=0
  - io_uring: 自動抑制（disable_uring=1）
  - 推奨 threads: 4（必要に応じてランタイムチューニングが増減）

適用時は stderr に詳細ログを 1 行出力: 例

```
fs-auto: fs='ext4' strategy='ext4' reason='fstype=ext4' changes=[getdents_buf_kb=128,prefetch=1] for '/data'
```
- Windows: `FindFirstFileExW`（`FIND_FIRST_EX_LARGE_FETCH`）による列挙
- macOS: `getattrlistbulk` による名称・型・サイズの一括取得

## 🛠️ 開発者向け

### プロジェクト構造

```
HyperDiskUsage/
├── hyperdu-core/     # コアスキャンエンジン
├── hyperdu-cli/      # CLIアプリケーション
├── hyperdu-gui/      # GUIアプリケーション
├── scripts/          # ビルド・パッケージングスクリプト
└── Cargo.toml        # ワークスペース設定
```

### ビルド機能フラグ

```bash
# mimalloc アロケータを無効化
cargo build --release --no-default-features

# Tracy プロファイラサポート
cargo build --release --features prof-tracy

# Puffin プロファイラサポート
cargo build --release --features prof-puffin

# io_uring サポート（Linux、実験的）
cargo build --release --features uring

# SIMD プリフェッチ（実験的）
cargo build --release --features simd-prefetch
```

### 配布用バイナリの作成

簡易パッケージングスクリプトを同梱：

**Unix/WSL:**
```bash
# 基本パッケージング
bash scripts/package_release.sh

# クロスビルドも対応
bash scripts/package_release.sh --targets "linux-musl,windows-gnu"

# CPU最適化オプション
bash scripts/package_release.sh --cpu-flavors "generic,native"

# GUIを省く
bash scripts/package_release.sh --skip-gui
```

**Windows (PowerShell):**
```powershell
# 基本パッケージング
powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1

# CPU最適化版
powershell -ExecutionPolicy Bypass -File scripts\package_release.ps1 -CpuFlavor native
```

### テスト実行

```bash
# すべてのテストを実行
cargo test --all

# ベンチマークを実行
cargo bench

# 特定のテスト実行
cargo test -p hyperdu-core test_name

# Clippy による静的解析
cargo clippy --workspace -- -D warnings

### ベンチと回帰基準

高速化/回帰検知のために簡易ベンチスクリプトを用意しています。

```
# 代表ディレクトリで比較（デフォルト3回平均）
scripts/bench.sh --root /path/to/dir

# rayon-par（自動並列）ビルドも測定
WITH_RAYON=1 scripts/bench.sh --root /path/to/dir

# 期待：NVMe/ext4 等では turbo-uring(+rayon-par) が turbo-off より高速

# 分類・インクリメンタルの測定
scripts/bench.sh --root /path/to/dir   # classify-basic / classify-deep / incr-update / incr-delta を含む
```

回帰基準（目安）
- NVMe/ext4: turbo-uring が turbo-off 比で +10% 以上
- WSL/DrvFS, ネットワークFS: turbo-off（--no-uring）が最速。io_uringは自動抑制または `--no-uring` を推奨

### ランタイムチューニング（任意・上級者）

- 環境変数 `HYPERDU_TUNE=1` でアダプティブチューナを有効化（内部のdir_yield/uringバッチ/有効スレッド数を動的調整）
- スレッドは `active_threads` を動的に制御（[1, threads] 範囲）
  - I/O待ちやSQE失敗が多い→縮退
  - throughput改善が続く→段階的に増加
```

## 🤝 コントリビューション

プルリクエストを歓迎します！大きな変更の場合は、まず Issue を開いて変更内容について議論してください。

1. フォーク
2. フィーチャーブランチ作成 (`git checkout -b feature/AmazingFeature`)
3. 変更をコミット (`git commit -m 'Add some AmazingFeature'`)
4. ブランチをプッシュ (`git push origin feature/AmazingFeature`)
5. プルリクエストを開く

## 📝 ライセンス

このプロジェクトは MIT ライセンスの下でライセンスされています - 詳細は [LICENSE](LICENSE) ファイルを参照してください。

## 🙏 謝辞

- [ripgrep](https://github.com/BurntSushi/ripgrep) - 高速検索の実装参考
- [fd](https://github.com/sharkdp/fd) - 並列ファイルシステム走査の参考
- [dust](https://github.com/bootandy/dust) - UIデザインの参考

## 📈 実装状況と今後の計画

### 今後の計画
- [ ] Linux/macOS での動作確認とテスト
- [ ] 各プラットフォームでのベンチマーク測定
- [ ] 機械学習によるサイズ推定の実装
- [ ] より詳細なドキュメント作成

## ⚠️ 既知の問題と制限事項

### 動作環境
- **Windows**: 動作確認済み
- **Linux**: 未確認（ビルドは可能だが動作未検証）
- **macOS**: 未確認（ビルドは可能だが動作未検証）

### その他の問題
- WSL環境: `/mnt/*` (NTFS) でビルド時に一時ディレクトリ削除エラーが出る場合があります。Linux側にリポジトリを配置するか、`CARGO_TARGET_DIR` を設定してください
- シンボリックリンク: デフォルトでは追跡しません。`--follow-links`で有効化できますが、循環参照に注意してください
- io_uring (Linux): 実装済みですがテストされていません

---

**HyperDU** - ディスク使用量分析を、より速く、より効率的に。
