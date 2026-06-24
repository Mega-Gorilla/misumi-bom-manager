# 03. ★核心：単価・出荷日チェック API

型番から **実際の単価と出荷日**を返す、最重要エンドポイント。

## エンドポイント

```
POST https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check
Content-Type: application/json
```

- ホストは EC フロントとは別の **`api-jp.misumi-ec.com`**。
- Akamai Cookie（`_abck`, `bm_sz` 等）が有効である必要がある（[05](./05-akamai-and-auth.md)）。
- `applicationId` クエリは不要（観測時点）。

## リクエストボディ

```json
{
  "detailList": [
    { "qty": 1, "inputProductCode": "CBT3-8", "brandCode": "MSM1" }
  ]
}
```

| フィールド | 型 | 必須 | 説明 |
|---|---|---|---|
| `detailList` | array | ✓ | 明細の配列。**複数型番をまとめて問い合わせ可能**（BOM 一括に有用） |
| `detailList[].qty` | number | ✓ | 数量。数量スライド割引の判定にも使われる |
| `detailList[].inputProductCode` | string | ✓ | 型番 |
| `detailList[].brandCode` | string | ✓ | ブランドコード（`suggest` で取得。ミスミ自社品は `MSM1`） |

> `detailList` に複数要素を入れれば、1 リクエストで複数部品の価格・納期を取得できる（BOM の一括見積に直結）。

## レスポンス（`CBT3-8`, qty=1 の実測）

```json
{
  "receiveDateTime": "2026-06-24T21:26:39+09:00",
  "acceptDateTime": "2026-06-24T21:26:39+09:00",
  "ccyCode": "JPY",
  "errorMessageList": [],
  "warningMessageList": [],
  "infoMessageList": [],
  "detailList": [
    {
      "lineNumber": 1,
      "qty": 1,
      "leadTimeBaseDate": "2026-06-25",
      "orderClosingTime": "2000",
      "sameDayShippingClosingTime": "1705",
      "product": {
        "ginnerCode": "MDM00001313262",
        "inputProductCode": "CBT3-8",
        "brandCode": "MSM1",
        "brandName": "ミスミ",
        "seriesCode": "110302678660",
        "productCategoryCode": "M3311010000",
        "productName": "ﾛｯｶｸｱﾅﾂｷｱﾙﾐﾎﾞﾙﾄ ﾁﾀﾝﾎﾞﾙﾄ",
        "soUnitQty": 1,
        "minSoQty": 1,
        "stockFlag": "1",
        "unitWeight": "1", "weight": "1", "weightUnit": "g"
      },
      "salesPrice": {
        "salesAmount": "400",
        "salesAmountIncludingTax": "440",
        "salesUnitPrice": "400",
        "salesUnitPriceIncludingTax": "440",
        "salesUnitPriceBeforeDiscount": "400",
        "taxRate": "10.00",
        "taxAmount": "40",
        "specialFreight": "0"
      },
      "leadTime": {
        "actualShippingDays": 1,
        "earliestShippingDays": 0,
        "catalogDays": 1,
        "vsd": "2026-06-26",
        "crd": "2026-06-29",
        "earliestVsd": "2026-06-25",
        "earliestCrd": "2026-06-26",
        "estimatedDateFixFlag": "1"
      },
      "trade": {
        "immediateShippableQty": 851,
        "orderableQty": 851,
        "shippingPlantCode": "0053",
        "shippingPlantNameNative": "東日本流通センター"
      },
      "infoMessageList": [
        { "code": "NPC01J", "message": "2026/07/07以降の出荷日から納期割引（出荷5日前までキャンセル可）を選択できます。" }
      ]
    }
  ]
}
```

## レスポンス主要フィールド

### トップレベル
| フィールド | 説明 |
|---|---|
| `ccyCode` | 通貨（`JPY`） |
| `errorMessageList` / `warningMessageList` / `infoMessageList` | 全体に対するメッセージ |
| `detailList` | 明細ごとの結果配列。リクエストの `detailList` と対応 |

### `detailList[].salesPrice`（価格）
| フィールド | 説明 |
|---|---|
| `salesUnitPrice` | **単価（税別）** ← 画面の「単価(税別)」 |
| `salesUnitPriceIncludingTax` | **単価（税込）** |
| `salesAmount` / `salesAmountIncludingTax` | 小計（税別／税込）= 単価 × 数量 |
| `salesUnitPriceBeforeDiscount` | 割引前単価 |
| `taxRate` / `taxAmount` | 税率（%）／税額 |
| `specialFreight` | 特別運賃 |

### `detailList[].leadTime`（出荷日・納期）
| フィールド | 説明 |
|---|---|
| `vsd` | **出荷予定日**（画面の「出荷日」に相当。Vendor Ship Date と推定） |
| `crd` | 着荷／客先納入関連日（推定・要検証） |
| `actualShippingDays` | 実出荷日数 |
| `catalogDays` | カタログ上の標準出荷日数 |
| `earliestVsd` / `earliestCrd` | 最短の出荷／着荷日 |
| `estimatedDateFixFlag` | 日付確定フラグ |

### `detailList[].trade`（在庫・出荷元）
| フィールド | 説明 |
|---|---|
| `immediateShippableQty` | 即時出荷可能数量 |
| `orderableQty` | 発注可能数量 |
| `shippingPlantNameNative` | 出荷元拠点名（例「東日本流通センター」） |

### `detailList[].product`（商品同定）
| フィールド | 説明 |
|---|---|
| `ginnerCode` | 商品マスタコード（= `innerCode`） |
| `seriesCode` | シリーズコード |
| `productName` | 商品名（半角カナのマスタ名） |
| `stockFlag` | 在庫品フラグ（`1`=在庫品） |

## 注意点

- 価格・数量は **文字列**で返る（`"400"`）。数値化時は `Number()` 等で変換する。
- 未ログイン時は標準カタログ価格。ログイン顧客では**契約単価・数量割引・専用納期**が反映され、値が変わる可能性がある。
- `qty` を増やすと数量スライド割引（`availableSlideList` / `iconType:1000`）が効く商品があり、`salesUnitPrice` が変動する。
- 型番が存在しない／不完全な場合は `errorMessageList` にメッセージが入る想定（要追加検証）。

> 認証・Cookie の前提は [05-akamai-and-auth.md](./05-akamai-and-auth.md)、実装方針は [06-implementation-notes.md](./06-implementation-notes.md)。
