# BOM エディタ 詳細計画

関連 Issue: [#3](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/3) ／ 前提: 単一型番ルックアップ（[#1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/1) 完了）

## 1. 目的

部品表（BOM）をアプリ上で編集・管理し、`ORDER=MISUMI` の行に **単価・出荷日・在庫**を自動付与する。
具体的には次の 3 つを満たす：

1. **編集表 UI**：`No / Parts Name / Parts No / ORDER / Qty / MATERIAL` ＋ MISUMI データを表示・編集できる表。
2. **Excel 取込**：任意の BOM Excel を、**どの列がどのデータに対応するか設定してから**取り込む。
3. **任意の列・行追加**ができるエディタ。

## 2. 取込の前提（任意の BOM に対応）

取り込む Excel BOM は様式が一定でない。本機能は**特定ファイルに依存せず、任意の BOM を取り込めること**を前提に設計する。具体的には、取込は次を汎用的に扱う：

- **ヘッダ行は先頭とは限らない**（タイトル・パラメータ行が上にあり得る）→ シート・ヘッダ行・開始行を選択できる。
- **列構成はファイルごとに異なる** → 固定列ではなく**列マッピング**で吸収。既知フィールド ＋ 任意パススルー列。
- **発注先を示す列が無いシート**（全行 MISUMI など）→ 取込時にソースを固定値指定できる。
- **数量倍率などのシートパラメータ**が存在し得る → 任意で取り込む。

> 動作確認には手元の BOM Excel をサンプルとして用いるが、その構造を固定仕様とはしない。

## 3. 要件

### 機能要件
- [ ] 編集表：行の追加・複製・削除・並べ替え、セルのインライン編集。
- [ ] 列：既知列（No/PartsName/PartsNo/ORDER/Qty/MATERIAL）＋ **任意列の追加・改名・削除**。
- [ ] MISUMI データ列：単価（税別/税込）・出荷日・在庫・小計・状態・メッセージ・取得日時 等を**追加列**として表示。
- [ ] `ORDER=MISUMI` の行を **一括取得**（行ごとに結果/エラーを反映、進捗表示）。手動「再取得」あり。
- [ ] **MISUMI フィールド → 列のリンク設定**：列ごとに 手動／リンク／追加 と書込ポリシー（`overwrite`/`fillEmpty`/`suggest`）をユーザー設定し、テンプレ保存（→ 7. MISUMI 連携）。
- [ ] Excel 取込：シート選択 → ヘッダ行選択 → 列マッピング → プレビュー → 取込。マッピングは**テンプレ保存**で再利用。
- [ ] 数量倍率（シートのパラメータ由来）を BOM 設定として保持し、小計に反映。
- [ ] 保存・読込：BOM をローカル JSON 文書として保存／復元。

### 非機能要件
- 数百行規模で実用的な編集レスポンス。
- 価格・出荷日は揮発性 → **取得日時を保持**し、価格を「真実の源」として永続化しない（再取得前提）。
- MISUMI への配慮：一括は **≤100 件/リクエスト・低並列**（[misumi-api/07](../../misumi-api/07-batch-and-limits.md)）。

## 4. データモデル（案）

```ts
type ColumnKind = "core" | "custom" | "misumi"; // 由来
type ColumnKey =
  | "no" | "partsName" | "partsNo" | "order" | "qty" | "material" // core
  | string; // custom.* / misumi.*

interface ColumnLink {     // MISUMI 連携設定（任意）。設定した列は MISUMI 値で駆動される
  misumiField: string;     // 例 "product.seriesName" / "salesPrice.salesUnitPrice"
  write: "overwrite" | "fillEmpty" | "suggest"; // 列ごとに選択（既定 fillEmpty）
}

interface ColumnDef {
  key: ColumnKey;
  label: string;
  kind: ColumnKind;        // 由来: core / custom / misumi(追加列)
  editable: boolean;
  width?: number;
  link?: ColumnLink;       // 設定があれば MISUMI リンク列（手動列は undefined）
}

interface MisumiData {
  status: "idle" | "pending" | "ok" | "error";
  brandCode?: string;
  unitPrice?: string;        // 税別
  unitPriceTax?: string;     // 税込
  shipDate?: string;         // vsd
  stock?: number;
  subtotal?: number;         // 単価 × Qty × 倍率
  errors: string[];          // detailList[].errorMessageList 等
  warnings: string[];
  fetchedAt?: string;        // ISO
}

interface BomRow {
  id: string;                // 内部 ID
  no?: number;
  partsName?: string;
  partsNo?: string;          // 型番
  order?: string;            // 発注先（"MISUMI" など）
  qty?: number;
  material?: string;
  custom: Record<string, string>;  // 任意列
  misumi?: MisumiData;       // ORDER=MISUMI のみ
}

interface BomDoc {
  version: 1;
  meta: { name?: string; importedFrom?: string; qtyMultiplier: number; updatedAt: string };
  columns: ColumnDef[];      // 表示順を含む
  rows: BomRow[];
}
```

## 5. アーキテクチャ

```
React UI (AG Grid)
  ├─ Excel 取込: SheetJS で xlsx をパース → マッピング UI → BomDoc 生成
  ├─ 編集: BomDoc を AG Grid で編集（行/列の追加・編集・削除）
  ├─ 一括取得: ORDER=MISUMI 行の partsNo を invoke("lookup_parts", …)
  └─ 保存/読込: plugin-dialog + plugin-fs で BomDoc(JSON) を入出力

Rust backend (既存ブリッジ WebView)
  └─ lookup_parts(parts: [{qty, partNumber}]) → MisumiCore.lookupMany を ≤100 件チャンクで実行
```

- **Excel 読込**：`xlsx`（SheetJS）。`sheet_to_json({ header: 1 })` で生配列を取り、ヘッダ行 index を指定して列を解釈（解析実績あり）。
- **書込/エクスポート（Phase 3）**：既存 `write-excel-file` を流用、または `xlsx`/`exceljs` に統一。
- **バックエンド追加**：`lookup_parts` コマンド。既存 `lookup_part`（単一）と同じブリッジ＋共有コア（`window.MisumiCore.lookupMany`）を使い、**≤100 件チャンク**＋進捗イベント。N 回単発呼びはしない。
- **永続化**：`BomDoc` を JSON で保存（拡張子例 `.bom.json`）。列マッピングテンプレは app config（`@tauri-apps/api/path` の appConfigDir）に保存。

## 6. Excel 取込フロー（Phase 2）

1. ファイル選択（plugin-dialog）→ SheetJS で全シート読込。
2. **シート選択**（複数シートを順に取り込めるように）。
3. **ヘッダ行選択**（プレビューで先頭 ~10 行を表示し、ユーザがヘッダ行をクリック指定）。
4. **列マッピング**：検出した各列を、ターゲット項目（PartsNo/PartsName/ORDER/Qty/MATERIAL/任意）にドロップダウンで割当。
   - ターゲットにできない列は「任意列として取り込む / 取り込まない」を選択。
   - `ORDER` 列が無いシート（例 `Misumi`）では「ソース＝固定値（全行 MISUMI）」を選べる。
   - 数量倍率を表すパラメータセルがあれば「数量倍率として取り込む」を任意で指定。
5. **プレビュー**（マッピング適用後の表）→ 問題なければ取込で `BomDoc` 生成。
6. **マッピングのテンプレ保存**（ファイル名やヘッダ構成で再利用）。

## 7. MISUMI 連携

### 7.1 取得対象
- 対象：`order` が MISUMI（既定は完全一致 "MISUMI"、設定で前方一致等に拡張可）の行。
- 解決：`partsNo` を `suggest`（共有コア）で正規化し `brandCode` を得てから `lookupMany`。
- 取得日時を保持し「再取得」で更新。行レベルエラー（例: 最小注文数 `【数量エラー】…`）は #1 と同様に明示表示。

### 7.2 MISUMI フィールド → 列のマッピング（ユーザー設定）
MISUMI は多数のフィールドを返すため、「どのフィールドをどの列に結ぶか」を**固定せずユーザー設定**にする（Excel 取込マッピングと対の概念）。各列は 3 つの役割を持つ：
- **手動(manual)**：MISUMI は書き込まない（REMARKS / FILE NAME / MATERIAL 等）。
- **リンク(linked)**：`ColumnLink` で指定した MISUMI フィールド値で駆動（例: Parts Name ← `product.seriesName`）。
- **追加(added)**：既存列に対応しない MISUMI データを新規列に（単価・出荷日 等）。「利用可能フィールド一覧」から追加。

取得できる主なフィールド（詳細は [misumi-api/03](../../misumi-api/03-price-delivery-check.md) / 04）：

| 区分 | フィールド（例） |
|---|---|
| リンク候補 | `seriesName`/`productName`（名称）、`partNumber`（正規化型番）、`brandName`（メーカー） |
| 追加データ | `salesUnitPrice`/`…IncludingTax`（単価）、`vsd`（出荷日）、`crd`（着荷）、`immediateShippableQty`（即納数）、`minSoQty`（最小注文数）、`shippingPlantNameNative`（出荷元）、`weight`、`categoryName`、CAD/画像/PDF、状態/`errorMessageList` |
| 内部キー（既定で列にしない） | `innerCode`/`ginnerCode`/`seriesCode`/`supplierCode`/`brandCode` |

> ⚠ `MATERIAL` は本 API から構造化フィールドとして取得できない（名称や説明文に含まれることはあるが非構造）→ 既定では手動列。**リンク可能な列とそうでない列が実在する**点に留意。

### 7.3 書込ポリシー（列ごとに選択）
リンク列は `ColumnLink.write` で挙動を選べる。**`fillEmpty` と `overwrite` の両方**に対応する：
- `fillEmpty`（既定）：セルが空のときだけ MISUMI 値で補完。
- `overwrite`：常に MISUMI 値で上書き。
- `suggest`：セルには書かず、MISUMI 値を提案表示（ユーザーが反映）。
- いずれのモードでも、**ユーザー編集値と MISUMI 値が食い違う行はハイライト**し、サイレント上書き/不一致を可視化する。

### 7.4 集計
- 小計：`単価(税別) × Qty × qtyMultiplier`。合計・最大リードタイム（出荷見込み）は Phase 3。

リンク設定（`ColumnLink`）も `BomDoc` の `columns` に保持し、**テンプレ保存**で再利用する。

## 8. グリッド選定：AG Grid Community

| 候補 | 評価 |
|---|---|
| **AG Grid Community（採用）** | 動的列・インライン編集・キーボード操作・コピペが標準、無料(MIT)。BOM 編集を最短で実装可。バンドルはやや大きめ。 |
| TanStack Table | ヘッドレスで軽量だが編集 UI/キーボード操作を自前実装（工数増）。 |
| Handsontable | スプレッドシート体験は最良だが非評価利用は商用ライセンス要。 |

→ 動的列＋インライン編集＋追加行/列という要件に対し、実装速度と機能のバランスで **AG Grid Community** を採用。

## 9. フェーズ計画と受け入れ条件

### Phase 1 — 編集表 ＋ 一括取得
- データモデル（`BomDoc`）＋ AG Grid 表示・編集。
- 行の追加/複製/削除、任意列の追加/改名/削除。
- `ORDER=MISUMI` 行の一括取得 → 単価/出荷日/在庫/エラーを反映（進捗表示）。
- `BomDoc` の JSON 保存/読込。
- バックエンド `lookup_parts`（チャンク＋進捗）追加。
- **受け入れ**：手入力 BOM で MISUMI 行に単価・出荷日が付き、行レベルエラーも表示。保存→再読込で復元。

### Phase 2 — Excel 取込
- シート＋ヘッダ行＋列マッピング＋プレビュー＋取込。
- マッピングテンプレ保存／再利用。数量倍率パラメータの取り込み。
- **受け入れ**：様式の異なる代表的な BOM Excel（ヘッダ行が先頭でない／列構成が異なる／発注先列が無く全行 MISUMI 等）を取り込め、MISUMI 行の一括取得まで通る。

### Phase 3 — 仕上げ
- Excel エクスポート、全行再取得、価格/正規化キャッシュ、合計（小計合計・最大リードタイム）。

## 10. 技術的決定事項（まとめ）

| 項目 | 決定 |
|---|---|
| 編集グリッド | AG Grid Community |
| Excel 読込 | SheetJS (`xlsx`) |
| Excel 書込 | `write-excel-file`（既存）/ Phase 3 で検討 |
| 一括取得 | `lookup_parts`（`MisumiCore.lookupMany`、≤100件チャンク、進捗イベント） |
| MISUMI 連携マッピング | `ColumnLink`（`misumiField` + `write: overwrite/fillEmpty/suggest`）。列ごとに 手動/リンク/追加 を設定、差異ハイライト、テンプレ保存 |
| 永続化 | BomDoc=ローカル JSON、マッピング=app config テンプレ |
| 数量倍率 | `BomDoc.meta.qtyMultiplier`（シートパラメータ由来） |

## 11. リスク・未決事項

- AG Grid Community のバンドルサイズ増（Tauri なので影響は限定的だが要計測）。
- マッピングテンプレの同定キー（ファイル名 / ヘッダ構成のどちらを優先するか）→ Phase 2 で確定。
- `ORDER` 判定ルール（完全一致 "MISUMI" vs 前方一致/別名）→ 既定は完全一致、設定で拡張。
- 大規模 BOM（数千行）時の一括取得レイテンシ（[07](../../misumi-api/07-batch-and-limits.md)）→ チャンク＋進捗で吸収、必要なら上限警告。
- 価格の揮発性 → 永続化は「取得日時付き」。発注の最終確定値は都度再取得を促す。

## 12. スコープ外（当面）

- ログイン（顧客別契約価格）。
- MISUMI への自動発注（カート投入/EDI）。
- 複数 BOM の横断管理 / DB 化（必要になれば SQLite へ）。

## 13. 参考

- MISUMI API 仕様: [`../../misumi-api/`](../../misumi-api/)（連鎖 02 / 価格 03 / バッチ上限 07 / テスト 08）
- 共有コア: [`../../../shared/misumi-lookup.js`](../../../shared/misumi-lookup.js)（`window.MisumiCore.lookupMany` を Phase 1 で活用）
