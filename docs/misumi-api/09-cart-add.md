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
| メーカー名 | `brandName` |
| 注文番号1 | `customerItemSubReferenceFirst` |
| 注文番号2 | `customerItemSubReferenceSecond` |
| 注文番号3 | `customerItemSubReferenceThird` |

> ⚠️ **お客様注文番号は「3スロット」だが、カート上は単一フィールド**（2026-07-13 `probe-cart-orderno.mjs` で実証）。上記 `First/Second/Third` は一括貼り付け UI の**入力列マッピング**にすぎず、カート投入時には **区切り文字なしで連結**され、明細1件につき**単一の `customerItemSubReference`** として送られる（例: 注文番号1〜3 に `PO-TEST-A`/`PO-TEST-B`/`PO-TEST-C` → `"customerItemSubReference":"PO-TEST-APO-TEST-BPO-TEST-C"`）。∴ データモデル上、お客様注文番号は**1明細＝1個**。アプリ側では「お客様注文番号列」を1列だけ割り当て、その値をそのまま `customerItemSubReference` として送る設計とした（Issue #14）。

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
    { "qty": 2, "brandCode": "MSM1", "inputProductCode": "CBT3-8", "customerItemSubReference": "PO-2026-001" },
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
- **`customerItemSubReference`（お客様注文番号）は任意**の明細フィールド。指定するとレスポンスの各明細にもそのままエコーされる（空なら送らない）。上表のとおり単一フィールド＝1明細1個。
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

#### ✅ 注入 JS によるトークン**直接取得（B1）は不可**（2026-07-11 スパイクで確定）
> ⚠️ **前提**：認証は **Cookie ではなく `Authorization: Bearer`**。
> Cookie は同一オリジンの `fetch` が自動付与するが、**`Authorization` ヘッダは `fetch` では自動付与されない**。
> よって「ページ文脈で生の `fetch('https://api-jp…/cart-detail/add', …)` を投げれば通る」わけではなく、Authorization を付けなければ 401/403。

`tools/misumi-api-probe/probe-cart-spike.mjs`（ログイン済み・読み取り専用・トークン値は非記録）で「注入 JS がトークンを取得できるか」を検証した結果：

- **`GACCESSTOKEN` / `GACCESSTOKENKEY` / `GREFRESHTOKENHASH` / `ACCESS_TOKEN_EXPIRATION` はすべて `document.cookie` で読めない**。
  これらは PR #13 のヘッダ観測で**リクエスト Cookie ヘッダ上の存在を確認済み**なので、読めない＝**存在しないのではなく HttpOnly** と確定できる。
- **`localStorage` / `sessionStorage` にトークンは存在しない**（optimizely・計測・ルーティング系のみ、JWT 形状もゼロ）。
- → **注入 JS からアクセストークンを直接取り出す方法は無い**（HttpOnly Cookie ＋ 実トークンは SPA のメモリ内保持とみられる）。
  よって「トークンを自前構築して直叩き」する素朴な直取得方式（**B1**）は**不可**。

#### ✅ B2-a（fetch/XHR フック方式）を実証（2026-07-11 `probe-cart-hook.mjs`）
document_start で `window.fetch`/`XMLHttpRequest` をフックし、**サイト自身が出す api-jp 呼び出しから
`Authorization: Bearer`（と付随ヘッダ）を捕捉できること／それを再利用して我々が api-jp を叩けること**を実証した。

- **捕捉 ✅**：ログイン後の api-jp リクエスト（`customer/v1/user/get`）から `Authorization: Bearer`（len≈455、XHR 経由）を捕捉。
  同時に付随ヘッダの**具体値**も判明：**`x-client-program: JP_ORDER`** / **`x-language-code: JPN`**。
- **再利用 ✅**：捕捉した `Authorization` ＋ `x-client-program` ＋ `x-language-code` を付けて、
  **我々自身の `fetch` で読み取り専用 `cart-detail/count` を叩き 200（`{"totalCount":0}`）** を確認。
  - 補足：`Authorization` **だけ**で叩くと 400 `"Client Program is null."`（＝トークンは受理されており、`x-client-program` 欠落が原因）。**付随ヘッダの同送が必須**。

#### → 実装方針は B2-a（fetch/XHR フック捕捉→自前発行）に確定
- **B2-a（採用・実証済み）** — ログイン済み WebView に `window.fetch`/`XMLHttpRequest` フックを document_start で注入し、
  サイトがロード時に出す api-jp 呼び出しから `Authorization: Bearer` ＋ `x-client-program`/`x-language-code` を**1回キャプチャ**して保持。
  以降は**我々が `cart-detail/add` を直接発行**（キャプチャした Bearer ＋ 付随ヘッダ）。単一エンドポイント一括投入を DOM 自動操作なしで実現・HttpOnly 回避・既存 bridge WebView＋eval 基盤に載る。401 時は再キャプチャ。
- **B2-b：サイト UI 自動操作方式（実証済み fallback）** — 一括入力 textarea → 次へ → 列マッピング → カートへ追加 をプログラム操作（`probe-cart-b.mjs` で**実証済み**）。
  トークン管理は不要だが DOM/フロー変更に弱い。B2-a が将来壊れた場合の**保険**として維持。
- **共通の前提**：どちらも「**可視ログイン＋永続プロファイルの WebView**」が新規に必要（現 bridge WebView は匿名 `needs_auth:false`）。

---

## 実装方針（2 段階）

### Phase A：クリップボード連携（半自動・最小工数・最堅牢）
1. BOM エディタに「カートに追加（MISUMI）」ボタン。
2. ORDER=MISUMI 行の `型番 TAB 数量` を TSV でクリップボードへコピー。
3. 既定ブラウザで `order/part-number/create` を開き、トーストで「一括入力欄に貼り付け → 次へ」を案内。
- **ログイン情報も自動操作も一切不要**。ToS 的にも通常利用と同一。

### Phase B：エンドポイント直叩き（全自動 / 方式 B2-a）
1. **可視ログイン＋永続プロファイルの WebView** を用意し、一度ログイン（セッションは WebView2 の永続ユーザーデータフォルダに保持）。
2. document_start で `window.fetch`/`XMLHttpRequest` フックを注入し、サイトがロード時に出す api-jp 呼び出しから
   `Authorization: Bearer` ＋ `x-client-program`（=`JP_ORDER`）/ `x-language-code`（=`JPN`）を**1回キャプチャ**
   （トークンは HttpOnly ＋ メモリ内保持のため注入 JS からは直接読めない＝実証済み）。
3. 対象行の `{qty, brandCode, inputProductCode}` を組み立て、キャプチャした Bearer ＋
   `content-type: application/json` / `x-client-program` / `x-language-code` / `idempotency-key` を付けて
   `POST https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/add` を発行。
   （**付随ヘッダの同送は必須**。`Authorization` だけだと 400 `"Client Program is null."` になる＝`probe-cart-hook.mjs` で確認済み）
4. レスポンス（`cartDetailId`・価格・納期）をアプリに反映し、「カートを開く」導線を提示。401 時は Bearer を再キャプチャして再試行。
- フォールバックは **B2-b（サイト UI 自動操作）**。詳細は上記「→ 実装方針は B2」参照。

---

## 実装（Phase B / B2-a・2026-07-11）
アプリ本体に B2-a を実装済み（Issue #14）。既存の隠し bridge WebView を認証エンジンとして拡張：

- `shared/misumi-auth-hook.js`（新規）：bridge に `initialization_script` として document_start 注入。`fetch`/`XHR` をラップし api-jp の `Authorization: Bearer` ＋ `x-client-program`/`x-language-code` を `window.__mbmAuth` に捕捉（値はページ内のみ・passthrough）。
- `shared/misumi-lookup.js`：`MisumiCore.authStatus()` / `addToCart(items)` を追加。捕捉ヘッダ＋`idempotency-key` で `cart-detail/add` を発行、`brandCode` は `suggest` で解決。
- `src-tauri/src/lib.rs`：`cart_add` / `misumi_auth_status` / `misumi_login`（bridge を表示→ログイン→Bearer 捕捉を検知→hide。着地ページが api-jp を呼ばない場合は注文ページへ nudge）/ `misumi_open_cart`。bridge の close を **hide** に差し替え（＝ログイン/カート表示に使い回す）。
- フロント：ツールバー「カートに追加（MISUMI）」→ 対象行を型番マージ・Qty×倍率で収集 → 確認ダイアログ → `cart_add`（未ログインは `misumi_login`→再試行）→「カートを開く」。
  対象行は **グリッドのチェックボックスで選択した行があればその行のみ、未選択なら `ORDER=MISUMI` の全行**（いずれも `ORDER=MISUMI`＋型番あり）。確認ダイアログにどちらのモードかを明示する。
- 認証情報の扱い：パスワードはアプリを通さず、Bearer はページ内 `window.__mbmAuth` のみで保持し Rust/ログ/DB に一切出さない。セッションは WebView2 の永続プロファイルで維持。

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
| `probe-cart-spike.mjs` | **非永続**でログイン後、注入 JS が Bearer トークンを取得できるか（`document.cookie`/`localStorage`/`sessionStorage`）を検証し **B1 不可を確定**。読み取り専用（`cart-detail/count`）でカートを汚さず、**トークン値は一切記録しない**（長さ・キー名・HTTP ステータスのみ） |
| `probe-cart-hook.mjs` | **非永続**でログイン後、document_start の `fetch`/`XHR` フックがサイトの `Authorization: Bearer` ＋ `x-client-program`/`x-language-code` を捕捉→再利用して `cart-detail/count` が 200 になることを実証し **B2-a を確定**。読み取り専用・**トークン値は非記録**（付随ヘッダ値は非機密なので記録） |

```bash
cd tools/misumi-api-probe
npm install
node probe-cart-b.mjs   # 初回のみ Edge ウィンドウでログイン（以後 .edge-profile を再利用）
```

> 🔐 **セキュリティ教訓**：観測ログには `sessionId`（ログインセッショントークン）が URL クエリに載る。
> probe は `sessionId` / `sensor_data` / 認証ヘッダをマスキングして書き出す。
> 生ログ・スクショ・プロファイルは `.gitignore` 済み。観測に使ったセッションは念のためログアウトで無効化推奨。
