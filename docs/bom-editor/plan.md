# BOM エディタ 詳細計画

関連 Issue: [#3](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/3) ／ 前提: 単一型番ルックアップ（[#1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/1) 完了）

## 1. 目的

部品表（BOM）をアプリ上で編集・管理し、`ORDER=MISUMI` の行に **単価・出荷日・在庫**を自動付与する。
具体的には次の 3 つを満たす：

1. **編集表 UI**：`No / Parts Name / Parts No / ORDER / Qty / MATERIAL` ＋ MISUMI データを表示・編集できる表。
2. **Excel 取込**：実 BOM（例 `YUBI Glove Assy_BOM.xlsx`）を、**どの列がどのデータに対応するか設定してから**取り込む。
3. **任意の列・行追加**ができるエディタ。

## 2. 実ファイル分析（`D:\gDrive\omakase\yubi\docs\BOM\YUBI Glove Assy_BOM.xlsx`）

設計を実物に合わせるため SheetJS で解析した結果。**この分析が後述の設計判断の根拠**。

### シート① `YUBI Glove Assy_BOM`（範囲 A1:K52） — 組立 BOM
- 0–1 行目：タイトル行（`YUBI Glove Assy_PA…` 等）。**ヘッダは 3 行目（index 2）**。
- ヘッダ列：`No.` / `PART No.` / `TYPE` / `ORDER` / `PART NAME` / `Qty.` / `MATERIAL` / `REMARKS` / `FILE NAME (STL/STEP)` / `URL`
- 中身：大半が `TYPE=3D-PRINTED`・`ORDER` 空（＝MISUMI 発注対象ではない）。

### シート② `Misumi`（範囲 A1:D201） — MISUMI 発注リスト
- 0 行目：**`発注セット数: 3`**（数量の全体倍率パラメータ）。
- **ヘッダは 2 行目（index 1）**：`No` / `商品型番（必須）` / `数量（必須）` / `注文番号1`
- 中身：199 件の実型番（`SSFRHQ8-15-F5-P5-T`, `GEAKBG1.0-30-6-A-8`, `CB2-6`, …）。

### ここから得た設計示唆
1. **ヘッダ行は 0 行目とは限らない** → 取込 UI に「シート選択・ヘッダ行選択・開始行」が必須。
2. **シートごとに形が違う**（英語の組立 BOM / 日本語の発注リスト）→ 列は固定にせず、**マッピングで吸収**。
3. **既知フィールド ＋ 任意パススルー列**（TYPE/REMARKS/FILE NAME/URL/注文番号…）。
4. **シートパラメータ**（`発注セット数`）を BOM の数量倍率として拾う。
5. **`ORDER` 列が発注先**。`ORDER=MISUMI` のみ自動取得、それ以外（3D-PRINTED 等）はパススルー。
6. 発注リストシートのように **ORDER 列が無い＝全行 MISUMI** のケースもある → 取込時に「ソース＝固定値（全行 MISUMI）」を選べるようにする。

## 3. 要件

### 機能要件
- [ ] 編集表：行の追加・複製・削除・並べ替え、セルのインライン編集。
- [ ] 列：既知列（No/PartsName/PartsNo/ORDER/Qty/MATERIAL）＋ **任意列の追加・改名・削除**。
- [ ] MISUMI データ列（読み取り専用）：単価（税別/税込）・出荷日・在庫・小計・状態・メッセージ・取得日時。
- [ ] `ORDER=MISUMI` の行を **一括取得**（行ごとに結果/エラーを反映、進捗表示）。手動「再取得」あり。
- [ ] Excel 取込：シート選択 → ヘッダ行選択 → 列マッピング → プレビュー → 取込。マッピングは**テンプレ保存**で再利用。
- [ ] 数量倍率（`発注セット数` 相当）を BOM 設定として保持し、小計に反映。
- [ ] 保存・読込：BOM をローカル JSON 文書として保存／復元。

### 非機能要件
- 数百行規模（実ファイルは ~200 行）で実用的な編集レスポンス。
- 価格・出荷日は揮発性 → **取得日時を保持**し、価格を「真実の源」として永続化しない（再取得前提）。
- MISUMI への配慮：一括は **≤100 件/リクエスト・低並列**（[misumi-api/07](../misumi-api/07-batch-and-limits.md)）。

## 4. データモデル（案）

```ts
type ColumnKind = "core" | "custom" | "misumi"; // 由来
type ColumnKey =
  | "no" | "partsName" | "partsNo" | "order" | "qty" | "material" // core
  | string; // custom.* / misumi.*

interface ColumnDef {
  key: ColumnKey;
  label: string;
  kind: ColumnKind;
  editable: boolean;       // misumi 列は false
  width?: number;
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
   - `発注セット数` のようなパラメータセルは「数量倍率として取り込む」を任意で指定。
5. **プレビュー**（マッピング適用後の表）→ 問題なければ取込で `BomDoc` 生成。
6. **マッピングのテンプレ保存**（ファイル名やヘッダ構成で再利用）。

## 7. MISUMI 連携

- 対象：`order` が MISUMI（既定は完全一致 "MISUMI"、設定で前方一致等に拡張可）の行。
- 解決：`partsNo` を `suggest`（共有コア）で正規化し `brandCode` を得てから `lookupMany`。
- 反映：行ごとに `MisumiData`（単価・出荷日・在庫・状態・`errorMessageList`/`warningMessageList`）。
  - 行レベルエラー（例: 最小注文数 `【数量エラー】…`）は #1 と同様に明示表示。
- 小計：`単価(税別) × Qty × qtyMultiplier`。合計・最大リードタイム（出荷見込み）は Phase 3。
- 取得日時を保持し、「再取得」で更新。

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
- マッピングテンプレ保存／再利用。`発注セット数` 取り込み。
- **受け入れ**：`YUBI Glove Assy_BOM.xlsx` の両シートを正しく取り込め、`Misumi` シートから一括取得まで通る。

### Phase 3 — 仕上げ
- Excel エクスポート、全行再取得、価格/正規化キャッシュ、合計（小計合計・最大リードタイム）。

## 10. 技術的決定事項（まとめ）

| 項目 | 決定 |
|---|---|
| 編集グリッド | AG Grid Community |
| Excel 読込 | SheetJS (`xlsx`) |
| Excel 書込 | `write-excel-file`（既存）/ Phase 3 で検討 |
| 一括取得 | `lookup_parts`（`MisumiCore.lookupMany`、≤100件チャンク、進捗イベント） |
| 永続化 | BomDoc=ローカル JSON、マッピング=app config テンプレ |
| 数量倍率 | `BomDoc.meta.qtyMultiplier`（`発注セット数` 由来） |

## 11. リスク・未決事項

- AG Grid Community のバンドルサイズ増（Tauri なので影響は限定的だが要計測）。
- マッピングテンプレの同定キー（ファイル名 / ヘッダ構成のどちらを優先するか）→ Phase 2 で確定。
- `ORDER` 判定ルール（完全一致 "MISUMI" vs 前方一致/別名）→ 既定は完全一致、設定で拡張。
- 大規模 BOM（数千行）時の一括取得レイテンシ（[07](../misumi-api/07-batch-and-limits.md)）→ チャンク＋進捗で吸収、必要なら上限警告。
- 価格の揮発性 → 永続化は「取得日時付き」。発注の最終確定値は都度再取得を促す。

## 12. スコープ外（当面）

- ログイン（顧客別契約価格）。
- MISUMI への自動発注（カート投入/EDI）。
- 複数 BOM の横断管理 / DB 化（必要になれば SQLite へ）。

## 13. 参考

- MISUMI API 仕様: [`../misumi-api/`](../misumi-api/)（連鎖 02 / 価格 03 / バッチ上限 07 / テスト 08）
- 共有コア: [`../../shared/misumi-lookup.js`](../../shared/misumi-lookup.js)（`window.MisumiCore.lookupMany` を Phase 1 で活用）
