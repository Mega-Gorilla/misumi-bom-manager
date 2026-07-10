# 09. カート投入（見積・注文カートへの一括追加）API 調査

BOM の部品（型番＋数量）を **MISUMI のカートへ一括投入**する仕組みの調査結果。
2026-07-10 に Playwright + Microsoft Edge（実ブラウザ）でネットワーク観測して判明。
「部品をカートに追加」ボタンで、ログイン済みのユーザーのカートへ BOM 明細をまとめて入れる機能の一次情報。

> ⚠️ [README.md](./README.md) の免責と同じ前提。これらは MISUMI 内部 API であり、公開 API ではない。
> ToS を尊重し、過度な連続アクセスを避けること。カート投入は**注文ではない**（カートに入るだけ・いつでも削除可能）。

---

## 結論（先に要点）

- **カート投入は単一エンドポイントで完結する**。多段の入力 UI を自動操作する必要はない。
- `POST https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/add`
  に **`{qty, brandCode, inputProductCode}` の配列**を投げれば、複数明細を一括でカートに追加できる。
- 必要な値（`brandCode`＝ミスミは `MSM1`／`inputProductCode`＝型番／`qty`＝数量）は、
  **既存の価格取得フロー（[03-price-delivery-check.md](./03-price-delivery-check.md)）で解決済みの値と同じ**。
- ただし **ログイン必須**（未ログインでは一括入力 UI も `cart-detail/add` も使えない）。
  認証は**アプリ内 WebView に一度ログイン → セッションを永続プロファイルに保持**する方式が最適（後述）。

---

## 見積・注文ページの一括入力 UI（参考：手動フロー）

`https://jp.misumi-ec.com/order/part-number/create`（見積・注文 / 型番手入力）には、
右サイドに「**まとめて一括入力**」パネルがあり、Excel からのコピー貼り付け／ファイルアップロードに対応する。
**いずれもログイン必須**（未ログインでは「先にログインしてください」表示）。

手動フローは **「次へ」が 3 段階**ある：

1. **テキスト投入 → 次へ**：貼り付けた TSV/CSV を `sales-order/v1/file/parse` で行に分解。
2. **列マッピング → 次へ**：各列に「型番【必須】/数量【必須】/メーカー名/注文番号1〜3/取り込み不要」を割り当て。
3. **プレビュー確認 → 次へ**：明細をグリッドに確定（この時点で価格チェックが走る）。
4. **カートへ追加**：`cart-detail/add` を発行。

→ **本アプリでは 1〜3 を飛ばし、4 の API を直接呼ぶのが最善**（UI 自動操作は不要・壊れにくい）。

---

## API 連鎖（観測結果）

### (1) 一括入力テキストの解析（UI 経由のときのみ / 直接呼ぶ場合は不要）

```
POST https://api-jp.misumi-ec.com/sales-order/v1/file/parse
Content-Type: application/json

{ "inputText": "CBT3-8\t2\nCBT3-10\t3\nSFJ3-10\t1",
  "headerRowType": "2",     // "1"=タイトル行あり / "2"=データ行(ヘッダなし)
  "delimiterType": "2" }    // "1"=カンマ(,) / "2"=タブ(TAB)
→ 200
{ "rowList": [ { "cellList": ["CBT3-8","2"] }, { "cellList": ["CBT3-10","3"] }, ... ] }
```

列マッピングの選択肢と内部値：

| 表示 | 内部値 |
|---|---|
| 取り込み不要 | `noUse` |
| 型番【必須】 | `productCode` |
| 数量【必須】 | `qty` |
| メーカー名 | （brandName 相当） |
| 注文番号1〜3 | （orderNo 相当） |

### (2) 価格・出荷日チェック（グリッド確定時／ログイン時は `shipToCode` 付き）

```
POST https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check
{ "shipToCode": "<お届け先コード>", "detailList": [ { "qty": 2, "inputProductCode": "CBT3-8" } ] }
```

- [03-price-delivery-check.md](./03-price-delivery-check.md) と同じエンドポイント。
- **ログイン時は `shipToCode`（顧客のお届け先）が付き、契約単価・専用納期が反映され得る**（未ログインは標準価格）。
- 明細ごとに 1 リクエストで観測されたが、`detailList` は配列なのでバッチ可能（[07-batch-and-limits.md](./07-batch-and-limits.md)）。

### (3) ★カート投入（本命）

```
POST https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/add
Content-Type: application/json

{ "cartDetailList": [
    { "qty": 2, "brandCode": "MSM1", "inputProductCode": "CBT3-8" },
    { "qty": 3, "brandCode": "MSM1", "inputProductCode": "CBT3-10" },
    { "qty": 1, "brandCode": "MSM1", "inputProductCode": "SFJ3-10" }
  ],
  "indirectSalesOrderInstrumentationFlag": "1" }
→ 200
{ "cartDetailList": [
    { "cartDetailId": <採番されたカート明細ID>,
      "qty": 2,
      "product": { "brandCode": "MSM1", "inputProductCode": "CBT3-8", "ginnerCode": "...", "weight": "2.00", "weightUnit": "g" },
      "leadTime": { "actualShippingDays": 1, "vsd": "2026-07-11", "crd": "2026-07-13" },
      "salesPrice": { "salesUnitPrice": "420.000", "salesUnitPriceIncludingTax": "462.000",
                      "salesAmount": "840.000", "salesAmountIncludingTax": "924.000" },
      "infoMessageList": [ { "code": "...", "message": "納期割引サービスは1個以上で..." } ] },
    ...
] }
```

- **リクエストは `cartDetailList` の配列で複数明細を一括投入**できる。
- 必須と思われるフィールド：`qty` / `brandCode`（ミスミ＝`MSM1`）/ `inputProductCode`（型番）。
- レスポンスは追加された各明細（`cartDetailId`・価格・納期・案内メッセージ）を返す＝そのまま UI 反映に使える。
- `indirectSalesOrderInstrumentationFlag` は計測フラグとみられる（`"1"` 固定で観測）。

### (4) カート参照（補助）

```
GET  https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/count        → { "totalCount": n }
POST https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/search       → { "totalCount": n, "maxTotalCount": 500, "cartDetailList": [...], "ccyCode": "JPY" }
```

- カート上限は `maxTotalCount: 500`（観測値）。

---

## 認証・ログインの設計

### 前提
- カート系（`shopping-cart/*`）と一括入力 UI は**ログイン必須**。
- ログインは別ドメインの SSO（`jp.sso.misumi-ec.com/api-auth-v2/...`、`account.misumi-ec.com`）。
- ログイン後、`jp.misumi-ec.com` の API は `sessionId` をクエリに付与、`api/v1/auth/check` でセッション確認。

### 「既存 Edge のログインを流用」の可否（検討）

| 方式 | 評価 |
|---|---|
| **A. 実 Edge プロファイルを自動制御** | ✕ 起動中はプロファイルがロックされ共存不可・ユーザー環境に侵襲的・bot 検知差 |
| **C. Edge の Cookie DB を復号して抽出** | ✕ App-Bound Encryption によりアプリ束縛で事実上不可・脆弱（マルウェア的手口） |
| **B. アプリ専用の永続プロファイルに一度ログイン** | ◎ **採用**。パスワードはアプリを一切通らず、Cookie は OS 保護ストアに永続化。ブラウザと同じ仕組み |

→ **B を採用**。既存の価格取得は隠し bridge WebView（[06-implementation-notes.md](./06-implementation-notes.md)）で行っている。
これを**可視ログイン可能にし、WebView2 の永続ユーザーデータフォルダにセッションを保持**すれば、
価格取得もカート投入も認証済みで動く。パスワードは保存しない。

### ⚠️ 実装着手時に確定すべき未解決点
- **`api-jp` 系（`cart-detail/add` 等）への認証情報の渡し方**（Cookie か Bearer ヘッダか `sessionId` か）は、
  今回のログ（ボディのみ記録）では未確定。
  価格 API（`sales-price-delivery/check`）は CORS `ACAO:*` のため**資格情報なし**で呼ぶ必要がある（[05-akamai-and-auth.md](./05-akamai-and-auth.md)）が、
  カート系は**認証必須**なので別の認証機構（ヘッダ or オリジン限定 CORS + Cookie）を使っているはず。
  → **Phase B 着手時にリクエストヘッダ/レスポンス CORS ヘッダを 1 回捕捉して確定する**（probe を拡張済み）。

---

## 実装方針（2 段階）

### Phase A：クリップボード連携（半自動・最小工数・最堅牢）
1. BOM エディタに「カートに追加（MISUMI）」ボタン。
2. ORDER=MISUMI 行の `型番 TAB 数量` を TSV でクリップボードへコピー。
3. 既定ブラウザで `order/part-number/create` を開き、トーストで「一括入力欄に貼り付け → 次へ」を案内。
- **ログイン情報も自動操作も一切不要**。ToS 的にも通常利用と同一。

### Phase B：エンドポイント直叩き（全自動）
1. アプリ内 WebView に一度ログイン（セッションは永続プロファイルに保持）。
2. 対象行の `{qty, brandCode, inputProductCode}` を組み立て、
   認証済み WebView から `POST shopping-cart/v1/cart-detail/add` を直接発行。
3. レスポンス（`cartDetailId`・価格・納期）をアプリに反映し、「カートを開く」導線を提示。
- 着手前に上記「未解決点（認証ヘッダ）」を 1 回の probe で確定する。

---

## 再現（probe）

`tools/misumi-api-probe/` に再現スクリプトあり（**ログ・スクショ・`.edge-profile/` は `.gitignore` 済み**：セッション Cookie を含むため never commit）。

| スクリプト | 用途 |
|---|---|
| `probe-cart.mjs` | 未ログインでカートボタン挙動を観測（型番手入力の DOM 調査） |
| `probe-paste.mjs` | 型番グリッドへの複数行ペースト挙動の確認 |
| `probe-cart-login.mjs` | ログイン後の一括入力パネル → 各エンドポイント観測（`sessionId` マスキング） |
| `probe-cart-b.mjs` | **永続プロファイル**でログインを再利用し、一括入力→列マッピング→カート投入まで自動化して `cart-detail/add` を捕捉 |

```bash
cd tools/misumi-api-probe
npm install
node probe-cart-b.mjs   # 初回のみ Edge ウィンドウでログイン（以後 .edge-profile を再利用）
```

> 🔐 **セキュリティ教訓**：観測ログには `sessionId`（ログインセッショントークン）が URL クエリに載る。
> probe は `sessionId` / `sensor_data` / 認証ヘッダをマスキングして書き出す。
> 生ログ・スクショ・プロファイルは `.gitignore` 済み。観測に使ったセッションは念のためログアウトで無効化推奨。
