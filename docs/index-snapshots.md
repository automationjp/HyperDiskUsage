# Linux directory snapshots (experimental)

**日本語** · [English](en/index-snapshots.md) · [简体中文](zh-CN/index-snapshots.md)

HyperDU の `index` サブコマンドは、**一度走査した directory 集計を保存し、次回はファイルツリーを再走査せずに読み出す**ための Linux 向け実験機能です。

> これは watcher ではありません。自動更新や常駐監視は行いません。保存値は常に `stale` として扱います。

## Quick start

```sh
mkdir -p "$HOME/.cache/hyperdu"
hyperdu index refresh /srv/data --database "$HOME/.cache/hyperdu/data.idx"
hyperdu index show /srv/data --database "$HOME/.cache/hyperdu/data.idx"
```

- `refresh`: 通常 scanner で tree を走査し、directory 単位の snapshot を保存する
- `show`: 保存済み snapshot を読み、対象 tree を再走査せずに結果を返す

## いつ使うか

向いているケース:

- 非常にファイル数が多い tree の概算を繰り返し確認したい
- 「最後に明示的に更新した時点の値」でよい
- 自動監視 daemon を入れたくない

向いていないケース:

- 常に最新値が必要
- filesystem の atomic snapshot が必要
- ファイル変更をリアルタイム追跡したい

## `stale` の意味

`show` は保存済みデータだけを読みます。`refresh` 後に作成・変更・削除されたファイルは反映されません。

また、`refresh` 自体も filesystem の atomic snapshot ではありません。走査中に tree が変更される可能性があります。

そのため出力では次を明示します。

```text
freshness: "stale"
monitoring: false
```

`stale` はエラーではなく、**「現在の filesystem と完全一致することは保証しない保存値」**という意味です。

## Database rules

安全に保存・再利用するため、database には次の制約があります。

- 親 directory は事前に存在している必要がある
- database は通常ファイルである必要がある
- symlink は拒否する
- database は scan root の外側に置く
- tree ごとに別 database を使う

この制約により `/` 自体は snapshot root として使用できません。

## Root identity

保存時と読み込み時で、mount root の device / inode identity が一致することを確認します。

remount、restore、root replacement などで identity が変わった場合は `refresh` し直してください。

inode は再利用される可能性があるため、これは freshness 保証ではなく consistency check です。

## Failure behavior

`refresh` が完全に成功しない場合、既存 snapshot を不完全な値で置き換えないことを優先します。

例:

- cancellation
- scan error
- 安全に表現できない bind-mount alias
- root identity を確定できない場合

## Cost model

`show` の読み込みコストは **O(indexed directories)** です。全ファイルを再走査しません。

読み込み後の root lookup は O(1) です。

## CLI note

`index` はサブコマンド名です。`index` という実 directory を通常 scan したい場合は明示します。

```sh
hyperdu ./index
# または
hyperdu -- index
```

通常 scan の option は、固定 semantics を持つ `index refresh` / `index show` にそのまま適用されません。

## What is not implemented

現在は次を含みません。

- inotify watcher
- background daemon
- automatic refresh
- overflow recovery
- watch budget policy

これらは将来検討の別機能です。過去の persistent-index / watcher 設計は [old documentation](old/README.md) に保存しています。

## Related docs

- [Architecture](architecture.md)
- [Performance design](performance.md)
- [Historical design records](old/README.md)
