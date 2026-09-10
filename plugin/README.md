# AI エージェント向け HyperDU

**日本語** · [English](README.en.md) · [简体中文](README.zh-CN.md)

一度のインストールでCLIとMCPサーバを利用できます。公開クレートと実行コマンドは `hyperdu`、
MCPサーバは `hyperdu mcp` です。0.5.0-beta.3で単独の `hyperdu-mcp` 実行ファイルを置き換えます。

## このチェックアウトからインストール

```sh
cargo install --locked --path hyperdu
hyperdu --help
hyperdu mcp --help
```

Rust 1.88+ が必要です。公開後は `cargo install hyperdu --version 0.5.0-beta.3` を利用できます。
セットアップスクリプトは、チェックアウトがあればそのソースから、なければGitHubからインストールします。

```sh
plugin/skills/disk-space-triage/scripts/setup-hyperdu.sh
```

```powershell
.\plugin\skills\disk-space-triage\scripts\setup-hyperdu.ps1
```

`--check` / `-Check` は導入状態を確認するだけです。`--register` / `-Register` は検出した
クライアントへ登録します。指定しない場合は登録コマンドの表示のみです。

## MCPクライアントへ登録

```sh
codex mcp add hyperdu -- hyperdu mcp
claude mcp add --transport stdio hyperdu -- hyperdu mcp
```

その他のstdioクライアントには、`mcp.json` と同様に `command: "hyperdu"`、`args: ["mcp"]` を指定します。
サーバは要求時だけ起動し、通常のCLI走査は従来どおり利用できます。

| ツール | 確認できること |
|---|---|
| `list_volumes` | 容量が不足しているボリューム |
| `scan_path` | ディレクトリ内の大きな項目 |
| `find_reclaimable` | 再生成できる可能性がある生成物 |

`unused_for_days` はサンプルした更新時刻によるフィルタです。ビルドの停止や削除の安全性は
保証しません。候補を確認し、削除前にユーザーの判断を受けてください。**削除ツールはありません。**

## 独立した利用方法

- MCPはPluginやSkillなしでも動作します。
- `skills/disk-space-triage/` はCLIを使うためMCP登録は不要です。
- `plugin.json` と `mcp.json` はエージェント用の配布設定です。
- `hyperdu-core` は再利用可能な走査・ドメイン処理のライブラリとして分離しています。
