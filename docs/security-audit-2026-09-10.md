# セキュリティ監査報告書（2026-09-10）

## 修正状況（2026-09-10）

下記の初回監査で確認したP2 5件に対する修正を実装しました。対象は
`codex/site-bench-refresh` の `c535dd8b34e877ac93170ab941dccf020f6b3c2d` に対する
未コミット差分です。ローカル検証の結果と、初回監査時点の記録を区別して記載します。
リモートCI・実リリース・本番受け入れは未実行です。

| 指摘 | 修正と検証 |
|---|---|
| P2-1 MCPキャンセル | ブロッキング処理をプロセス共通で最大2件に制限。permitを実ワーカーが終了まで保持し、リクエストの破棄でも枠を先に返しません。キャンセル時は協調フラグを立て、最大250ms終了待ち。入場待ちもキャンセル可能です。MCP単体17件、protocol 4件、SDK 3件PASS。 |
| P2-2 CSV数式 | CSVパス列の危険な数式・制御文字・空白付きprefixへ単一引用符を付与。JSONの元パスは保持。旧writerで回帰テストが失敗し、修正後3件PASS。 |
| P2-3 Actions権限 | 全Actionをupstreamで確認した完全SHAへ固定し、既定権限をreadへ変更。ビルド、release書き込み、Scoop／wingetのPAT利用を別jobへ分離。書き込みjobは同じrunの成果物をデータとして処理し、実行しません。actionlint 1.7.7と権限境界テストPASS。既存のdraft作成後の公開動作を維持。 |
| P2-4 AppImage工具 | mutable自動ダウンロードとPATH fallbackを撤去。明示したローカル工具2個と独立確認済みSHA256が必須。privateコピーのハッシュ照合後に実行権限を付与。未設定・不一致なら工具を実行せず失敗。直接appimage.shを呼ぶ経路も保護。 |
| P2-5 Plugin導入 | POSIX／PowerShellのremote cargo installを、実在確認済みの監査対象revision c535dd8へ固定。ローカルcheckout導入は従来どおり。導入引数のモック検証PASS。 |

MCPでは停止できないOS呼び出しを強制終了しません。250msを過ぎても最大2件の枠は
実処理の終了まで保持されます。これはキャンセルを繰り返して重い処理が際限なく重なる問題の
対策であり、大量リクエストの負荷試験や全処理の即時停止を保証するものではありません。
permitを早期返却する変異ではライフサイクルテスト4件が失敗し、復元後はPASSしました。

AppImageを生成する場合は、レビューした工具を事前に配置し、以下4変数を指定します。
ハッシュを指定ファイルからその場で自動採用して検証の代わりにしないでください。

```sh
export LINUXDEPLOY=/absolute/path/to/reviewed-linuxdeploy.AppImage
export LINUXDEPLOY_SHA256='<independently verified 64-digit SHA256>'
export APPIMAGETOOL=/absolute/path/to/reviewed-appimagetool.AppImage
export APPIMAGETOOL_SHA256='<independently verified 64-digit SHA256>'
bash scripts/package/appimage.sh
```

release workflowの任意AppImage工程は、工具を供給していないため生成をスキップします。
実工具での生成と配布資産は未検証です。Pluginの固定revisionは監査時点のソースであり、
今回の未コミット修正をremote導入できることを意味しません。次のPlugin更新でレビュー済みの
修正コミットへ固定先を更新する必要があります。

### 依存更新と残る通知

`wayland-scanner 0.31.7 → 0.31.11` により `quick-xml 0.37.5 → 0.41.0` へ更新し、
RUSTSEC-2026-0194／0195と適用対象のなくなった例外を削除しました。
`cargo deny check` はPASS、Linux GUIで更新した依存のコンパイルもPASSしました。
依存manifestの変更はなく、lockfileの2パッケージだけを更新しています。

`ttf-parser 0.25.1` のRUSTSEC-2026-0192は、修正版のない保守終了通知として残っています。
`deny.toml` にegui → ab_glyph側の経路と理由を明記しました。GUI依存スタックの移行は未実施です。

### 修正後のローカル検証

| コマンド／検証 | 結果 |
|---|---|
| `cargo +stable check --locked --workspace` | PASS |
| `cargo +stable test --locked --workspace` | PASS |
| 同test + `--features hyperdu-core/rayon-par,hyperdu-core/rayon-inner,hyperdu-core/simd-prefetch` | PASS |
| `cargo +stable clippy --locked --workspace --all-targets -- -D warnings` | PASS |
| `cargo +stable fmt --all -- --check` | PASS |
| `cargo +nightly fmt --all -- --check --config group_imports=StdExternalCrate --config imports_granularity=Crate` | PASS（既存strict.rsのimport順も修正） |
| `cargo +stable build --locked --release --workspace` | PASS |
| prof-tracy／prof-puffinを個別指定したworkspace check | 両方PASS（all-featuresは使用せず） |
| Linux Ubuntu 26.04 `cargo check --locked -p hyperdu-gui` | PASS |
| `cargo deny check` | PASS（ttf-parserの保守終了例外は残る） |
| `python3 scripts/package/test_supply_chain.py` | 6 PASS（旧実装では4件失敗） |
| `pwsh -File scripts/package/test_setup_hyperdu.ps1` | PASS（ローカル／remote引数モック） |
| actionlint 1.7.7、ShellCheck、shell／PowerShell構文、Python Ruff | PASS |

Rustと供給網の独立レビューでは、現行実装に追加のP0／P1／P2脆弱性は確認されませんでした。
供給網の回帰テストに対する指摘を反映し、両起動経路と検証ヘルパーでのダウンロード再導入を検査しています。

以降は初回監査の記録です。行番号、依存例外、未対策の挙動は初回監査SHAを指します。

## 初回監査の判定

要修正です。P0 / P1 は検出しませんでしたが、P2 を5件確認しました。今回のPRで新たに
入った問題は、MCPリクエストをキャンセルした後もブロッキング走査を待たずに応答を返す
点です。残り4件はmerge-baseにも存在する既存リスクです。

監査対象は `codex/site-bench-refresh` の
`5ba9e34f753f98c647cba30b4b7a61df0a23e327`、比較対象は `main` とのmerge-base
`eac21de11043acc9ecd9f6cab8b2d659ace9febd` です。作業ツリーがクリーンであることを監査の
前後に確認しました。この監査では製品コードを修正していません。

## 指摘

### P2-1: キャンセルしたMCP走査がバックグラウンドに残り得る（今回差分）

`hyperdu-cli/src/mcp/progress.rs:75-84` は走査を `spawn_blocking` で開始しますが、MCPの
キャンセルを受けると `JoinHandle` を待たずにエラーを返します。`CancelOnDrop` は協調的な
キャンセルフラグを立てるだけなので、ブロッキングタスクは次のキャンセル確認点まで継続します。
`hyperdu-cli/src/mcp.rs:58-62` では `max_depth = 0` が無制限走査を意味します。大きなボリュームに
対してキャンセルを繰り返すと、走査タスクと内部ワーカーが一時的に重なり、CPU、I/O、メモリを
消費できます。

対策は、MCP走査に同時実行数の上限を設け、キャンセル時もブロッキングタスクの終了を待つか、
終了待ちに明示的な上限を設けることです。既存テストはキャンセル後に接続を再利用できることを
確認していますが、ワーカーの終了を確認していません。

### P2-2: CSVのパス列を表計算ソフトが数式として解釈できる（既存挙動）

`hyperdu-core/src/report.rs:39-48` はパスをCSVセルへそのまま書き込みます。相対ルート名の先頭が
`=`、`+`、`-`、`@` の場合、Excelなどで数式として評価される可能性があります。新しい共有writerは
今回差分ですが、同じ未対策のCSV出力はmerge-baseのCLIにもありました。

次のローカル再現で、先頭セルが無加工で出力されることを確認しました。

```text
cargo run --locked --manifest-path <repo>/Cargo.toml -p hyperdu -- '=2+2' --top 20 --csv report.csv

path,logical,physical,files
=2+2,8,8,1
```

CSV用途に限定して危険な先頭文字を単一引用符などで無害化し、その仕様をテストしてください。
JSONと画面表示のパスは変更する必要がありません。

### P2-3: 書き込み権限を持つworkflowが可変タグのActionを実行する（既存）

`.github/workflows/release.yml:12-14` はworkflow全体へ `contents: write` と
`pull-requests: write` を与えています。同じjobは `snapcore/action-build@v1`、
`bilelmoussaoui/flatpak-github-actions/flatpak-builder@v6`、
`softprops/action-gh-release@v2` など、完全なcommit SHAではない参照を実行します。Windows jobは
後段でScoopとwinget用の長期トークンも使用します。タグが移動または侵害された場合、リリース資産、
リポジトリ、後段の秘密情報へ影響するコードがrunner上で動き得ます。

すべてのActionを検証済みの完全なcommit SHAへ固定し、既定権限を `contents: read` に下げて、
release作成と外部PR作成を必要最小限の別jobへ分離してください。GitHubも、完全なSHAだけがActionを
不変のリリースとして扱う方法だと説明しています。

### P2-4: AppImage工具を整合性確認なしで取得して実行する（既存）

`scripts/package/release.sh:129-141` は `linuxdeploy` と `appimagetool` を `continuous` URLから
取得し、チェックサムや署名を確認せず実行可能にします。`curl` に `--fail` がなく失敗を
`|| true` で無視するため、取得失敗も明確に止まりません。この経路は既定のrelease targetには
含まれませんが、`linux-all`、`all`、`linux-appimage` を指定すると実行されます。

工具のversionとSHA-256を固定し、`curl --fail --show-error` で一時ファイルへ取得し、検証後にだけ
実行可能名へ移動してください。検証失敗時はパッケージ処理を停止する必要があります。

### P2-5: Pluginのリモート導入が既定branchを追跡する（既存）

`plugin/skills/disk-space-triage/scripts/setup-hyperdu.sh:31` と
`setup-hyperdu.ps1:34` は、checkout外で実行すると `cargo install --git` にrevisionやtagを指定
しません。Plugin `0.5.0-beta.3` をレビューして導入しても、実行時点の既定branchから別のコードを
buildしてしまい、監査済みPluginと導入されるbinaryの対応が失われます。`--locked` は依存lockを
使う指定であり、Git revisionの固定ではありません。

Plugin versionに対応するtagまたはcommitを `--tag` / `--rev` で固定してください。

## 依存関係と構成の観測結果

通常の `cargo deny check` は advisories / bans / licenses / sources をすべてPASSしました。ただし
`deny.toml:22-24` が次を明示的に除外しているためです。

- `quick-xml 0.37.5`: [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194.html)
  と [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195.html)。除外なしではFAIL。
  現在の依存経路は `wayland-scanner 0.31.7` のproc-macro配下で、製品が非信頼XMLをruntimeで
  解析する経路は確認しませんでした。`>= 0.41.0` で修正済みです。
- `ttf-parser 0.25.1`: [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)。
  既知の脆弱性ではなくunmaintained通知で、修正版はありません。

現在の到達経路から即時のruntime脆弱性とは判定しません。ただしadvisory ID単位の例外は将来別の
runtime依存が同じcrateを導入しても検査を通すため、依存更新時に経路を再確認する必要があります。

AgentShield 1.5.0はGrade A / 100、critical / high / medium / lowはいずれも0でした。走査対象は
`plugin/mcp.json` 1ファイルだけで、MCP server descriptionがないinfo 1件を報告しました。この点数を
リポジトリ全体のセキュリティ合格とは扱いません。

追跡ファイルとGit履歴に対する値を伏せた一般的な秘密情報pattern検査は0件でした。専用toolの
Gitleaksは環境になく、Semgrep、cargo-audit、zizmorも未実行です。

## 実行した検証

| 検証 | 結果 |
|---|---|
| `cargo deny check` | PASS（構成済み例外あり） |
| 一時的に例外を除いた `cargo deny check advisories` | EXPECTED FAIL（上記3 advisory） |
| MCP protocol / SDK / option境界 | 12 PASS |
| core parser単体テスト | 217 PASS / 3 ignored（3件とも明示的な性能ベンチマーク） |
| symlink / junction / cycle / cancellation / MFT統合テスト | 37 PASS |
| CLI進捗thread終了テスト | 1 PASS |
| AWS比較harnessの入力・provenance・timeout・fail-closedテスト | 16 PASS |
| AgentShield | A / 100、info 1件、走査1ファイル |
| 追跡ファイル・履歴の秘密情報pattern検査 | 0件 |
| 独立した全PR静的レビュー | P0 / P1なし、P2-1とP2-2を確認 |

MCP検証ではliteral option terminator、handshake、request token付きprogress、無効引数後の回復、
並行requestの分離、キャンセル後の接続再利用を確認しました。core検証では破損MFT、fixup、runlist、
境界、cycle、junction、hardlink、sparse file、キャンセルの挙動を確認しました。

## 未実行・保証しない範囲

- libFuzzer / AFL、Miri、AddressSanitizerなどによる長時間のmemory-safety検証
- Gitleaks、Semgrep、cargo-audit、zizmorによる専用scan
- Linux / macOSでの今回HEADの全platform runtime検証
- GitHub Actionsのremote CI、実際のrelease、公開資産のprovenance検証
- 悪意あるMCP clientから大量キャンセルを繰り返す負荷試験
- 本番環境、AWS、利用者環境での受け入れ

## 修正順序

1. MCP走査の同時実行上限とキャンセル後の終了待ちを追加する。
2. CSVのパス列を式として解釈されない形式へする。
3. release workflowのActionを完全なSHAへ固定し、権限と秘密情報をjob単位で分離する。
4. AppImage工具とPluginのGit sourceを固定し、整合性を検証する。
5. GUI依存を更新し、`deny.toml` のadvisory例外を縮小する。

GitHub Actionsの供給網対策は
[GitHub Secure use reference](https://docs.github.com/en/actions/reference/security/secure-use)を基準に
しています。
