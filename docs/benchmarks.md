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
