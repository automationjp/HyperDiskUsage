# HyperDiskUsage (HyperDU)

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-%23000000.svg?style=flat&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20Linux%20%7C%20macOS-blue)](https://github.com/yourusername/HyperDiskUsage)

**HyperDU** は、高速なディスク使用量分析を目指した Rust 製ツールです。並列処理とOS固有APIの最適化により、従来のツールより高速に動作する可能性があります。

> **⚠️ 動作確認について**: Windows と Linux は実機で検証済みです（CI でも両方をテストしています）。macOS はビルドのみで実機検証していません。

## 🚀 特徴

- **高速スキャン**: 並列処理とプラットフォーム最適化による高速化を目指しています
- **du 互換モード**: `--compat gnu` オプションで GNU du 風の出力形式に対応
- **マルチプラットフォーム対応**: Windows (検証済み), Linux (検証済み), macOS (実機未検証)
- **並列処理**: ワークスティーリングアルゴリズムによる効率的な並列化
- **プラットフォーム最適化**:
  - Linux: `getdents64` システムコール + `statx`
  - Windows: `NtQueryDirectoryFile` による一括列挙（物理サイズ・ファイルIDを列挙結果から直接取得。`FindFirstFileExW` はフォールバック）
  - macOS: `getattrlistbulk` による一括取得
- **リアルタイムチューニング**: 実行中にパフォーマンスパラメータを自動調整
- **多様な出力形式**: du互換出力、CSV、JSON、独自の詳細表示
- **GUI版も提供**: CLI版に加えて、直感的なGUIアプリケーション

## 📊 パフォーマンスについて

本ツールは OS 固有の列挙 API と並列処理を最適化し、高速なスキャンを目指しています。実測の性能はストレージ（NVMe/SSD/HDD/ネットワーク）、ファイル構成、除外条件、オプション設定に大きく依存します。再現可能な比較が必要な場合は、同一環境・同条件で `hyperdu-cli --progress` と `du` などを用いてベンチマークを取得してください。

### パフォーマンスの秘密

1. **プラットフォーム最適化**
   - Linux: `getdents64` システムコール + `statx`
   - Windows: `NtQueryDirectoryFile`（`FileIdFullDirectoryInformation`）で 128KiB 単位に一括取得。割り当てサイズとファイル ID も同時に得られるため、物理サイズ計算やハードリンク重複排除でファイル毎のシステムコールが発生しません（MSVC ビルド既定。`HYPERDU_WIN_USE_NTQUERY=0` で `FindFirstFileExW` 経路に切替、`HYPERDU_WIN_DIR_BUF_KB` で列挙バッファサイズを変更）
   - macOS: `getattrlistbulk` による名称・型・サイズのバルク取得

2. **並列処理**
   - ワーカー毎の LIFO デック＋ワークスティーリング（浅い＝大きなサブツリーから盗む）による動的負荷分散
   - 処理中ジョブを数えて終了判定するため、キューが一瞬空いてもワーカーが早期終了しない
   - 巨大ディレクトリの継続ジョブは高優先度キューで先に処理

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
- **Linux**: 検証済み（Amazon Linux 2023 / xfs、WSL2 / ext4）
- **macOS**: 実機未検証

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

## 🔥 パフォーマンス

数値はすべて `scripts/bench_du.sh` による実測です。同スクリプトは、古いバイナリを測ろうとした場合と、両ツールが同じファイル集合を走査していない場合に**エラーで停止**します。

### GNU du 8.32 との比較（EC2 t3.large / xfs / kernel 6.1）

| ツリー | files | dirs | warm | cold |
|---|---|---|---|---|
| Linux カーネル | 95,953 | 6,299 | 1.32x | **4.93x** |
| rust リポジトリ | 62,621 | 4,734 | 1.36x | **5.56x** |
| git リポジトリ | 4,874 | 242 | 1.00x | 2.02x |

warm は 5 回の最小値、cold は 3 回の burn-in 後に両ツールを交互実行した 5 組の中央値です。ファイル数は 3 つとも `find` と完全一致しています。

### warm の比は物理コア数の関数です

**du は単一スレッド**なので、比は走査対象よりも物理コア数で決まります。上表の t3.large は **2 vCPU = 物理 1 コア + SMT** で、並列化の余地がほぼありません。参考までに WSL2（物理 8 コア / 16 スレッド、ext4）では同じ rust ツリーで **7.18x** です。

**cold こそが実用上の主戦場**です。ディスク使用量の調査は通常 1 回きりで、ページキャッシュは温まっていません。

### Windows

`NtQueryDirectoryFile` + `FileIdFullDirectoryInformation` への変更により、旧実装比 **4.7x**。1 回の列挙でサイズ・割り当てサイズ・ファイル ID が同時に得られるため、ファイルごとのハンドル open が不要です。

### なぜ Linux は Windows より差が小さいのか

構造的な理由があります。`struct dirent64` は `d_ino` / `d_off` / `d_reclen` / `d_type` / `d_name` の 5 フィールド固定で、**サイズを入れる場所がありません**。さらに VFS の `dir_emit()` / `filldir64()` のコールバック署名が `(name, namelen, ino, dtype)` しか受け取らないため、どのファイルシステムであれ列挙時にサイズを返せません。

したがって Linux では**ファイル 1 個につき `statx` が 1 回**必要で、syscall 数はファイル数に比例します（Windows はディレクトリ数に比例）。GNU du も同じ制約下にあるため、warm では両者が同じ壁に当たります。

### du 互換モードでの動作確認

```bash
# 従来の du コマンド
du -sh /path/to/directory

# HyperDU で完全互換動作
hyperdu-cli --compat gnu -sh /path/to/directory

# du 互換の出力形式を目指しています
```

### プラットフォーム別最適化

- Linux: `getdents64` + `statx`

#### FS戦略の振る舞い（Linux）

- Ext4/XFS/ZFS
  - getdents_buf_kb=128、prefetch=1（posix_fadvise+readahead/madvise）
- Btrfs
  - compute_physical=false（論理サイズ優先）
  - getdents_buf_kb=128、prefetch=0
- DrvFS（WSL）/Network（NFS/SMB/SSHFS/9p/fuse）
  - compute_physical=false、getdents_buf_kb=64、prefetch=0
  - 推奨 threads: 4（必要に応じてランタイムチューニングが増減）

適用時は stderr に詳細ログを 1 行出力: 例

```
fs-auto: fs='ext4' strategy='ext4' reason='fstype=ext4' changes=[getdents_buf_kb=128,prefetch=1] for '/data'
```
- Windows: `NtQueryDirectoryFile` による列挙（既定）。物理サイズはディレクトリ列挙が返す割り当てサイズ（クラスタ単位）で、GNU du のブロック集計に相当します。`--logical-only` で論理サイズのみを集計できます。`HYPERDU_WIN_USE_NTQUERY=0` を設定すると `FindFirstFileExW` 経路（物理サイズはファイル毎に `GetCompressedFileSizeW`）に切り替わります。`\?\` プレフィックスにより 260 文字を超えるパスも走査します。
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

**GNU du との比較には `scripts/bench_du.sh` を使ってください。** 公平性の条件をスクリプト側で強制します。

```
# warm のみ
scripts/bench_du.sh /path/to/tree

# cold も測る（drop_caches のため root/sudo が必要）
scripts/bench_du.sh --cold /path/to/tree1 /path/to/tree2
```

このスクリプトは以下を**自動で検出して停止**します。いずれも過去に実際にやらかした失敗です。

| 検出 | 背景 |
|---|---|
| **古いバイナリ** | `cargo build --features X` が `target/release` を上書きし、後の計測が古いバイナリを測っていた |
| **走査対象の不一致** | 既定除外の `.git` が `.github` に部分一致し、HyperDU だけ 437 ディレクトリ少なく走査していた。`find` のファイル数と突き合わせる |
| **cold の最小値** | EBS gp3 のバースト枯渇で同一条件が 0.495s → 0.88s と 1.8 倍ぶれる。最小値はバースト状態だけを拾うため、burn-in → 交互実行 → **中央値**とする |

ヘッダには commit、未コミット変更の有無、物理コア数、ファイルシステム種別、カーネルを出力します。後から「どの条件の数字か」を復元できるようにするためです。

HyperDU の変種間（rayon-par など）の比較には `scripts/bench.sh` を使います。

```
scripts/bench.sh --root /path/to/dir
WITH_RAYON=1 scripts/bench.sh --root /path/to/dir
```

回帰基準（目安）

### ランタイムチューニング（任意・上級者）

- `hyperdu-cli … --tune` でアダプティブチューナを有効化（dir_yield/実行スレッド数を動的調整）
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
- **Windows**: 検証済み（NTFS、CI あり）
- **Linux**: 検証済み（Amazon Linux 2023 / xfs、WSL2 / ext4、CI あり）
- **macOS**: 実機未検証（ビルドは可能）

### その他の問題
- WSL環境: `/mnt/*` (NTFS) でビルド時に一時ディレクトリ削除エラーが出る場合があります。Linux側にリポジトリを配置するか、`CARGO_TARGET_DIR` を設定してください
- シンボリックリンク: デフォルトでは追跡しません。`--follow-links`で有効化できますが、循環参照に注意してください

---

**HyperDU** - ディスク使用量分析を、より速く、より効率的に。
