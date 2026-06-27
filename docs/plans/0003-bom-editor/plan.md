# BOM エディタ 詳細計画

関連 Issue: [#3](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/3) ／ 前提: 単一型番ルックアップ（[#1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/1) 完了）

## 1. 目的

部品表（BOM）をアプリ上で編集・管理し、`ORDER`（サプライヤ）に対応する行へ **単価・出荷日・在庫**を自動付与する。
取得はプロバイダ抽象の上に載せ、**Phase 1 は MISUMI を実装**（将来は他 EC へ拡張）。具体的には次の 3 つを満たす：

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
type ColumnKind = "core" | "custom" | "supplier"; // 由来
type ColumnKey =
  | "no" | "partsName" | "partsNo" | "order" | "qty" | "material" // core
  | string; // custom.* / supplier.*

interface ColumnLink {     // サプライヤ連携設定（任意）。設定した列はサプライヤ値で駆動される
  field: string;           // 正規化フィールドのパス。例 "quote.unitPrice" / "product.name"
  write: "overwrite" | "fillEmpty" | "suggest"; // 列ごとに選択（既定 fillEmpty）
}

interface ColumnDef {
  key: ColumnKey;
  label: string;
  kind: ColumnKind;        // 由来: core / custom / supplier(追加列)
  editable: boolean;
  width?: number;
  link?: ColumnLink;       // 設定があればサプライヤリンク列（手動列は undefined）
}

// 各 EC の応答を共通形（正規化 quote）へ写像する。EC 固有データは raw に保持。
interface SupplierQuote {
  supplierCode: string;      // "MISUMI" など
  status: "idle" | "pending" | "ok" | "error";
  currency?: string;         // "JPY"/"USD" 等（多通貨）
  unitPrice?: string;        // 税別
  unitPriceTax?: string;     // 税込
  taxRate?: string;
  shipDate?: string;         // 出荷予定日（MISUMI: vsd）
  leadTimeDays?: number;     // 出荷/納期日数
  stock?: number;            // 即納可能数
  moq?: number;              // 最小注文数
  packQty?: number;          // 入数
  subtotal?: number;         // 単価 × Qty × 倍率
  errors: string[];
  warnings: string[];
  fetchedAt?: string;        // ISO
  raw?: unknown;             // プロバイダ固有の生レスポンス
}

interface BomRow {
  id: string;                // 内部 ID
  no?: number;
  partsName?: string;
  partsNo?: string;          // 型番（サプライヤ依存）
  order?: string;            // 発注先＝サプライヤ（"MISUMI"/"MONOTARO"…）。取得の振り分けに使用
  qty?: number;
  material?: string;
  custom: Record<string, string>;  // 任意列
  supplier?: SupplierQuote;  // 選択サプライヤの取得結果
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
  ├─ 一括取得: ORDER=サプライヤ 行の partsNo を invoke("quote", {supplier, items})
  └─ 永続化/入出力: invoke でデータ層コマンド（保存・読込・検索）／ Excel・JSON は入出力

Rust backend
  ├─ データ層: SQLite（rusqlite）+ マイグレーション。BomRepository で CRUD・検索・キャッシュ
  └─ サプライヤ層: SupplierProvider をディスパッチ
       ├─ MISUMI   : transport=webview（既存ブリッジ + 共有コア / Akamai）
       └─ 他EC(将来): transport=http-api（reqwest, APIキー）など
```

- **Excel 読込**：`xlsx`（SheetJS）。`sheet_to_json({ header: 1 })` で生配列を取り、ヘッダ行 index を指定して列を解釈。
- **書込/エクスポート（Phase 3）**：既存 `write-excel-file` を流用、または `xlsx`/`exceljs` に統一。
- **サプライヤ層（プロバイダ抽象）**：`SupplierProvider`（`resolve`/`quote`）を実装ごとに用意し、`order`（サプライヤ）で振り分ける。各 EC の応答は**正規化 quote**へ写像し、生データは `raw` に保持。MISUMI は既存ブリッジ＋共有コア（`MisumiCore.lookupMany`、**≤100 件チャンク**＋進捗）。**Phase 1 は MISUMI のみ実装**（抽象は先に用意）。詳細は [7](#7-サプライヤ連携プロバイダ抽象--phase1misumi)。
- **永続化（SQLite を system-of-record）**：アプリデータの正本は **SQLite**（`appDataDir` の `*.db`）。`BomDoc` は**メモリ表現／入出力形式**で、保存時は下記スキーマに分解して格納。**JSON は import/export（交換・バックアップ・共有）専用**。Rust に `BomRepository`（**rusqlite** + マイグレーション、WAL、トランザクション）を置き Tauri コマンドで公開（簡易案として `tauri-plugin-sql` も可）。

### 5.1 SQLite スキーマ（概要・system-of-record / サプライヤ汎用）
- `bom(id, name, qty_multiplier, imported_from, created_at, updated_at)`
- `supplier(code, name, transport, currency, …)` — 対応 EC（Phase 1 は MISUMI の 1 行）
- `bom_column(bom_id, key, label, kind, editable, sort_order, link_field, link_write)` — `link_field`=正規化フィールドのパス
- `bom_row(id, bom_id, sort_no, parts_name, parts_no, "order", qty, material, custom_json, supplier_json, updated_at)` — core はカラム、**任意列(custom)とサプライヤ取得結果は JSON1**（`custom_json`/`supplier_json`＝正規化 quote+raw）
- `supplier_cache(supplier_code, parts_no, payload_json, currency, fetched_at)` — **複合キー (supplier_code, parts_no)**・**全 BOM 共有**（再取得判定・レート制限配慮）
- `supplier_price_history(supplier_code, parts_no, unit_price, currency, ship_date, fetched_at)` — 価格/納期の履歴（任意・Phase 3）
- `mapping_template(id, name, kind, config_json)` — 取込/リンクのテンプレ

> **動的列の格納方針**：core を典型カラム、任意列・サプライヤ値は **JSON1** で保持（EAV 比で扱いやすく、必要なら `json_extract` で横断クエリ可能）。EAV は代替案として残す（[11](#11-リスク未決事項)）。

### 5.2 SQLite 採用理由
JSON 単独ではなく **SQLite を正本**とするのは、本アプリが「1 BOM の編集」に留まらず次へ育つため：

- **EC フェッチ結果の全 BOM 横断キャッシュ**：**(サプライヤ, 型番)** ごとの取得結果（単価・名称・出荷日等）を共有保持し、**複数 BOM 間で再利用**（同じ型番を何度も取得しない）。
- **複数 BOM の管理**：一覧・**検索・表示絞り込み**を索引／クエリで効率的に行う。
- **型番起点の情報取得**：型番を入力すると、紐づく情報を **DB キャッシュから即時取得**（高速・オフライン可、必要時のみ再フェッチ）。
- 付随利点：トランザクション（書込中破損なし）・部分更新・マイグレーション・価格/納期の履歴。

JSON は引き続き **import/export（交換・バックアップ・共有）**として残す。

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

## 7. サプライヤ連携（プロバイダ抽象 / Phase1=MISUMI）

将来の多 EC 対応を見据え、取得処理は**プロバイダ抽象**の上に載せる。**Phase 1 は MISUMI のみ実装**し、抽象だけ先に用意する。

### 7.1 プロバイダ抽象（SupplierProvider）
```ts
interface SupplierProvider {
  code: string; name: string;
  caps: { batch: boolean; maxBatch: number; needsAuth: boolean;
          transport: "webview" | "http-api"; currency: string };
  resolve(partNo: string): Promise<ResolvedPart | null>;          // 正規化/suggest
  quote(items: {partNo: string; qty: number}[]): Promise<SupplierQuote[]>; // 価格・納期・在庫
}
```
- 各 EC の応答を**正規化 quote**（`SupplierQuote`：[4](#4-データモデル案)）へ写像し、固有データは `raw` に保持。
- **transport はプロバイダが選ぶ**：MISUMI は `webview`（Akamai のため実ブラウザ経由）、公式 API を持つ EC は `http-api`（Rust/reqwest, API キー）。WebView ブリッジは **MISUMI 固有の実装詳細**でありアーキの前提にしない。
- 認証・レート制限・チャンクサイズ・通貨は `caps`／プロバイダ実装ごと。

### 7.2 取得対象（振り分け）
- 対象：`order`（サプライヤ）が登録プロバイダに一致する行。既定は完全一致（"MISUMI" 等）、設定で別名/前方一致に拡張可。
- バックエンド `quote(supplier, items)` が該当プロバイダへディスパッチ。取得日時を保持し「再取得」で更新。
- **キャッシュ優先**：`supplier_cache`（(supplier,型番) キー・全 BOM 共有）を先に参照し、鮮度切れ/未取得のみ実フェッチ。

### 7.3 フィールド → 列のマッピング（ユーザー設定）
「どのフィールドをどの列に結ぶか」を**固定せずユーザー設定**にする（Excel 取込マッピングと対）。各列は 3 役割：
- **手動(manual)**：サプライヤは書き込まない（REMARKS / FILE NAME / MATERIAL 等）。
- **リンク(linked)**：`ColumnLink.field`（**正規化フィールドのパス**、例 `quote.unitPrice` / `product.name`）で駆動。プロバイダ非依存。
- **追加(added)**：既存列に対応しない取得データを新規列に。「利用可能フィールド一覧」から追加。
- 正規化に無い**プロバイダ固有値**は `raw.*` 参照で追加列にできる。

### 7.4 書込ポリシー（列ごとに選択）
リンク列は `ColumnLink.write` で挙動を選べる。**`fillEmpty` と `overwrite` の両方**に対応：
- `fillEmpty`（既定）：セルが空のときだけ取得値で補完。
- `overwrite`：常に取得値で上書き。
- `suggest`：セルには書かず、取得値を提案表示。
- いずれでも、**ユーザー編集値と取得値が食い違う行はハイライト**し可視化する。

### 7.5 MISUMI プロバイダ実装（Phase 1）
MISUMI は `transport:"webview"`。既存ブリッジ＋共有コア（`suggest`→`sales-price-delivery/check`、`MisumiCore.lookupMany`、≤100 件チャンク）。応答→正規化 quote の写像例：

| 正規化フィールド | MISUMI ソース |
|---|---|
| `product.name` | `seriesName` / `productName` |
| `quote.unitPrice` / `unitPriceTax` | `salesUnitPrice` / `…IncludingTax` |
| `quote.shipDate` | `vsd` |
| `quote.stock` | `immediateShippableQty` |
| `quote.moq` | `minSoQty` |
| `quote.currency` | `JPY`（固定） |
| `raw` | 価格 API レスポンス全体（出荷元・重量・CAD・カテゴリ・`errorMessageList` 等） |

> ⚠ `MATERIAL` は MISUMI API から構造化フィールドとして取得できない → 既定は手動列。**リンク可能な列とそうでない列が実在する**。詳細フィールドは [misumi-api/03](../../misumi-api/03-price-delivery-check.md)/04。

### 7.6 集計
- 小計：`単価(税別) × Qty × qtyMultiplier`。合計・最大リードタイム（出荷見込み）は Phase 3。

リンク設定（`ColumnLink`）も `BomDoc.columns` に保持し、**テンプレ保存**で再利用する。

## 8. グリッド選定：AG Grid Community

| 候補 | 評価 |
|---|---|
| **AG Grid Community（採用）** | 動的列・インライン編集・キーボード操作・コピペが標準、無料(MIT)。BOM 編集を最短で実装可。バンドルはやや大きめ。 |
| TanStack Table | ヘッドレスで軽量だが編集 UI/キーボード操作を自前実装（工数増）。 |
| Handsontable | スプレッドシート体験は最良だが非評価利用は商用ライセンス要。 |

→ 動的列＋インライン編集＋追加行/列という要件に対し、実装速度と機能のバランスで **AG Grid Community** を採用。

## 9. フェーズ計画と受け入れ条件

### Phase 1 — データ層 ＋ 編集表 ＋ 一括取得
- **SQLite データ層**：スキーマ＋マイグレーション＋`BomRepository`（rusqlite, WAL, トランザクション）。
- データモデル（`BomDoc`）＋ AG Grid 表示・編集。行の追加/複製/削除、任意列の追加/改名/削除。
- BOM の保存/読込（正本は SQLite、JSON で import/export）。
- **サプライヤ抽象（SupplierProvider）＋ MISUMI 実装 1 本**：`ORDER=MISUMI` 行を一括取得 → 単価/出荷日/在庫/エラーを反映（進捗表示）。バックエンド `quote(supplier, items)`（チャンク＋進捗）。
- **横断キャッシュ**：取得結果を `supplier_cache`（**(supplier,型番) キー**・全 BOM 共有）へ保存。型番入力時はまずキャッシュ参照（高速・再フェッチ抑制、取得日時で鮮度判定）。
- **受け入れ**：手入力 BOM で MISUMI 行に単価・出荷日が付き、行レベルエラーも表示。保存→再読込で復元。**別 BOM で同一型番がキャッシュから即時表示**される。

### Phase 2 — Excel 取込
- シート＋ヘッダ行＋列マッピング＋プレビュー＋取込。
- マッピングテンプレ保存／再利用。数量倍率パラメータの取り込み。
- **受け入れ**：様式の異なる代表的な BOM Excel（ヘッダ行が先頭でない／列構成が異なる／発注先列が無く全行 MISUMI 等）を取り込め、MISUMI 行の一括取得まで通る。

### Phase 3 — 複数 BOM ・履歴・仕上げ
- **複数 BOM ライブラリ**：一覧・検索・表示絞り込み（SQLite クエリ／索引）。
- **価格/納期の履歴**（`supplier_price_history`）と横断分析。
- Excel エクスポート、全行再取得、合計（小計合計・最大リードタイム）。

> **追加 EC（将来）**：`SupplierProvider` を実装し `supplier` に 1 行登録するだけで対応（公式 API 系は `http-api`/reqwest）。スキーマ・UI・キャッシュは既にサプライヤ汎用のため改修最小。

## 10. 技術的決定事項（まとめ）

| 項目 | 決定 |
|---|---|
| 編集グリッド | AG Grid Community |
| Excel 読込 | SheetJS (`xlsx`) |
| Excel 書込 | `write-excel-file`（既存）/ Phase 3 で検討 |
| サプライヤ抽象 | `SupplierProvider`（`resolve`/`quote`、transport `webview`/`http-api`、正規化 quote+raw）。**Phase1=MISUMI のみ実装** |
| 一括取得 | `quote(supplier, items)` でプロバイダにディスパッチ。MISUMI=`MisumiCore.lookupMany`、≤100件チャンク、進捗イベント |
| 連携マッピング | `ColumnLink`（`field`=正規化フィールドパス + `write: overwrite/fillEmpty/suggest`）。列ごとに 手動/リンク/追加、差異ハイライト、テンプレ保存 |
| 永続化 | **SQLite を正(system-of-record)**：rusqlite + マイグレーション（WAL/トランザクション）。任意列/サプライヤ値は JSON1。**JSON は import/export 専用**。テンプレ=`mapping_template` |
| サプライヤキャッシュ | `supplier_cache`（**(supplier,型番) キー**・全 BOM 共有）。型番入力時はキャッシュ優先、取得日時/TTL で再フェッチ判定 |
| 数量倍率 | `BomDoc.meta.qtyMultiplier`（シートパラメータ由来） |

## 11. リスク・未決事項

- AG Grid Community のバンドルサイズ増（Tauri なので影響は限定的だが要計測）。
- マッピングテンプレの同定キー（ファイル名 / ヘッダ構成のどちらを優先するか）→ Phase 2 で確定。
- `ORDER` 判定ルール（完全一致 "MISUMI" vs 前方一致/別名）→ 既定は完全一致、設定で拡張。
- 大規模 BOM（数千行）時の一括取得レイテンシ（[07](../../misumi-api/07-batch-and-limits.md)）→ チャンク＋進捗で吸収、必要なら上限警告。
- 価格の揮発性 → キャッシュは「取得日時付き」。即時表示はキャッシュ、確定値は再取得を促す（`supplier_cache` の TTL/鮮度判定）。
- **スキーマのマイグレーション規律**（バージョン管理必須。rusqlite + version テーブル or refinery 等）。
- **動的列の格納方式**（JSON1 を既定、EAV を代替）。横断クエリ要件が増えたら一部を正規化列へ昇格。
- **多 EC のプロバイダ差異**：認証/APIキー（`http-api` 系）、レート制限、**多通貨**、数量階梯価格（数量で単価が変わる EC ではキャッシュは代表値＋取得日時、確定はqty指定で再取得）。
- **同一型番のキャッシュ粒度**：キーは (supplier,型番)。数量依存価格は別途 qty を考慮（キー拡張 or 価格ブレイク保持）→ Phase 1 で確定。

## 12. スコープ外（当面）

- ログイン（顧客別契約価格）。
- 各 EC への自動発注（カート投入/EDI/PunchOut）。
- 複数 BOM の高度な分析・レポーティング（基本の管理/検索は SQLite で対応、高度分析は将来）。
- **コンポーネント同一性**（別 EC 間で同じ部品を SKU 横断で束ねる）→ 将来。Phase 1 は行＝単一サプライヤ。

## 13. 参考

- MISUMI API 仕様: [`../../misumi-api/`](../../misumi-api/)（連鎖 02 / 価格 03 / バッチ上限 07 / テスト 08）
- 共有コア: [`../../../shared/misumi-lookup.js`](../../../shared/misumi-lookup.js)（`window.MisumiCore.lookupMany` を Phase 1 で活用）
