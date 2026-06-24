# misumi-api-probe

MISUMI の内部「単価・出荷日チェック」API を検証するための再現用ハーネス。
仕様の解説は [`../../docs/misumi-api/`](../../docs/misumi-api/) を参照。

## なぜ Node + Playwright なのか（Python venv ではない理由）

`jp.misumi-ec.com` は **Akamai Bot Manager** で保護されている。
素の HTTP クライアント（`curl` / Python `requests` / Node `fetch`）は、
Akamai の難読化 JS を実行できず有効な `_abck` Cookie を生成できないため **403 でブロックされる**。

→ **実ブラウザ（Edge）を起動して Akamai JS を実行 → そのセッション Cookie を引き継いだ
HTTP クライアントで API を叩く**、というブラウザ経由が必須。本ハーネスは Playwright でそれを行う。
（このため Python 仮想環境では本 API のライブ検証はできない。）

## セットアップ

前提：Node.js（プロジェクトと同じ v22 系）、Microsoft Edge がインストール済み。

```bash
cd tools/misumi-api-probe
npm install
```

`channel: 'msedge'` を使うため、通常は追加のブラウザダウンロードは不要
（Edge が無い場合は `npx playwright install chromium` して `probe.mjs` の channel 指定を外す）。

## 使い方

```bash
# 単一型番：suggest で brandCode 解決 → 単価・出荷日を取得
node probe.mjs lookup CBT3-8

# 複数型番を 1 リクエストでバッチ取得
node probe.mjs batch CBT3-8 CBT3-10 CBTB5-12

# バッチ件数スイープ（件数 vs レイテンシ／上限の特性把握）
node probe.mjs sweep
```

## 既知の結果（2026-06-24 実測）

- `lookup CBT3-8` → 単価(税別) 400 / 税込 440、出荷日 vsd 2026-06-26。
- バッチは `detailList` 配列で対応。**1000 件まで HTTP 200・欠落なし**。
- **2000 件で HTTP 504（upstream request timeout, 約 5 分）** → 件数ではなくレイテンシが実質上限。
- レイテンシ目安：50 件 ≈ 3.6 秒、200 件 ≈ 11 秒、1000 件 ≈ 42 秒。
- 実装は **1 リクエスト ≤ 100 件・並列 2〜3・キャッシュ併用**を推奨。

詳細は [`docs/misumi-api/07-batch-and-limits.md`](../../docs/misumi-api/07-batch-and-limits.md)。

## 注意

非公開の内部 API を対象とする。MISUMI の利用規約を順守し、過度な連続・大量アクセスを行わないこと。
仕様は予告なく変わり得るため、壊れた場合はまず `docs/misumi-api/02-api-flow.md` の連鎖を実ブラウザで再確認する。
