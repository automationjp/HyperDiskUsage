# AWS EC2: GNU du と同条件で計測する手順

この手順は Linux x86_64 / glibc / GNU coreutils 用です。結果はまだ未取得です。EC2 の接続先と有効な認証を確定してから実行します。新規インスタンスや EBS を作る場合は、リージョン・構成・費用上限も先に確定します。

## ソースとビルド

親作業環境が用意する `aws-source.tar.gz` と `source-snapshot.json` を EC2 の作業領域へ転送します。計測データ、ビルド領域、バイナリ領域、結果領域を別のディレクトリにします。archive は未コミットの変更を含むため、HEAD だけを最新版の証明に使いません。snapshot の `files` の各 SHA-256 が展開後のファイルと一致することを確認し、その後にビルドします。

```bash
# 展開先の source ディレクトリで実行。GNU du がない場合はテストが失敗する。
cargo test --locked -p hyperdu --test du_parity_linux -- --nocapture
cargo test --locked -p hyperdu-core -p hyperdu
cargo test --locked -p hyperdu-core -p hyperdu \
  --features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch
cargo clippy --locked -p hyperdu-core -p hyperdu --all-targets -- -D warnings
python3 -m unittest discover -s scripts/bench -p test_du_same_conditions.py

# portable release と実機 CPU 向け release を別々に保存。
CARGO_TARGET_DIR=target/portable cargo build --locked --release -p hyperdu
RUSTFLAGS='-C target-cpu=native' CARGO_TARGET_DIR=target/native \
  cargo build --locked --release -p hyperdu
```

`Cargo.toml` の release 設定を利用します。各 executable を `hyperdu-config.json` のない別ディレクトリへコピーし、それぞれの build record を作ります。native binary は計測した CPU 用であり、汎用配布バイナリの速度とは区別します。並列 feature を有効にした構成を候補に追加する場合も、同じ feature で正確性テストを通し、別 record を残します。

build record は次の JSON 形式です。値を実測・実取得の値で埋めます。

```json
{
  "head": "source-snapshot.json の head",
  "source_snapshot_sha256": "source-snapshot.json 自体の SHA-256",
  "binary_sha256": "計測する hyperdu executable の SHA-256",
  "build_command": "環境変数・feature を含む実際のビルドコマンド",
  "rustc_version": "rustc -Vv の出力",
  "cargo_version": "cargo -V の出力"
}
```

GNU du は `/usr/bin/du` を使い、package manager の coreutils package/version、`/usr/bin/du --version` の先頭行、executable の SHA-256 を次の record に残します。これらは取得した provenance の整合確認であり、署名付きビルド証明ではありません。

```json
{
  "binary_sha256": "GNU du executable の SHA-256",
  "version": "du --version の先頭行をそのまま記録",
  "package": "OS package manager が示す coreutils package/version"
}
```

## AWS とデータの記録

`aws-record.json` に `provider: "AWS"` と、取得日時、instance ID/type、AMI、リージョン/AZ、CPU model/vCPU、RAM、kernel、GNU/Linux distribution、filesystem と mount options、対象 EBS の volume type/size/IOPS/throughput を記録します。資格情報・session token は保存しません。runner の EC2 vendor 判定だけでは構成や費用の証明になりません。

変化しない専用データを同じユーザー権限で読みます。実行中の OS ルートや更新中の作業ツリーは対象にしません。ディレクトリ構成・ファイル数・サイズ分布・疎ファイル・ハードリンクの有無と準備方法も記録します。全ディレクトリ行の直接比較を要求するため、別々の親ディレクトリにまたがるハードリンクを含むデータは、走査順による帰属差で不採用になることがあります。この場合も補正して合格にはしません。

runner は計測前後に metadata fingerprint を確認します。この読み取りと初回照合でキャッシュが温まるため、結果は warm 条件です。データが RAM に収まる保証はなく、cold-cache 性能とは呼びません。名前に改行や非 UTF-8 byte があるデータはこのテキスト比較プロトコルの対象外です。

## pilot と本計測

以下のパスは例です。実在する転送先・データ先へ置き換えます。

```bash
python3 scripts/bench/du_same_conditions.py \
  --binary /bench/bin/portable/hyperdu \
  --root /bench-data/representative --scope directory \
  --build-record /bench/records/portable-build.json \
  --source-snapshot /bench/records/source-snapshot.json \
  --expected-head VERIFIED_SOURCE_HEAD \
  --aws-record /bench/records/aws-record.json \
  --du /usr/bin/du --du-record /bench/records/du-record.json \
  --phase pilot --runs 4 --output /bench/results/portable-pilot.json
```

portable/native、スレッド数、`--io-profile balanced|throughput`、`--prefetch auto|true|false`、`--dir-yield-every`、`--fs-auto|--no-fs-auto` を候補にできます。近似・論理サイズ・異なる出力粒度は候補にしません。pilot の全候補と不合格結果を残し、正確性を満たす中で最速の設定を選びます。

採用設定を固定し、別の新しい output で `--phase measurement --runs 8` として実行します。毎回の全ディレクトリ行が GNU du と完全一致し、前後のデータと executable が不変で、`complete: true` の結果だけを採用します。8組の先行順を交互に変え、各ツール8試行の全値と中央値を保存します。固定した実効環境も JSON に残ります。timeout・不一致・警告は `complete: false` で保存し、成功サンプルとして再利用しません。

Linux は filesystem root と通常ディレクトリで同じ列挙エンジンです。両方を測る場合、専用 filesystem の mount root に `--scope filesystem`、代表ディレクトリに `--scope directory` を指定します。1種類だけを掲載する場合は、その範囲を明示します。

README には取得日、実測構成、同じ表示バイト数、中央値、全試行へのリンク、`du / HyperDU` 比を記載します。pilot 値や過去の WSL 値を本計測と混ぜず、遅い結果も隠しません。CI、Windows MFT、GUI、別機種の速度をこの結果から推定しません。
