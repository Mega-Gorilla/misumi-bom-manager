# misumi-cli

MISUMI の型番→単価・出荷日ルックアップを **CLI から実行**するためのツール。
ローカルでの自動/手動テスト・回帰確認・MISUMI 仕様変更の検知に使う。

> ⚠️ **前提**：実 Edge/Chromium が **Akamai を通過できる環境**が必要（既定は headed 起動）。
> ヘッドレスや CI・制限環境では Akamai に弾かれてデータ取得に失敗し得る。
> ただし CLI は**必ず全体デッドラインで終了**し、失敗時も `{ok:false,error}` を出して `exit 1` する（ハングしない）。

## 単一ソース

ルックアップ本体（suggest → price/delivery の fetch 連鎖）は
[`../../shared/misumi-lookup.js`](../../shared/misumi-lookup.js) に **一本化**されており、
**Tauri アプリ本体（`src-tauri/src/lib.rs` が `include_str!` で埋め込み）と本 CLI が同じコードを使う**。
→ MISUMI 側の仕様変更（例: 今回の CORS/credentials 修正）は共有コア 1 か所の修正で両方に反映される。

## なぜブラウザ経由なのか

`jp.misumi-ec.com` は Akamai Bot Manager で保護されており、素の HTTP クライアントでは 403。
実ブラウザ（Edge/Chromium）で Akamai JS を実行して初めて API を叩ける。本 CLI は Playwright で実ブラウザを起動し、
`shared/misumi-lookup.js` をページに注入して `window.MisumiCore` を呼ぶ。詳細は [`../../docs/misumi-api/05-akamai-and-auth.md`](../../docs/misumi-api/05-akamai-and-auth.md)。

## セットアップ

前提: Node.js（v22 系）、Microsoft Edge。

```bash
cd tools/misumi-cli
npm install
```

## 使い方

```bash
# 単一型番（stdout に JSON、失敗時 exit 1）
node cli.mjs lookup CBT3-8

# 複数型番をバッチで（1 リクエストにまとめて取得）
node cli.mjs batch CBT3-8 CBTB5-12 E-GBSCB4-20

# 人間向けサマリを stderr に併記（stdout は JSON のまま）
node cli.mjs lookup CBT3-8 --pretty

# 全体デッドライン（秒）を指定
node cli.mjs lookup CBT3-8 --timeout=45
```

### 終了保証（ハングしない）

`page.evaluate` には標準タイムアウトが無く、Akamai が fetch を保留すると無限待ちになり得る。
これを防ぐため、各実行は **(1) Node 側の全体デッドライン**・**(2) ページ内呼び出しと JS タイマーの `Promise.race`**・
**(3) `browser.close()` も詰まった場合の強制 `process.exit`** の三重で必ず終了する。
タイムアウト時は `{ "ok": false, "error": "timeout after <ms>ms" }` を出力して `exit 1`。

出力（`lookup` の主要フィールド）:
- `ok` … 成功可否（型番不一致などは `false` + `error`）
- `suggest.brandCode` … 解決したブランド（例 `MSM1`）
- `price.detailList[0].salesPrice.salesUnitPrice` … 単価（税別）
- `price.detailList[0].salesPrice.salesUnitPriceIncludingTax` … 単価（税込）
- `price.detailList[0].leadTime.vsd` … 出荷予定日

### 環境変数

| 変数 | 既定 | 説明 |
|---|---|---|
| `HEADLESS` | `0`（headed） | `1` でヘッドレス起動（Akamai に弾かれる可能性あり・実験的） |
| `TIMEOUT_MS` | `60000` | 全体デッドライン（ミリ秒）。`--timeout=SEC` でも指定可 |

## アサート例（実 Edge が Akamai を通過できる環境でのみ成功）

```bash
# 単価が取れることをアサート（取れない環境ではこの行が失敗する＝検知できる）
node tools/misumi-cli/cli.mjs lookup CBT3-8 | jq -e '.ok and (.price.detailList[0].salesPrice.salesUnitPrice|tonumber > 0)'
```

## 注意

非公開の内部 API を対象とする。MISUMI の利用規約を順守し、過度な連続・大量アクセスを行わないこと。
仕様変更で壊れた場合は、まず `shared/misumi-lookup.js` と
[`../../docs/misumi-api/02-api-flow.md`](../../docs/misumi-api/02-api-flow.md) の連鎖を実ブラウザで再確認する。
