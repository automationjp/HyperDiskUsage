# hyperdu-gui

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

![HyperDU GUI](../docs/images/gui.png)

`hyperdu-gui` は、共有 scanner core を使ってディスク使用量を調べる desktop GUI です。対象フォルダをパス入力またはフォルダ選択で指定し、ディレクトリの内訳を段階的に確認できます。

画面のラベル、状態表示、設定項目は日本語です。起動時に OS のフォントディレクトリから CJK・絵文字・UI・等幅フォントの fallback を探して登録するため、日本語のパスを含む画面を表示できます。native startup での日本語描画を確認済みです。

## リリース状況

`0.5.0-beta.3` は公開準備中です。crate registry からの version 指定インストールは公開後に利用できます。現在はリポジトリ root から source をインストールしてください。

## インストールと起動

リポジトリ root で実行します。

```bash
cargo install --locked --path hyperdu-gui
hyperdu-gui
```

開発 checkout から起動する場合：

```bash
cargo run --release -p hyperdu-gui
```

Linux では `eframe` / `winit` が利用する X11 または Wayland の development libraries が必要です。Windows は通常の desktop session を使用します。macOS は現在 GUI の release target に含めていません。詳しい Rust version、OS 依存、build 手順は [セットアップ](../docs/setup.md) を参照してください。

## 画面と走査の流れ

1. 「対象フォルダ」にパスを入力するか、「選択…」でフォルダを選ぶ
2. 「走査モード」を選ぶ
3. 必要なら詳細設定を変更する
4. 「スキャン開始」を押す
5. 左のディレクトリ tree と右の一覧で結果を確認する

設定変更は次回のスキャンに適用されます。走査中は設定と対象フォルダを変更できず、「中止」で cooperative cancellation を要求できます。

### Interactive と Batch

既定値は `Interactive` です。core は最初に root 直下を列挙し、直下のファイル合計と子フォルダの pending 状態を通知します。その後、子フォルダを一つずつ全 worker で走査し、完了した子フォルダから結果を UI に反映します。残りの走査中も完了済みフォルダを開いて確認できます。

`Batch` は全体の map を一度に受け取るモードです。走査途中の子フォルダ別結果を順次表示する用途ではなく、完了後に全体結果を表示します。

Windows の MFT 経路が Interactive scan で成功した場合は、通常の子フォルダ逐次結果ではなく、一括結果を受信することを UI に表示します。

## 詳細設定

| 設定 | 動作 |
|---|---|
| 名前・パスに含まれる文字列 | literal contains filter。1行に1パターンで、前後の空白と空行は除外します |
| glob | glob filter。1行に1パターンで、前後の空白と空行は除外します |
| 正規表現 | regex filter。1行に1パターンで、前後の空白と空行は除外します。不正な regex は開始時にエラーになります |
| 最小ファイルサイズ | 整数と `B / KB / KiB / MB / MiB / GB / GiB` を指定します。KB/MB/GB は 1000 倍、KiB/MiB/GiB は 1024 倍です。空白は無視され、大文字小文字は区別しません |
| 深さ上限 | `0` は無制限。1以上では元の scan root を基準に深さを制限します |
| リンク先を走査 | symlink を追跡します。追跡時は link cycle 検出を有効にします |
| ハードリンクを個別に数える | 既定は GNU `du` と同様に hardlink を重複排除し、有効にすると各 hardlink を別々に数えます |
| 同じファイルシステムのみ | scan root の filesystem 境界を越えないようにします |

### サイズ計算

- **物理 + 論理**（既定）：allocation/physical size と logical file size を集計します。
- **論理のみ**：physical size を計算せず、UI の物理欄にも logical size を代替値として表示します。
- **概算（高速）**：physical size を計算せず、regular file size を近似して metadata I/O を減らします。min size が0の regular file では現在 4096 bytes（4 KiB）を推定値として使い、directory entry は0として扱います。正確な file size を優先しないため、UI は「概算結果」と表示します。物理欄も代替値です。

論理のみ・概算では、物理欄の数値を実際の allocation size と解釈しないでください。

### 速度と I/O

| 設定 | 動作 |
|---|---|
| スレッド数 | `0` は core の default thread 数を使います。正の値は要求する worker 数です。Gentle profile では有効数が最大2に抑えられます |
| I/O 標準 | **Balanced**。正しい結果を保ち、意図的に page cache を warm しません |
| I/O 速度優先 | **Throughput**。hardware が提供できる I/O を最大限使います |
| I/O 低負荷 | **Gentle**。他の処理を妨げないよう readahead を使わず、worker 数と巨大 directory の分割も抑えます |
| 先読み 自動 | I/O profile に決めさせます |
| 先読み 有効 / 無効 | readahead を明示的に on / off にします。readahead は latency を短くする一方、読む総量を増やすため、既定では要求されない限り有効にしません |
| 巨大フォルダの分割間隔 | `N` エントリごとに大きな directory を分割して yield します。`0` は無効で、GUI では `0..=1,000,000` を指定できます |

### Windows の MFT

`NTFS MFT直接走査を試す` は Windows 専用の opt-in です。管理者権限、NTFS、volume root、全 volume を読む条件が必要です。MFT の layout を安全に完全解析できない場合、昇格されていない場合、NTFS でない場合、または対象が volume 全体でない場合は、部分結果を返さず通常の directory enumeration に fallback します。

MFT は filesystem を利用者が見る view ではなく volume 自体の view を返すため、既定では有効にしていません。MFT 成功時の Interactive UI は一括結果を表示します。

## 結果の表示

### 左の tree

開いた directory ごとに最大500件を表示し、画面全体では最大1500行まで recursive rendering します。深い directory は table から引き続き開けます。画面上の上限は scan 結果を削除する制限ではありません。

### 右の一覧

現在選択している directory の子を**全件**表示します。物理サイズ、論理サイズ、ファイル数、名前で並べ替えできます。tree の500件・1500行制限は右の一覧には適用されません。

root 直下のファイルは、directory と混同する仮想 path を作らず、`直下のファイル合計` として表示します。

各行のサイズは `物理 / 論理` です。論理のみ・概算では前述のとおり物理欄が代替値になります。

## 状態、エラー、キャンセル

- 走査中は pending の子フォルダに spinner が表示され、走査済み files、残り folders、error 数を表示します。
- 読み取りエラーは error count に加算され、詳細は最大20件まで表示します。
- error があるまま core が終了通知を送った場合は、結果を表示しますが「一部を読み取れませんでした」となり、完了結果とは扱いません。
- 「中止」は cooperative cancellation です。完了済みフォルダの部分結果は閲覧できますが、キャンセルされた走査を完了とは表示しません。
- root が存在しない、pattern/size が不正、scan thread が失敗するなど開始または走査に失敗した場合はエラー状態を表示します。

JSON と CSV の保存ボタンは、走査が `Finished` で終了し、error count が0で、現在 scan 中でない場合だけ有効です。部分結果、キャンセル結果、失敗結果は export できません。

## 通常の CLI / MCP

GUI は表示と対話操作の front-end です。script、CI、structured output、AI agent から使う場合は、通常の `hyperdu` CLI と `hyperdu mcp` を使用してください。インストールと MCP の登録は [hyperdu CLI / MCP の README](../hyperdu/README.md) を参照してください。

## ライセンス

MIT。egui の dependency chain に同梱される font には、それぞれ OFL-1.1 と Ubuntu-font-1.0 のライセンスが適用されます。
