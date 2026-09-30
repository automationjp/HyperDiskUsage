# HyperDU と du の比較

成功した測定は独立oracleと一致し、比較が完了した測定はGNU `du`とも直接一致しています。外部のバイト補正は行わず、旧WSL2の速度値は使用しません。測定したソースは`2645689515ab2e608a78b7492637e55179e4739a`です。詳細なhash、全sample、corpus fingerprintは[公開測定結果JSON](https://automationjp.github.io/HyperDiskUsage/benchmarks.json)にあります。

## Linux結果（GitHub Actions）

Ubuntu 24.04/ext4で、256 Bの通常ファイル100万個をflat／wide／deepの3形状に配置しました。各ツールを交互に8回warm計測し、全48 raw sampleを保持しています。プロセス起動とディレクトリ出力を含み、全ディレクトリ行の割当バイト数が直接一致しています。

Linuxの中央値（HyperDU / GNU `du`、GNU `du` / HyperDUの速度比）はflat 2327.49 / 2763.52 ms / 1.187x、wide 745.96 / 2428.80 ms / 3.256x、deep 764.67 / 2434.28 ms / 3.183xです。

保守的な見出し値は3.25x（wide、Linuxのみ）です。[GitHub ActionsのLinux実行](https://github.com/automationjp/HyperDiskUsage/actions/runs/34550503086)でも、同じsource／build／corpus／parityの証跡を確認できます。

### Linux command

```bash
hyperdu --compat gnu-strict --block-size 1 --one-file-system ROOT
du -x --block-size=1 ROOT
```

両方とも全ディレクトリ行を出力します。比較時だけ行順を正規化し、合計値やバイト数は補正しません。

## Windows補足結果（ローカル）

Windows 11のNTFS/NVMeで、Ryzen 9 3900X（12 cores／24 threads、128 GiB）を使用しました。GNU `du`はGit for Windows MSYSの8.32です。両ツールに`--apparent-size`を指定して論理バイトを集計し、warm計測を行いました。

1M flatではHyperDU単独を8回測定し、中央値は589.91 msでした。GNU `du`はwarmup中に600秒でtimeoutしたため、受理済みの1M比較も1M比率もありません。wide／deepの1M測定は未実行です。

別の10K診断では、3形状を各ツール2回ずつ交互に実行し、12 measured sampleと6 warmupを記録しました。全ディレクトリ行は一致していますが、比率はこの小規模診断だけの値で、1Mの一般的な見出し値には使いません。

10K診断の中央値（HyperDU / GNU `du`、GNU `du` / HyperDUの速度比）はflat 32.98 / 704.17 ms / 21.35x、wide 27.46 / 712.33 ms / 25.94x、deep 32.32 / 836.73 ms / 25.89xです。

### Windows command

```powershell
hyperdu --compat gnu-strict --apparent-size --block-size 1 --one-file-system --io-profile balanced ROOT
du -x --apparent-size --block-size=1 ROOT
```

AWSのrunbookは未実行の計画として残しています。現在の結果や速度値には含めません。[AWS runbook（未実行計画）](benchmarks/aws-protocol.md)

[再現可能なLinux benchmark runner](../scripts/bench/du_same_conditions.py)


## dua-cli・tokei との比較（Windows ローカル）

`d4ebdb85577c53e881af292b549bc784f05bec60`（v0.5.0-beta.5 の開発版）から `cargo build --locked --release -p hyperdu`（rustc 1.98.0）でビルドした HyperDU と、dua-cli 2.45.0・tokei 15.0.0（どちらも `cargo install --locked` で同じ rustc からビルド）を比べました。Windows 11 / NTFS / NVMe、Ryzen 9 3900X（12 cores / 24 threads、128 GiB RAM）で、warm cache のままツールを入れ替えながら各12回ずつ実行し、全 raw sample を[記録 JSON](benchmarks/2026-09-30-windows-vs-dua-tokei.json)に保存しています。時間はプロセス起動から終了までで、標準出力への書き出しを含みます。各データセットは計測前後に独立した lstat 走査で内容が変わっていないことを確認しました。

### dua-cli（同じ仕事の比較）

HyperDU と dua-cli の論理バイト合計は、5つすべてのデータセットで独立走査の値と一致しました。外部でのバイト補正はしていません。

| データセット | ファイル数 | HyperDU | dua-cli | dua / HyperDU |
|---|---:|---:|---:|---:|
| 横に広いフォルダ（合成） | 99,856 | 67.57 ms | 187.37 ms | 2.77倍 |
| 深い階層のフォルダ（合成） | 100,000 | 179.68 ms | 194.62 ms | 1.08倍 |
| 平坦なフォルダ（合成） | 100,000 | 98.80 ms | 997.51 ms | 10.10倍 |
| Cargo registry（実データ） | 107,953 | 478.55 ms | 554.61 ms | 1.16倍 |
| node_modules（実データ） | 27,940 | 72.26 ms | 156.60 ms | 2.17倍 |

HyperDU は5件すべてで短い時間でしたが、差は木の形に左右されます。平坦なフォルダを除くと 1.08〜2.77倍です。

- 深い階層（1.08倍）は、走査を並列に広げられる幅がほとんどありません。ばらつき（HyperDU 159〜264 ms、dua-cli 170〜312 ms）が重なるため、ほぼ同等と読むのが妥当です。
- 平坦なフォルダの10.10倍は代表値に使えません。dua-cli は入力直下の全エントリを1行ずつ出力するため（100,001行）、その出力時間を含みます。`--depth 1` は出力を減らさず、合計も独立走査と一致しないため使っていません。
- 実データの Cargo registry は1.16倍、node_modules は2.17倍でした。

### tokei（参考。別の仕事）

tokei はソースコードの行数を数えるツールで、ファイルの中身を読んで言語ごとに解析します。HyperDU はサイズの集計だけで中身を読みません。同じ仕事ではないため、**速度比は示さず**、時間だけを参考に載せます。合成データには拡張子がなく tokei が何も解析しないため、実データ2種だけを測りました。

| データセット | ファイル数 | tokei が解析したファイル数 | HyperDU | tokei |
|---|---:|---:|---:|---:|
| Cargo registry（実データ） | 107,953 | 80,897 | 478.55 ms | 9,407.19 ms |
| node_modules（実データ） | 27,940 | 16,576 | 72.26 ms | 782.31 ms |

### 限界

- 測定中、別プロジェクトのジョブ（densel の GC と Python ワーカー）が24論理 CPU の60〜90%を使っていました。ツールを交互に実行して同じ負荷を受けるようにしましたが、絶対値は膨らんでおり、単発の時間は大きくぶれます（HyperDU の平坦は 68-1,096 ms）。分離した測定ホストではありません。
- cold cache は測っていません。tokei はファイルの中身を読むため、warm の値を cold へ当てはめることはできません。
- 合成データは拡張子のない 4 KiB の通常ファイルで、dua-cli の出力形式（直下の全エントリを列挙）の影響を受けます。示した木以外へ一般化しません。
- 実データは当初もう1種（Rust toolchains、345,102ファイル）を予定していましたが、tokei の1回が10分を超えたため中止して外しました。原因は調べていません。
- HyperDU は origin/main の d4ebdb8 で、公開済み v0.5.0-beta.5 のバイナリとは別です。

### コマンド

```powershell
hyperdu ROOT --top 1 --exclude "" --one-file-system
dua aggregate -A -x -f bytes ROOT
tokei ROOT --output json
```

[比較用 benchmark runner](../scripts/bench/compare_tools.py)
