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

### ✅ 認証機構（2026-07-11 ヘッダ観測で確定）

`tools/misumi-api-probe/probe-cart-auth.mjs`（リクエスト/レスポンスヘッダを捕捉。Cookie は**名前のみ**・値は全マスク）で、
`cart-detail/add` を含む全 api-jp 呼び出しの認証機構を確定した。

- **`api-jp.misumi-ec.com` 系（`cart-detail/add` / `search` / `count` / `price-delivery-calculation/check` / `sales-order/file/parse`）は
  `Authorization: Bearer <JWT 約455文字>` で認証する。Cookie は送っていない。**
  - レスポンス CORS は **`Access-Control-Allow-Origin: *` ＋ `Access-Control-Allow-Credentials: false`**（`expose-headers: *`）。
    つまり **ステートレス・オリジン非依存**で、Bearer トークンさえ付ければ**どのオリジンからでも**呼べる
    （＝ Cookie/同一オリジン制約なし。ただし後述の Akamai gate は残る）。
  - 価格 API（`sales-price-delivery/check`）と**完全に同じ CORS/認証パターン**。価格取得基盤で確立済みの方式がそのまま使える。
- **`jp.misumi-ec.com/api/v1/*`（`auth/check` / `brand`・`inner`・`category`/search）は別方式**＝
  **`sessionId` クエリ＋Cookie・同一オリジン・`Allow-Credentials: true`**（サイト BFF）。カート投入には**使わない**。
- **Bearer トークンの出所**：ログイン時に発行され、`.misumi-ec.com` の Cookie
  `GACCESSTOKEN` / `GACCESSTOKENKEY` / `GREFRESHTOKENHASH` / `ACCESS_TOKEN_EXPIRATION` 一式で保持。
  期限切れ時は `POST https://jp.sso.misumi-ec.com/api-auth-v2/auth/api/sso/satellite/misumi-ec`（body `at=…&rt=…`）でリフレッシュ。
  → **ログイン済み WebView 内なら、サイトの JS がこのリフレッシュを自動継続する**ので、アプリはトークン失効を意識しなくて済む。
- **複製すべき付随ヘッダ**（`cart-detail/add` の実測キー）：
  `content-type: application/json` / `x-client-program` / `x-language-code` /
  `idempotency-key`（＝重複投入防止。**再試行を安全にできる**ので必ず付ける）。
  `x-datadog-*` は分散トレース用で認証には無関係（送らなくてよい）。

#### Phase B 実装時に確認する**軽微な残点（ブロッカーではない）**
- **Bearer トークンをアプリ側 JS からどう読むか**（`GACCESSTOKEN` Cookie が `document.cookie` で読めるか＝非 HttpOnly か、
  あるいは SPA の memory/localStorage 保持か）は未確認。
  ログイン済み WebView 内で `document.cookie` を 1 行読むだけで判明する軽微な確認事項で、着手を妨げない。
  最悪、**トークン抽出に頼らず**「ログイン済み WebView の**ページ文脈内で `fetch('/…/cart-detail/add', …)` を実行**」すれば、
  サイトと同じ経路でトークンが付与され、この残点自体が不要になる。

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
   ログイン済み WebView の**ページ文脈内**で
   `POST https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/add` を発行
   （`Authorization: Bearer <token>` ＋ `x-client-program` / `x-language-code` / `idempotency-key` を付与）。
3. レスポンス（`cartDetailId`・価格・納期）をアプリに反映し、「カートを開く」導線を提示。
- **認証機構は確定済み（上記「✅ 認証機構」参照＝Bearer JWT）**。残るのは「トークンの読み出し方」の軽微確認のみで、
  ページ文脈内 `fetch` にすればそれも不要。

---

## 再現（probe）

`tools/misumi-api-probe/` に再現スクリプトあり（**ログ・スクショ・`.edge-profile/` は `.gitignore` 済み**：セッション Cookie を含むため never commit）。

| スクリプト | 用途 |
|---|---|
| `probe-cart.mjs` | 未ログインでカートボタン挙動を観測（型番手入力の DOM 調査） |
| `probe-paste.mjs` | 型番グリッドへの複数行ペースト挙動の確認 |
| `probe-cart-login.mjs` | ログイン後の一括入力パネル → 各エンドポイント観測（`sessionId` マスキング） |
| `probe-cart-b.mjs` | **永続プロファイル**でログインを再利用し、一括入力→列マッピング→カート投入まで自動化して `cart-detail/add` を捕捉 |
| `probe-cart-auth.mjs` | **非永続**（毎回ログイン・プロファイル不要）でカート投入まで自動化し、**リクエスト/レスポンスのヘッダ**を捕捉して認証機構（Bearer/CORS）を確定。Cookie は名前のみ・値は全マスク、`sessionId`/`at`/`rt`/`set-cookie` 値も全マスク |

```bash
cd tools/misumi-api-probe
npm install
node probe-cart-b.mjs   # 初回のみ Edge ウィンドウでログイン（以後 .edge-profile を再利用）
```

> 🔐 **セキュリティ教訓**：観測ログには `sessionId`（ログインセッショントークン）が URL クエリに載る。
> probe は `sessionId` / `sensor_data` / 認証ヘッダをマスキングして書き出す。
> 生ログ・スクショ・プロファイルは `.gitignore` 済み。観測に使ったセッションは念のためログアウトで無効化推奨。
