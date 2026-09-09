# POSIX du 互換性の確認

[日本語](posix-compatibility.md) | [English](en/posix-compatibility.md) | [简体中文](zh-CN/posix-compatibility.md)

結論: 現状のCLIはPOSIX du完全互換ではありません。

通常モードはHyperDU独自の一覧・サマリです。`--compat posix-strict` という名前は完全準拠を保証しません。

| 項目 | 確認結果 |
|---|---|
| 必須オプション `-a`, `-s`, `-H`, `-L` | 未実装。Linux releaseで `--compat posix-strict -s <path>` は終了コード2、unexpected argument。 |
| `-k`, `-x` | CLIで受け付ける。全境界ケースの適合認証ではありません。 |
| 既定出力単位 | `posix-strict` のコードは512-byte単位を選択。通常モードは独自出力。 |
| 存在しないパス | Linux releaseでstderr診断と終了コード1を確認。 |
| 完全な意味論 | ファイル引数、シンボリックリンク、ディレクトリ自身の割当量、オプションの順序などを含む完全適合は未達。 |
| 割当量の差 | 4096-byteファイル、ハードリンク、割当済みリンク2件、ディレクトリ2件のfixtureで、duのroot合計40ブロックに対しHyperDUは8ブロック。ディレクトリ・未追跡リンク自身を含めていません。 |
| シンボリックリンク | コマンド引数のリンクでerrno20。`--follow-links` ではdangling linkを黙って飛ばして終了0となり、du -Lの診断・終了1と異なります。 |
| 出力区切り | posix-strictもTAB区切り。POSIX規定の空白区切りと異なります。 |

README/サイトの動作しない `-sh` / `-ak` 例を修正し、`du` エイリアスの推奨を削除しました。完全互換の実装はこの監査では追加していません。

[POSIX.1-2017 / Open Group du specification](https://pubs.opengroup.org/onlinepubs/9699919799/utilities/du.html)

Audit: 2026-09-09 / release source `0a089c90c0e2995ef702ff053820d129265c3659` / WSL2 Ubuntu 26.04.
