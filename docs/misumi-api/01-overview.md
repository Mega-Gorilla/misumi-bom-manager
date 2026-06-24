# 01. 全体像・前提

## このページは何をしているか

対象ページ `https://jp.misumi-ec.com/order/part-number/create`（「見積・注文 → 型番手入力」）は、
ユーザーが **型番** と **数量** を入力すると、その場で **単価** と **出荷日** を表示する SPA（Single Page Application）。

- 1 行ごとに「型番（`partNumber`）」「メーカー名（`brandName`）」「数量」を入力する複数行フォーム
- Excel からの貼り付け（TAB 区切り textarea）、CSV/ファイルアップロードにも対応
- 入力した型番はサジェスト API でリアルタイムに正規化され、確定すると価格・出荷日が計算される

## 前提・特徴

| 項目 | 内容 |
|---|---|
| 公式 API か | **No**。EC フロントエンドが使う内部 API（非公開・無契約） |
| 認証 | 標準カタログ価格・標準出荷日は **未ログインで取得可能**。顧客別契約価格・与信・専用納期はログイン後に変わり得る |
| Bot 対策 | **Akamai Bot Manager** が全リクエストをガード（詳細は [05](./05-akamai-and-auth.md)） |
| フロント技術 | Next.js（`X-Powered-By: Next.js`）/ ホスティングは Vercel / CDN は Akamai |
| レスポンス形式 | すべて JSON |
| 通貨 | `JPY`（レスポンスに `currencyCode` / `ccyCode` で明示） |

## 関係するホスト

| ホスト | 役割 |
|---|---|
| `jp.misumi-ec.com` | EC フロント本体。`/api/v1/*` 系の商品・サジェスト・カテゴリ API を提供 |
| `api-jp.misumi-ec.com` | **価格・納期計算 API ゲートウェイ**（`price-delivery-calculation/*`） |
| `content.misumi-ec.com` | 商品画像 CDN（`//content.misumi-ec.com/image/upload/...`） |
| `s3.ap-northeast-1.amazonaws.com/jp.misumi-ec.com/...` | お知らせ・ワンポイント解説などの静的 JSON |

## 共通パラメータ

- `jp.misumi-ec.com/api/v1/*` 系は、固定の **`applicationId=de30e2b2-db86-435d-9929-646c11a3c4cd`** をクエリに付与する。
  - これは EC フロント用に埋め込まれた公開アプリ識別子（ユーザー固有のキーではない）。
- 言語指定は `lang=JPN`。
- 価格 API（`api-jp.misumi-ec.com`）は `applicationId` を**使わず**、Akamai Cookie とリクエストボディのみで動作する（観測時点）。

## 用語

| 用語 | 意味 |
|---|---|
| `partNumber` / `inputProductCode` | 型番（ユーザー入力値）。例 `CBT3-8` |
| `brandCode` | ブランドコード。ミスミ自社品は `MSM1` |
| `seriesCode` | 商品シリーズコード。例 `110302678660` |
| `innerCode` / `ginnerCode` | 社内商品マスタコード。例 `MDM00001313262` |
| `vsd` | 出荷予定日（Vendor Ship Date と推定） |
| `crd` | 着荷／客先納入関連日（Customer Requested/Receive Date と推定。要検証） |

> 関連: 型番→価格の流れは [02-api-flow.md](./02-api-flow.md)、核心 API は [03-price-delivery-check.md](./03-price-delivery-check.md)。
