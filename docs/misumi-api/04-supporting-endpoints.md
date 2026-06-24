# 04. 補助エンドポイント

核心の価格 API（[03](./03-price-delivery-check.md)）を支える周辺 API。すべて `jp.misumi-ec.com` ホスト、
クエリに `applicationId=de30e2b2-db86-435d-9929-646c11a3c4cd` を付与する。

---

## 4.1 型番サジェスト / 正規化

```
GET https://jp.misumi-ec.com/api/v1/partNumber/suggest
    ?applicationId=de30e2b2-db86-435d-9929-646c11a3c4cd
    &keyword=cbt3-8
    &field=@default,partNumberList.checkCFlag
```

入力された型番文字列を正規化し、**`brandCode` / `seriesCode` / `innerCode`** を解決する。
価格 API に必要な `brandCode` をここから得る。

レスポンス（`CBT3-8`）:
```json
{
  "partNumberList": [
    {
      "partNumberType": "1",
      "partNumber": "CBT3-8",
      "brandCode": "MSM1",
      "brandName": "ミスミ",
      "seriesCode": "110302678660",
      "innerCode": "MDM00001313262",
      "checkCFlag": "0",
      "completeType": "4",
      "normalizedKeyword": "CBT3-8",
      "searchedKeyword": "CBT3-8"
    }
  ],
  "comboFlag": "0", "normalizeFlag": "1", "convertFlag": "0"
}
```

| フィールド | 説明 |
|---|---|
| `partNumber` | 正規化後の型番 |
| `brandCode` / `brandName` | ブランド（価格 API に必須） |
| `seriesCode` | シリーズコード（preview に必要） |
| `innerCode` | 商品マスタコード（inner 検索に必要） |
| `completeType` | 型番の完成度（`4` = 完全一致と推定） |

---

## 4.2 型番プレビュー（カタログ概要）

```
POST https://jp.misumi-ec.com/api/v1/partNumberPreview/search?applicationId=...
Content-Type: application/json

{"partNumberList":[{"seriesCode":"110302678660","completeType":"4","partNumber":"CBT3-8"}]}
```

商品の表示用メタ情報（系列名・カテゴリ・画像・**標準単価レンジ**・標準出荷日数・CAD 有無）。
価格・出荷日の「確定値」ではなく**カタログ上の概算レンジ**である点に注意。

レスポンス主要フィールド:
| フィールド | 説明 |
|---|---|
| `seriesName` | 系列名（例「六角穴付ボルト －高強度βチタン合金・純チタン－」） |
| `categoryName` / `categoryCode` | カテゴリ |
| `productImageList[].url` | 商品画像 URL |
| `minStandardUnitPrice` / `maxStandardUnitPrice` | 標準単価の最小／最大（例 395 / 1970） |
| `minStandardDaysToShip` / `maxStandardDaysToShip` | 標準出荷日数レンジ |
| `cadTypeList` | 提供 CAD（2D/3D） |
| `volumeDiscountFlag` | 数量割引の有無 |
| `digitalBookPdfUrl` | カタログ PDF |

---

## 4.3 商品マスタ検索（inner）

```
GET https://jp.misumi-ec.com/api/v1/inner/search?applicationId=...&innerCode=MDM00001313262
```

社内商品マスタ。供給元・系列・カテゴリ階層を取得。

| フィールド | 説明 |
|---|---|
| `innerCode` / `zinnerCode` | 商品マスタコード |
| `productCode` | 型番 |
| `supplierCode` | 供給元コード |
| `seriesList[].categoryList[]` | カテゴリ階層（パンくず用） |

---

## 4.4 カテゴリ検索

```
GET https://jp.misumi-ec.com/api/v1/category/search
    ?categoryCode=M3311010000,M3311000000,mech_screw&applicationId=...&lang=JPN
```

カテゴリ名・説明・画像・親子関係を取得（パンくず／カテゴリ表示用）。
ページ初期表示時にも、メガナビ用に
`?ancesterType=1&categoryLevel=2&filterType=1&page=1&pageSize=30` 形式で呼ばれる。

---

## 4.5 ブランド検索

```
GET https://jp.misumi-ec.com/api/v1/brand/search?applicationId=...&lang=JPN
```

全ブランド一覧（観測時 `totalCount: 3180`）。`brandCode` ↔ `brandName` の対応表として使える。
（ページ初期化時に複数回呼ばれていた = メーカー名サジェストの候補ソースと推定）

---

> これらは価格・出荷日の取得に必須ではない（[02](./02-api-flow.md) の「最小経路」参照）が、
> 商品名・画像・カテゴリを画面表示する際に利用する。
