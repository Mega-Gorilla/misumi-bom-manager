# 06. 本アプリでの実装方針・注意事項

技術スタックは **Tauri 2 + React + TypeScript（Rust バックエンド）**。
[05](./05-akamai-and-auth.md) の制約（Akamai + CORS）を踏まえた実装方針。

## 方針比較

| 方針 | 概要 | Akamai 突破 | 配布容易性 | 安定性 | 推奨度 |
|---|---|---|---|---|---|
| **A. 内蔵 WebView 経由** | Tauri の WebView2 で `jp.misumi-ec.com` を読み込み、同一オリジンから `fetch` | ◎（実ブラウザ） | ◎ | ○ | **本命** |
| B. Playwright サイドカー | ヘッドレス/ヘッドフル Edge を同梱して操作 | ◎ | △（重い） | ○ | 予備 |
| C. 素の HTTP（Rust reqwest） | Cookie を自前管理して直接叩く | ✕（`_abck` 生成不可） | ◎ | ✕ | 非推奨 |
| D. 公式 eProcurement / API 連携 | MISUMI と法人契約（cXML/PunchOut, EDI） | 該当なし | △ | ◎ | 商用・正式運用時 |

## 方針 A（本命）の実装イメージ

> 「`jp.misumi-ec.com` オリジン上で動く WebView から fetch する」のが肝。

### パターン A-1：メイン WebView を MISUMI に置く
- アプリ起動時、Tauri のメインウィンドウ（または非表示の子 WebView）で `jp.misumi-ec.com` 上の任意ページを読み込む。
- そのページコンテキストで `fetch` を実行（Akamai Cookie が自動付与・CORS も通る）。
- 取得した JSON を Tauri の IPC でフロント（自前 React UI）へ渡す。

### パターン A-2：非表示 WebView をブリッジに使う（推奨）
1. 自前 UI（`tauri://localhost`）は通常どおり表示。
2. バックグラウンドに `jp.misumi-ec.com` を読み込んだ非表示 WebView を 1 枚持つ。
3. 価格取得時、その WebView に対して以下を `eval` 実行：
   ```js
   // jp.misumi-ec.com オリジン上で実行されるコード
   async function lookup(parts) {
     const res = await fetch(
       "https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check",
       {
         method: "POST",
         headers: { "Content-Type": "application/json" },
         // ⚠ credentials は付けない（下記「CORS の落とし穴」参照）
         body: JSON.stringify({ detailList: parts }) // [{qty, inputProductCode, brandCode}]
       }
     );
     return await res.json();
   }
   ```
4. 結果を IPC でメイン UI に返す。

> 🛑 **CORS の落とし穴（実装時に必ず踏む）**
> api-jp への POST に **`credentials: "include"` を付けると `TypeError: Failed to fetch` で失敗する**。
> api-jp は `Access-Control-Allow-Origin: *` を返すため、資格情報付きリクエストはブラウザの CORS チェックで拒否される。
> **資格情報なし（既定 / omit）で呼ぶ**こと。未ログインの標準価格に Cookie は不要。
> （`suggest` は同一オリジンなので credentials は無関係。）詳細は [05 §CORS](./05-akamai-and-auth.md)。

> 🧩 **実装の単一ソース**：この fetch 連鎖は [`shared/misumi-lookup.js`](../../../shared/misumi-lookup.js) に一本化され、
> Tauri 本体（`include_str!`）と CLI（`tools/misumi-cli`）が共用する。仕様変更時はそこ 1 か所を直す。テストは [08](./08-testing.md)。

> `brandCode` が不明な型番は、先に
> `GET https://jp.misumi-ec.com/api/v1/partNumber/suggest?applicationId=...&keyword=<型番>`
> を同じ WebView から呼んで解決する（[04](./04-supporting-endpoints.md)）。

### ログインが必要な場合
- 顧客別価格が要るなら、その MISUMI WebView 上でユーザーにログインしてもらう（OAuth）。
- ログインセッション（Cookie）は同一 WebView コンテキストに保持される。

## BOM 一括対応

- `sales-price-delivery/check` の `detailList` は**配列**なので、複数型番を 1 リクエストで取得できる。
- ただし**過度な大量・高頻度アクセスは避ける**（[ToS / レート制限](#tos--レート制限)）。
  - 例：1 リクエストあたりの明細数を常識的な範囲に抑える、行間にウェイトを入れる、結果をキャッシュする。

## キャッシュ方針（推奨）

- 単価・出荷日は変動するため、**短時間キャッシュ**（例：同一型番・数量を数分〜当日）に留める。
- 型番→`brandCode`/`seriesCode` の正規化結果は比較的安定なので、長めにキャッシュ可。
- BOM はローカル DB（SQLite 等）に保持し、価格・納期は「最終取得日時」付きで保存する設計が無難。

## エラーハンドリング

- レスポンスの `errorMessageList` / `warningMessageList` を必ず確認する。
- 型番不一致・廃番・要見積品はメッセージで返る想定（[03](./03-price-delivery-check.md) 末尾、要追加検証）。
- 価格・数量は文字列で返るため数値変換する。

## ToS / レート制限

- 本 API は**非公開の内部 API**。利用は MISUMI の**利用規約**に従うこと。
- 自分（自社）が MISUMI の顧客として、自分の発注・見積のために使う範囲に留めるのが穏当。
- **大量スクレイピング・転売・再配布は避ける。** 礼儀的なレート制限（同時実行を絞る／間隔を空ける）を実装する。
- 商用・大規模・基幹連携を行うなら、MISUMI 営業窓口に **eProcurement / API 連携（法人契約）** を相談すること（cXML/PunchOut, EDI による正式連携が用意されている）。

## 仕様変更への備え

- 内部 API は予告なく変わり得る。**エンドポイント URL・`applicationId`・フィールド名を設定値として外出し**し、壊れた時に追従しやすくしておく。
- 定期的に [02](./02-api-flow.md) の連鎖を実ブラウザで再確認する運用を推奨。

> 関連メモリ: `misumi-price-delivery-api`（プロジェクトメモリに同内容を記録済み）。
