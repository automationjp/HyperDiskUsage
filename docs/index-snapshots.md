# Linux directory snapshots (experimental)

HyperDU の Linux 向け `index` サブコマンドは、ディレクトリ単位の走査結果を明示的に保存し、次回はファイルツリーを再走査せずに集計値を読み出すための実験的機能です。

> この機能は watcher ではありません。自動更新や常駐監視は行わず、保存値は常に `stale` として扱います。

## Quick start

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

`refresh` は既存の physical-size scanner を実行し、ディレクトリ単位の snapshot をアトミックに置き換えます。`show` は保存済み snapshot を読み込み、対象ツリーを再走査せずに結果を返します。

両コマンドの JSON 出力には、少なくとも次の情報が含まれます。

- `physical_bytes`
- `files`
- `directory_count`
- `freshness: "stale"`
- `monitoring: false`

## Freshness semantics

snapshot はファイルシステムの atomic snapshot ではありません。`refresh` 完了直後であっても、走査中に対象ツリーが変更される可能性があります。

また、`show` は保存済みデータだけを読みます。新しいファイルや削除されたファイルは、次に `refresh` するまで反映されません。そのため HyperDU は snapshot を常に `stale` と明示します。

## Database constraints

- database の親ディレクトリは事前に存在している必要があります。
- database は通常ファイルでなければならず、symlink は拒否します。
- database は scan root の外側に置く必要があります。
- この制約により `/` は snapshot root として使用できません。
- 1つの database を複数 tree で共有せず、tree ごとに別ファイルを使用してください。

## Root identity

保存時と読み込み時で、mount された root の device/inode identity が一致している必要があります。

remount、restore、root replacement などで identity が変わった場合は、明示的に `refresh` して snapshot を再構築してください。

inode は再利用される可能性があるため、この identity check は consistency check であり freshness guarantee ではありません。

## Failure semantics

次の場合、既存 snapshot を不完全な結果で置き換えません。

- cancellation
- scan error
- 表現できない bind-mount alias
- root identity を安全に確定できない場合

不完全な結果を「成功した snapshot」として保存しないことを優先しています。

## Cost model

`show` の読み込みコストは **O(indexed directories)** です。ファイル数そのものには比例しません。読み込み後の root lookup は O(1) です。

このため、非常に多数のファイルを含み、ディレクトリ数が相対的に少ない tree では、毎回のフルスキャンより大幅に軽い問い合わせが可能です。

## CLI parsing note

`index` は予約されたサブコマンドです。`index` という名前のディレクトリを通常スキャンしたい場合は、次のように明示してください。

```sh
hyperdu ./index
# または
hyperdu -- index
```

通常スキャン用オプションは、固定 semantics を持つ `index refresh` / `index show` には適用されません。

## Relationship to watchers

現在の snapshot 機能には、次のものは含まれません。

- inotify watcher
- background daemon
- automatic refresh
- overflow recovery
- watch budget policy

これらは別の受入条件として扱っています。実装境界は [Issue #16 / #41 implementation notes](design/issue-16-41-implementation.md) を参照してください。

## Windows MFT note

snapshot 実装とは別に、Windows の MFT scan では ordinary-file DATA extension の stream name と validated record reference を使用します。必須 DATA extent を解決できない場合は directory enumeration に fallback します。

named stream（WOF を含む）は診断情報として扱い、physical usage に無条件で加算しません。live-volume parity は別の acceptance gate です。詳細は [MFT parity verification](design/mft-parity-verification.md) と [Issue #16 / #41 implementation notes](design/issue-16-41-implementation.md) を参照してください。
