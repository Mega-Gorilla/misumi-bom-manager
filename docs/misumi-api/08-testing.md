# 08. テスト基盤（共有コア + CLI）

MISUMI 連携ロジックを **CLI から（手動・半自動で）検証**するための仕組み。

> ⚠️ **前提**：実 Edge/Chromium が **Akamai を通過できる環境**が必要（既定は headed 起動）。
> ヘッドレスや CI・制限環境では Akamai に弾かれて**データ取得に失敗し得る**。
> その場合でも CLI は**必ずタイムアウトして `{ok:false,error}` を出力し exit 1 で終了**する（ハングしない）。
> 「どこでも CI で価格が取れる」ことは保証しない — “環境が許せば自動アサートに使える” 位置づけ。

## 単一ソース：`shared/misumi-lookup.js`

型番 → 単価・出荷日の fetch 連鎖（`suggest` → `sales-price-delivery/check`）は
[`shared/misumi-lookup.js`](../../../shared/misumi-lookup.js) に **一本化**されている。

- **Tauri 本体**：`src-tauri/src/lib.rs` が `include_str!("../../shared/misumi-lookup.js")` で埋め込み、
  ブリッジ WebView に注入して `window.MisumiCore.lookupOne(...)` を呼ぶ。
- **CLI**：`tools/misumi-cli` が Playwright の `page.evaluate(CORE)` で同じファイルを注入して呼ぶ。

→ MISUMI 仕様変更（例: [CORS/credentials 修正](./05-akamai-and-auth.md#-credentials-の落とし穴実測で確認)）は
**共有コア 1 か所**を直せばアプリ・CLI の両方に反映される（ロジック二重管理の排除）。

### `window.MisumiCore` API

| 関数 | 説明 |
|---|---|
| `suggest(keyword)` | 型番正規化 → `{partNumber, brandCode, seriesCode, ...}` \| null |
| `priceDelivery(detailList)` | `[{qty, inputProductCode, brandCode}]` → 価格・出荷日レスポンス |
| `lookupOne(partNumber)` | 単一型番：suggest → price（`{ok, suggest, price}`） |
| `lookupMany(parts)` | 複数型番：各 suggest → **1 リクエスト**で price（バッチ） |

## CLI：`tools/misumi-cli`

```bash
cd tools/misumi-cli && npm install

node cli.mjs lookup CBT3-8                       # stdout に JSON、失敗時 exit 1
node cli.mjs batch CBT3-8 CBTB5-12 E-GBSCB4-20   # バッチ
node cli.mjs lookup CBT3-8 --pretty              # 人間向けサマリを stderr に併記
node cli.mjs lookup CBT3-8 --timeout=45          # 全体デッドライン（秒）
```

なぜブラウザ経由か：Akamai のため素の HTTP では 403。実 Edge を起動し共有コアを注入して叩く（[05](./05-akamai-and-auth.md)）。

### 終了保証（ハングしない）

各実行は**全体デッドライン**（既定 60 秒、`--timeout=SEC` / `TIMEOUT_MS`）で必ず終了する。
`page.evaluate` には標準タイムアウトが無いため、Akamai が fetch を保留すると無限待ちになり得る。
これを防ぐため **(1) Node 側の全体デッドライン**、**(2) ページ内呼び出しの JS タイマーとの `Promise.race`**、
**(3) `browser.close()` も詰まった場合の強制 `process.exit`** の三重で、**JSON を出して exit 1**で抜ける。

### アサート例（実 Edge が Akamai を通過できる環境でのみ成功）

```bash
# 単価が正の数で取れることを確認（取れない環境ではこの行は失敗する＝それも検知できる）
node tools/misumi-cli/cli.mjs lookup CBT3-8 \
  | jq -e '.ok and (.price.detailList[0].salesPrice.salesUnitPrice|tonumber > 0)'
```

実測（2026-06-27, 未ログイン）:

| 型番 | 単価(税別) | 税込 | 出荷日 | 備考 |
|---|---|---|---|---|
| CBT3-8 | 400 | 440 | 2026-06-29 | 在庫 851 |
| CBTB5-12 | 465 | 512 | 2026-06-29 | 在庫 608 |
| E-GBSCB4-20 | 14 | 15 | 2026-06-29 | ⚠ 200個から注文可（行ごとの数量エラー例） |

## ツールの使い分け

| ツール | 役割 |
|---|---|
| `tools/misumi-cli` | **共有コアを使う正式 CLI**。回帰テスト・自動検証はこちら |
| `tools/misumi-api-probe` | 低レベルな調査用 probe（エンドポイント単体・件数/レイテンシ計測など。[07](./07-batch-and-limits.md)） |

## 既知の制約

- 既定は headed 起動（Akamai 実証済み経路）。`HEADLESS=1` でヘッドレスにできるが Akamai に弾かれる可能性あり（要検証）。
- `shared/misumi-lookup.js` は `src-tauri` の外にあるため、これ単体を編集しても `tauri dev` の自動再ビルドは走らない（`lib.rs` 等を触ると再ビルドされる）。
