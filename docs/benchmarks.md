[日本語](benchmarks.md) | [English](en/benchmarks.md) | [简体中文](zh-CN/benchmarks.md)

# Benchmarks

測定日 2026-09-09。対象ソース `0a089c90c0e2995ef702ff053820d129265c3659`。

Windows / NTFS と Linux / WSL2 / ext4 で、同じデータを両ツール交互に8回走査した warm 計測です。処理時間はプロセス起動と出力を含む中央値。倍率は比較対象 / HyperDU で、1未満はHyperDUが遅い結果です。

## 環境

- Windows 11 Pro 10.0.26200、Ryzen 9 3900X (12C/24T)、128 GiB RAM。synthetic は F: の WD_BLACK SN850X HS 4 TB、registry は C: の GIGABYTE GP-ASM2NE6100TTTD、いずれも NTFS / NVMe。robocopy の取得バージョンは下記のJSONに記録。
- Linux は同一PCの WSL2 Ubuntu 26.04、kernel 6.18.33.2、16 vCPU、約90 GiB RAM、ext4。比較対象は uutils du 0.8.0。GNU du およびベアメタルLinuxの測定ではありません。

## 結果

### Windows / NTFS / Native

Binary: `hyperdu 0.5.0-beta.3`、SHA-256: `b4c4c4ca78119d61319c9b0a342ab7b7e7dfed3992a44fe56d88ddb5b6a9c3d8`。

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 30.721 | 57.424 | 1.87x |
| deep | 2,000 | 77.123 | 72.041 | 0.93x |
| flat | 20,000 | 34.081 | 42.825 | 1.26x |
| registry | 111,813 | 332.236 | 1838.460 | 5.53x |

全試行 (ms):

```text
wide       hyperdu    34.665 30.334 33.340 29.218 31.107 30.203 29.661 32.721
wide       baseline   60.855 55.954 58.894 62.127 53.976 65.117 55.036 55.126
deep       hyperdu    80.579 82.515 92.383 76.418 75.246 72.922 77.827 75.048
deep       baseline   72.060 81.065 76.229 72.021 79.658 71.715 69.163 71.954
flat       hyperdu    35.492 29.856 33.223 30.974 35.251 34.938 44.015 31.479
flat       baseline   42.960 42.689 43.561 50.821 37.715 39.821 73.456 41.414
registry   hyperdu    329.521 321.446 325.137 330.354 334.119 356.102 343.325 514.561
registry   baseline   1691.376 1823.197 1937.345 1853.723 1592.403 1687.168 2615.618 2950.394
```

[コマンド・集計照合・全試行のJSON](benchmarks/2026-09-09-windows.json)

### Linux / ext4 / WSL2

Binary: `hyperdu 0.5.0-beta.3`、SHA-256: `a43fff5f5b7a7663b15f081c1361256ef111461848f1fe49419b0e829bd32eb4`。

| Dataset | Files | HyperDU (ms) | Baseline (ms) | Ratio |
|---|---:|---:|---:|---:|
| wide | 20,000 | 47.343 | 208.596 | 4.41x |
| deep | 2,000 | 89.107 | 143.359 | 1.61x |
| flat | 20,000 | 130.465 | 184.377 | 1.41x |
| registry | 37,814 | 144.584 | 978.928 | 6.77x |

全試行 (ms):

```text
wide       hyperdu    24.960 19.303 27.865 57.863 83.760 57.849 77.518 36.838
wide       baseline   219.142 186.493 265.028 186.324 211.074 259.606 206.119 145.009
deep       hyperdu    88.301 82.276 80.782 89.913 103.993 111.027 110.154 84.604
deep       baseline   161.231 157.352 143.158 148.519 141.402 114.486 143.561 133.878
flat       hyperdu    118.474 105.591 140.027 130.245 151.339 181.407 89.872 130.685
flat       baseline   173.565 138.248 179.544 193.533 189.211 237.937 189.578 157.039
registry   hyperdu    115.010 142.254 146.913 107.350 150.306 153.823 178.075 112.644
registry   baseline   944.617 947.984 908.689 1558.686 1043.559 1187.297 1009.872 839.885
```

[コマンド・集計照合・全試行のJSON](benchmarks/2026-09-09-linux.json)

## 方法と照合

`scripts/bench/remeasure.py` は wide (500ディレクトリ x 40ファイル)、deep (400段 x 5ファイル)、flat (20,000ファイル) を作成します。各ファイルは4,096 bytes。追加のCargo registryはOSごとに異なる集合なので、OS間の速度比較には使用しません。

HyperDU は `--top 1 --exclude "" --one-file-system`、Windowsは `robocopy /L /S /XJ /BYTES`、Linuxは `du -sx --block-size=1`。最初の照合走査でキャッシュを暖めてから、HyperDU先行と比較対象先行を交互に実行します。

独立したPython lstat走査と、ファイル数・ディレクトリ数・論理サイズを照合します。Linuxは通常ファイルの物理サイズも照合し、duの値にはディレクトリとリンク自身の割当量を加えます。Windowsの物理サイズ照合は未実施です。計測後に集合とHyperDU集計が変化していないこと、計測前後でバイナリのハッシュが同じことを再確認します。

## 適用範囲

- cold、XFS、macOS、MFT直接走査は未測定です。
- この表はCLIの一括走査です。GUIの描画やインタラクティブモードの時間を表すものではありません。
- robocopyはコピー計画用の列挙、duは割当量の集計であり、各ツールの機能全体が同一という意味ではありません。
- エラー・リンク・圧縮・スパース等の特殊ケースはこの速度表だけで保証できません。意味の一致は別途Rustテストで検証します。
- 共有開発PCでの測定です。他タスクのビルドを含むバックグラウンド負荷とWSL2の影響を排除できていません。差の小さい結果を一般化しません。

## 再実行

```powershell
cargo build --release --locked -p hyperdu
python scripts/bench/remeasure.py --bin <release-binary> --commit <source-sha> --output <results.json> --dataset-parent <scratch> --runs 8
```

既存のwide/deep/flatを再利用する場合は `--synthetic-root <directory>`、実データを追加する場合は `--tree <directory>` を指定します。出力の `complete: true` を確認してから比較してください。

[Build provenance](benchmarks/build-record.md)
