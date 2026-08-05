# Excel リンクモード 実装計画（§7 ステップ4〜9 の具体化）

- 親文書: [plan.md](./plan.md)（合意済み設計。**設計判断と実測根拠はすべて plan.md が正本**であり、
  本書は §番号で参照して**実装の具体のみ**を規定する。両文書が矛盾したら plan.md を優先し本書を直す）
- 前提: 技術検証ステップ1〜3c 成立（3c は **GO**・plan.md §3.12・[3c-results.md](./3c-results.md)）。
  3b の symlink 実測のみ環境待ち（fail closed で書き戻し禁止のまま・§3.11）
- スコープ: plan.md §7 の実装ステップ **4〜9** を PR 系列に落とす。src-tauri / src(フロント) を初めて変更する
- 本書自体はドキュメントのみの計画 PR として合意してから実装に入る

## 0. 本書で決着させる未決事項

| 事項 | 決定 | 根拠・補足 |
|---|---|---|
| §6.2.1 EC 取得失敗時のアプリ所有列既存値 | **残して警告**（暫定方針を正式化） | ユーザー決定（2026-08-05）。既存値を残し、アプリ UI で「前回値・手動値の可能性あり」を行単位に警告。古い価格が残るリスクは警告表示で緩和。plan.md §6.2.1 を【決定済み】に更新 |
| CI | **GitHub Actions を導入**（PR-0） | ユーザー決定（2026-08-05）。恒久回帰テスト（§7.1.1）の価値を PR ごとに自動化 |
| backup 保持ポリシー（§4.2.2「実装時に決定」） | **最新5件は必ず残し、かつ30日以内のものも残す。削除条件 = 同一 BOM 内で新しい順の順位 > 5 **AND** 経過 > 30日** | 台帳（`bom_link_backup`）で管理。**削除対象外**: 未解決競合（`is_conflict=1 AND resolved_at IS NULL`）・`volume_temp`（移送途中）。ファイル削除に**成功したときだけ** `deleted_at` を記録し、失敗時は台帳・ファイルとも残して警告。台帳行は削除後も残す（監査可能性） |
| デバウンス幅（§4.2.3/§7.2） | **PR-7 実装時に実測で決定**（初期値 500ms から調整） | Excel 保存のイベント発火回数は環境依存のため計画では固定しない |

## 1. V5 マイグレーション（ステップ4）

### 1.1 設計原則

1. **リンクメタは `bom_column`/`bom_row` と完全分離する。** `save_bom` は両テーブルを
   DELETE→全 INSERT する full-replace（db.rs:241-289）。リンクメタが同居すると保存のたびに消えるため、
   全テーブルを新設し、`bom_column` へは **FK を張らない**（トランザクション内 DELETE の CASCADE
   巻き添え防止。`app_key` は `bom_column.key` の論理参照）。リンク BOM の `bom_column`/`bom_row` は
   読込成功のたびに契約＋Excel 現在行＋採用スナップショット（`bom_link_quote`・§2.3）から再生成される
   **表示キャッシュ**（§4.9。共有 `supplier_cache` は取得最適化専用でリンク BOM の表示・書き戻しの正本にしない）
2. **契約（宣言）と状態（揮発）を別テーブルにする。** 再マッピングで契約（`bom_link`＋`bom_link_column`）を
   書き直しても、計算状態・指紋（`bom_link_state`）が巻き添えで消えない
3. **backup 台帳を DB に持つ。** §4.2.2 の要求 —(a) 競合 backup はユーザー解決まで自動削除禁止、
   (b) 6段階移送の途中失敗は「元 backup を残して警告」（クラッシュ横断で記憶が必要）、
   (c) 保持ポリシー適用 — はファイルシステム走査では「なぜ残っているか」を復元できない
4. **指紋は algo＋値の対**（§4.6.1: `sha256-v1`）。内容指紋3本は同一 algo 列を共有し、
   構造フィンガープリント（§4.7・内容指紋とは別物）は `structure-v1` を別途持つ

### 1.2 DDL（db.rs `MIGRATIONS` に V5 として append）

```sql
-- V5: Excel link mode structure contract + calc state + pending + backup ledger
-- (docs/plans/0018-excel-link-mode/plan.md §4.7 / §4.4.2 / §4.2.1 / §4.2.2 / §4.10)

-- 1:1 リンク本体 = 契約ヘッダ + 環境判定。行の存在 = 「この BOM はリンク BOM」。
-- bom_link_* は save_bom の full-replace (bom_column/bom_row DELETE→INSERT) の外に置く。
CREATE TABLE bom_link (
  bom_id            TEXT PRIMARY KEY REFERENCES bom(id) ON DELETE CASCADE,
  workbook_path     TEXT NOT NULL,      -- ユーザーが選んだパス = MVP の同一性根拠 (§3.7/§4.7)
  sheet_name        TEXT NOT NULL,      -- 対象シート名 (表示名。r:id 解決は読込時に実施)
  header_row        INTEGER NOT NULL,   -- 1-based Excel 行番号
  data_start_row    INTEGER NOT NULL,   -- 1-based
  env_verdict       TEXT NOT NULL CHECK (env_verdict IN ('allow','no_writeback')),
                                        -- §4.10: NTFS実体=allow / 解決不能・仮想FS=no_writeback(読取専用降格)
  env_resolved_path TEXT,               -- 解決済み実体パス (診断表示用。同一性根拠にはしない)
  env_fs_name       TEXT,               -- 実体ボリュームの FS 名 (例 'NTFS','FAT32')
  env_checked_at    TEXT,
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
);

-- 列マッピング = 構造契約の本体 (§4.7)。Excel 列位置ごとに1行。読み飛ばし列も記録する。
-- bom_link(bom_id) を参照: リンク解除 (bom_link 行の DELETE) で契約・状態・pending が
-- cascade 削除され、孤児化・再リンク時の PK 衝突や古い状態の再利用を構造的に防ぐ。
-- app_key は bom_column.key の論理参照 (FK は張らない: save_bom の full-replace と衝突するため。
-- リンク BOM の bom_column は読込時に本契約から再生成される表示キャッシュであり、契約が正)。
CREATE TABLE bom_link_column (
  bom_id       TEXT NOT NULL REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  excel_col    INTEGER NOT NULL,        -- 0-based 列 index (calamine 座標系。A1 変換は xlsx 層)
  header_label TEXT,                    -- 最後に確認したヘッダ表示名 (§4.7: 恒久識別子にしない)
  app_key      TEXT,                    -- アプリ列キー。NULL = 読み飛ばし列
  ownership    TEXT NOT NULL CHECK (ownership IN ('app','user','skipped')),  -- §4.3 二分 + 読み飛ばし
  required     INTEGER NOT NULL DEFAULT 0,   -- 必須列 (§4.9 破綻判定の入力)
  role         TEXT,                    -- 'partNo'/'source' 等。契約として自己完結させる (§4.7)
  source_field TEXT,                    -- EC 項目 (unitPrice/shipDate/subtotal/product.name/…)
  projection   TEXT CHECK (projection IN ('writeback','suggest')),
                                        -- writeback = Excel へ書き戻す (app 所有列のみ)
                                        -- suggest   = アプリ内表示のみの提案 (user 所有列。Excel へ書かない)
  PRIMARY KEY (bom_id, excel_col),
  -- 3状態の排他的列挙。SQLite の CHECK は式が NULL だと通過するため、NULL を含む個別 OR 条件では
  -- 'app' 列の source_field/projection 欠損を拒否できない。有効な組合せを列挙し、それ以外を全て拒否する。
  CHECK (
       (ownership = 'app'     AND app_key IS NOT NULL AND source_field IS NOT NULL
                              AND projection = 'writeback')
    OR (ownership = 'user'    AND app_key IS NOT NULL
                              AND ( (source_field IS NULL     AND projection IS NULL)
                                 OR (source_field IS NOT NULL AND projection = 'suggest') ))
    OR (ownership = 'skipped' AND app_key IS NULL AND source_field IS NULL
                              AND projection IS NULL AND required = 0)
  )
);
CREATE UNIQUE INDEX idx_bom_link_column_key
  ON bom_link_column(bom_id, app_key) WHERE app_key IS NOT NULL;

-- 揮発状態: 読み書きループが毎回更新する側。契約の書き直し(再マッピング)から独立。
-- ワークブック単位の粒度で足りる (§4.4.2: MVP は全数式一括 stale)。
CREATE TABLE bom_link_state (
  bom_id               TEXT PRIMARY KEY REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  sync_status          TEXT NOT NULL DEFAULT 'linked'
                         CHECK (sync_status IN ('linked','needs_review','broken','conflict')),
  sync_error           TEXT,            -- 直近のエラー/停止理由 (人間可読 + 機械判別コード prefix)
  calc_state           TEXT NOT NULL DEFAULT 'unverified'
                         CHECK (calc_state IN ('unverified','trusted','stale','missing')),  -- §4.4
  recalc_requested     INTEGER NOT NULL DEFAULT 0,  -- fullCalcOnLoad 付与済みか (§4.4.2)
  fp_algo              TEXT NOT NULL DEFAULT 'sha256-v1',  -- 下記3指紋の共通アルゴリズム (§4.6.1)
  last_read_fp         TEXT,            -- 最終読込時の指紋 = §4.6.2 書き込み前照合の基準
  last_read_at         TEXT,
  last_app_write_fp    TEXT,            -- アプリが書き込んだ直後の指紋 = stale 復元基準 (§4.4.2)
  last_app_write_at    TEXT,
  last_verified_read_fp TEXT,           -- 最後に「Excel保存後の読込」を確認した指紋 (§4.7)
  structure_fp         TEXT,            -- 構造フィンガープリント (§4.7。内容指紋とは別)
  structure_fp_algo    TEXT NOT NULL DEFAULT 'structure-v1',
  data_first_row       INTEGER,         -- 最後に確認したデータ範囲 (§4.7)
  data_last_row        INTEGER,
  row_count            INTEGER,
  ec_generation        INTEGER NOT NULL DEFAULT 0,  -- EC 取得世代カウンタ (発行元。§2.3)
  applied_generation   INTEGER NOT NULL DEFAULT 0   -- Excel へ反映済みの世代
);

-- BOM 単位の採用スナップショット。supplier_cache は (supplier_code, parts_no) キーの全 BOM 共有で
-- 「取得の最適化」に限定し、この BOM が採用した EC 値の正本はここに置く。
-- これにより ec_generation が指す DB 状態が BOM 境界で閉じる (他 BOM の再取得が共有 cache を
-- 更新しても、この BOM のスナップショット・世代は変わらない)。reader/writeback はここを参照する。
CREATE TABLE bom_link_quote (
  bom_id        TEXT NOT NULL REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  supplier_code TEXT NOT NULL,
  parts_no      TEXT NOT NULL,
  payload_json  TEXT NOT NULL,     -- 採用時点の SupplierQuote (supplier_cache と同形)
  currency      TEXT,
  fetched_at    TEXT NOT NULL,     -- 元データの取得時刻
  generation    INTEGER NOT NULL,  -- この行を採用した世代 (bom_link_state.ec_generation)
  PRIMARY KEY (bom_id, supplier_code, parts_no)
);

-- 反映待ち (§4.2.1)。latest-wins のため BOM につき最大1行 (UPSERT)。セル値は持たない。
CREATE TABLE bom_link_pending (
  bom_id               TEXT PRIMARY KEY REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  requested_generation INTEGER NOT NULL,  -- 要求時点の ec_generation (「EC取得世代と対象BOM」のみ)
  requested_at         TEXT NOT NULL,
  last_attempt_at      TEXT,
  attempt_count        INTEGER NOT NULL DEFAULT 0,
  blocked_reason       TEXT               -- 'file_open'/'permission'/'env'/... 直近の失敗分類
);

-- バックアップ台帳 (§4.2.2 バックアップ運用)。競合版はユーザーの外部版の唯一の退避先であり
-- BOM 削除後も追跡が必要なため、FK は SET NULL (行は残す)。
CREATE TABLE bom_link_backup (
  id             INTEGER PRIMARY KEY AUTOINCREMENT,
  bom_id         TEXT REFERENCES bom(id) ON DELETE SET NULL,  -- 生存中の JOIN 用 (削除で NULL)
  origin_bom_id  TEXT NOT NULL,        -- 不変の発生元 BOM ID。保持順位 (PARTITION BY) はこちらで計算する
                                       -- (bom_id は削除で NULL になり、複数の削除済み BOM の backup が
                                       --  同一集合に混ざって順位付けが壊れるため。workbook_path は移動・
                                       --  再利用があり安定識別子にしない)
  workbook_path  TEXT NOT NULL,        -- どのファイルの置換だったか (bom_id 消失後の文脈)
  backup_path    TEXT NOT NULL,        -- 現在のファイル所在
  location       TEXT NOT NULL CHECK (location IN ('volume_temp','app_data')),
                                       -- 6段階移送の進行位置: 同一ボリューム一時置き / 移送完了
  fp_algo        TEXT NOT NULL DEFAULT 'sha256-v1',
  backup_fp      TEXT NOT NULL,        -- backup の内容指紋 (移送時 SHA-256 照合にも使用)
  f0_fp          TEXT NOT NULL,        -- 書き込みの基になった内容の指紋 F0 (§4.2.2 手順2)
  is_conflict    INTEGER NOT NULL DEFAULT 0,  -- backup_fp != f0_fp (§4.2.2 手順8)
  created_at     TEXT NOT NULL,
  transferred_at TEXT,                 -- app_data への移送完了時刻 (NULL = 元 backup が残置)
  resolved_at    TEXT,                 -- 競合をユーザーが解決した時刻
  deleted_at     TEXT                  -- 保持ポリシーによるファイル削除時刻 (台帳行は残す)
);
CREATE INDEX idx_bom_link_backup_bom ON bom_link_backup(origin_bom_id, created_at);
CREATE INDEX idx_bom_link_backup_open_conflict
  ON bom_link_backup(is_conflict) WHERE is_conflict = 1 AND resolved_at IS NULL;
```

### 1.3 スキーマ設計の補足判断

- **`sync_status` の遷移**（§4.9 3判定表＋§4.2.2 競合検出に対応）:

  | 値 | 意味 | 入る契機 | 出る契機 |
  |---|---|---|---|
  | `linked` | 正常同期 | リンク作成 / Safe 判定の自動取り込み | — |
  | `needs_review` | 要確認。同期停止・候補提示 | Confirm 判定（列移動・ヘッダ名変更・シート名変更・ヘッダ行移動） | ユーザーが候補確定 or 再マッピング → `linked` |
  | `broken` | 破綻。取り込み・書き込み禁止 | Broken 判定（必須列消失等）/ `workbook_path` 解決不能（移動・改名 §4.7） | 再マッピング / 再リンク → `linked` |
  | `conflict` | 置換後に backup≠F0（§4.2.2）。同期停止・自動復元しない | 手順8の指紋不一致 | ユーザーが競合解決（`resolved_at`）→ `linked` |

- **環境降格（§4.10）は `sync_status` に含めない**: 構造の健全性と直交（読取・EC 取得は継続可）。
  `bom_link.env_verdict='no_writeback'` で永続化 = §6.2 の退避経路（読み取り専用降格）の実装点
- **「反映待ち」は状態値ではなく `bom_link_pending` 行の存在**で表現（latest-wins の UPSERT 対象を1行に限定）
- **EC 取得世代**: BOM ごとの単調増加整数 `ec_generation`。発行経路は §2.3「quote との接続」を参照
  （既存 `quote` コマンドの拡張）。「Excel へ反映」要求時に現在値を
  `bom_link_pending.requested_generation` へ UPSERT（古い要求は上書き = latest-wins）。
  書き込み成功時に基になった世代を `applied_generation` に記録し、`requested <= applied` なら pending 削除
- **`link_field`/`link_write` の読み替え**（§4.3「fillEmpty/overwrite は継続同期で不成立」の帰結）:
  リンク作成ウィザードで既存の `overwrite` リンク列（実在例 `partsName ← product.name`・§3.3）を
  「**app 所有列として宣言**（`ownership='app', projection='writeback', source_field=…`）」か
  「**suggest に降格**（`ownership='user', projection='suggest', source_field=…` — アプリ内表示のみ・
  Excel へは書かない）」の二択で選ばせ、リンク BOM の `bom_column.link_field/link_write` は NULL に
  正規化する。suggest の対応関係（どの EC 項目を提案するか）は契約の `source_field` が保持する
- **リンク解除（`excel_link_unlink`）は1トランザクション**で行う:
  最新の安全なスナップショット（bom_column/bom_row）を確定 → `bom_link` 行を DELETE →
  契約・状態・pending・採用スナップショット（`bom_link_quote`）が FK cascade で消える。
  backup 台帳（`bom(id)` 参照・SET NULL、不変の `origin_bom_id` を保持）だけが履歴として残る。
  再リンク時は必ず素の状態から開始される（古い指紋・世代・pending を引き継がない）

## 2. src-tauri モジュール構成と PoC 移植（ステップ4〜9 共通の骨格）

### 2.1 新モジュール `src-tauri/src/excel_link/`

```
excel_link/
  mod.rs          オーケストレーション: open/refresh(読込→構造検証→復元規則→合成)、
                  writeback パイプライン(§4.2.2 手順1〜9 を一関数に直列化)
  fingerprint.rs  ファイル全体 SHA-256 (§4.6.1)。読取中の共有違反 → 不安定スナップショット扱いで再試行
  calc_state.rs   CalcState 状態機械・restore()・A1 パーサ・write_blocked (PoC state.rs)
  env.rs          §4.10 環境判定: 実体解決(canonicalize + .lnk 追跡) + FS 名取得 + link_allowed。
                  canonicalize の前にパス全構成要素(親ディレクトリ含む)の reparse tag を検査し、
                  IO_REPARSE_TAG_SYMLINK 検出時は no_writeback へ降格(3b 実測完了まで fail closed。
                  canonicalize 後は「symlink 経由だった」情報が失われるため事前検査が必須)。
                  junction(IO_REPARSE_TAG_MOUNT_POINT)は 3b 実測済みのため許可 — tag を区別する
  xlsx.rs         zip 直編集 RMW: raw copy / sheet_parts(r:id) / patch_cell / patch_calc_pr /
                  spill_ranges / cell_has_formula / XML ウォーカー(cells + sharedStrings + inlineStr)
  contract.rs     構造契約型(V5 テーブル ⇔ Rust 型)・verify_structure(key ベース版)・構造 fp 計算
  reader.rs       calamine worksheet_range + worksheet_formula 併読(§3.4.1 座標オフセット)、
                  契約 + Excel 現在行 + bom_link_quote(採用スナップショット) → BomDoc 合成(§4.9 再構築)
  writeback.rs    F0 規律・行単位再計算(§4.5)・スピル交差ガード・temp 生成・
                  ReplaceFileW(backup 付き原子的置換)・backup 指紋照合
  backup.rs       backup 一意命名・6段階移送(copy→SHA256照合→rename→元削除)・保持ポリシー(§0)
  store.rs        V5 テーブルの rusqlite クエリ(&Connection 受け・rusqlite::Result。
                  マイグレーション定数自体は db.rs に append)
  watch.rs        notify ベース監視: 親ディレクトリ監視・デバウンス・~$ 除外・リトライ・イベント発火
```

- 追加クレート: `sha2`（指紋）・`zip`（RMW）・`notify`（監視・PR-7）・
  `windows-sys`（`ReplaceFileW`/`GetVolumeInformationW`。std::fs に backup 付き置換は無い）。
  `.lnk` 解決に COM が要る場合は `windows` クレート
- `spreadsheet.rs`（calamine 読込・rust_xlsxwriter 書き出し）は**従来 BOM 専用として不変**（§9-26 回帰なし）

### 2.2 PoC 移植マップ（tools/excel-link-poc/src/ → excel_link/）

| PoC | 移植先 | 変更点 |
|---|---|---|
| state.rs: `CalcState`/`restore`/`Persisted`/`parse_cell`/`parse_range`/`write_blocked`＋15テスト | calc_state.rs | **ほぼ verbatim**。`Persisted` の供給元を `bom_link_state` に接続 |
| state.rs: `LinkDecision`/`link_allowed` | env.rs | 純関数そのまま。実体解決（canonicalize・.lnk・FS 名取得）を追加 |
| structure.rs: `Verdict`/`SheetObs`/異常収集/`finalize`（Broken>Confirm>Safe・fail closed）＋19テスト | contract.rs | **`Contract` は key ベースへ再設計**（§3.11: PoC の label 連続列前提は移設不可。`ColumnContract{app_key, excel_col, header_label, ownership, required, role, source_field, projection}`）。判定骨格とテスト意図は維持し、fixture を key ベースへ書き換え |
| main.rs: `fingerprint()` | fingerprint.rs | ＋共有違反時の再試行（§4.6.1） |
| main.rs: `read_all_bytes`/`sheet_parts`/`sheet_part_for`/`patch_calc_pr`/raw copy ループ | xlsx.rs | ほぼそのまま（バイト保全は §3.9 実測済み） |
| main.rs: `patch_cell` | xlsx.rs | **PoC の穴を塞ぐ**: ①対象セル不在時の `<c>` 挿入 ②文字列値の書き込み ③属性順が異なる writer への耐性（対象セル近傍のみ寛容パース。他パートはバイトコピー維持） |
| main.rs: `spill_ranges`/`cell_has_formula` | xlsx.rs | そのまま（fail closed は write_blocked 側で担保） |
| stale_scan.rs: XML ウォーカー（cells/shared_strings） | xlsx.rs | ＋**inlineStr 対応**（§3.11: PoC はヘッダ検出が sharedStrings のみ） |
| sync.rs ほか（実同期ハーネス・PS 群） | 移植しない | 調査用。3c 完了で役目終了（証跡は 3c-results.md） |

### 2.3 Tauri コマンド面

既存規約（snake_case `<領域>_<動作>`・lib.rs にフラット・`generate_handler!` 登録・
フロントは `src/api/bom.ts` に camelCase ラッパ）に従い、**9コマンド**を追加する:

| コマンド | 役割 | 戻り値 |
|---|---|---|
| `excel_link_probe(path)` | リンクウィザード用の下見（読み取りのみ）: シート一覧・ヘッダ候補・環境判定 | `LinkProbe` |
| `excel_link_create(bom_id?, config)` | リンク作成。環境判定→契約保存→初回読込 | `LinkedBomView` |
| `excel_link_unlink(bom_id)` | リンク解除。スナップショットを残し従来 BOM 化 | `()` |
| `excel_link_open(bom_id)` | 読込＋構造検証＋復元規則＋合成。初回表示・「更新」・自動再読込の共通入口 | `LinkedBomView` |
| `excel_link_apply(bom_id)` | 「Excel へ反映」= §4.2.2 手順1〜9。手動再試行も同コマンド | `ApplyOutcome` |
| `excel_link_status(bom_id)` | 軽量ステータス（sync_status・calc_state・pending・未解決競合） | `LinkStatus` |
| `excel_link_confirm(bom_id, resolution)` | Confirm 判定の候補確定・再マッピング | `LinkedBomView` |
| `excel_link_resolve_conflict(bom_id, backup_id, action)` | 競合の解決記録・フォルダを開く導線 | `()` |
| `excel_link_watch(bom_id, enable)` | 監視の開始/停止。検知は `emit("excel-link:changed")` → フロントが `excel_link_open` | `()` |

- 「Excel で編集」はファイル起動のみ（§4.2）→ 既存 `tauri-plugin-opener` をフロント直用、専用コマンドなし

**quote との接続（`ec_generation` の発行経路と BOM 採用スナップショット）** — 現行 `quote` は
`bom_id` を受け取らず、`supplier_cache` は全 BOM 共有・**チャンクごとに commit** される。
そのため「世代が指す DB 状態」を BOM 境界で閉じるには、共有 cache を取得最適化に限定し、
**BOM 単位の採用スナップショット（`bom_link_quote`・§1.2）を正本にする**:

- `quote` に **省略可能な `bom_id: Option<String>`** を追加（従来 BOM・非リンク呼び出しは None で従来挙動。
  新コマンドは作らない — 取得フロー・進捗イベント・共有 cache 書き込みは従来 BOM と共通のため）
- **reader/writeback は共有 cache ではなく `bom_link_quote` を参照する**。他 BOM の再取得で共有 cache が
  変わっても、この BOM のスナップショット・世代・pending の整合は崩れない
- 世代の確定規則: ネットワーク応答は DB 外で収集し、コマンド完了時に**成功結果のスナップショット
  UPSERT と `ec_generation + 1` を同一 DB トランザクションで一括 commit** する。
  進める条件は「**スナップショットが実際に変わった**」こと — 取得成功だけでなく、
  **cache hit でも BOM が初採用 or 前回スナップショットと値が変わった場合は新世代**。
  変化ゼロ（全件失敗・全件同値）は世代を進めない。共有 cache のチャンク commit は従来どおりで良い
  （最適化に過ぎず、途中失敗で共有 cache が進んでもこの BOM の世代整合には影響しない）
- 部分成功は**構造化された部分成功**として返し、失敗行はスナップショット据え置き（＝§0 の
  「前回値を残し警告」に接続）。Excel へは成功行のみ書く
- 戻り値: `QuoteOutcome { results: Vec<SupplierQuote>, generation: Option<i64>, failed: Vec<…> }`。
  現行の `Promise<SupplierQuote[]>`（src/api/bom.ts）と App.tsx の呼び出しも **PR-4 で同時に更新**し、
  従来 BOM 経路は §9-26 の回帰テスト対象に含める
- 実装・テストは **PR-4**（同型番複数行〔§9-18〕・部分失敗の世代/警告接続・cache hit 初採用で世代が
  進むこと・他 BOM の取得でこの BOM の世代が進まないこと）。latest-wins〔§9-17〕は **PR-5**、
  失敗行の「前回値・手動値の可能性あり」UI 表示は **PR-6**

- **3判定・反映結果は `Result<T,String>` の Err に落とさず成功系 enum で返す**（Err は I/O・DB 障害のみ）:

```rust
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StructureVerdict {
    Safe    { new_columns: Vec<String> },               // 自動取り込み済み (§4.9)
    Confirm { reasons: Vec<String>, candidates: Vec<LinkResolutionCandidate> },
    Broken  { reasons: Vec<String> },                   // 取り込み・書き込み禁止
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ApplyOutcome {
    Applied  { generation: i64, fingerprint: String },
    Pending  { reason: String },      // "fileOpen" 等 → bom_link_pending 記録済み (§4.2.1)
    Refused  { reason: RefuseReason },// 構造NG/指紋変化/スピル交差/5000行超/env降格 — 書かずに終了
    Conflict { backup_id: i64, backup_path: String },  // backup≠F0 (§4.2.2)。同期停止済み
}
```

`LinkedBomView` は `doc: BomDoc`（合成済み。Broken 時は前回スナップショット）＋ `verdict`＋
`calc_state`＋`sync_status`＋`env_verdict`＋`pending`＋`formula_cells`（fx 表示用）。

### 2.4 ファイル監視の選択（PR-7）

**`notify` クレート直用**を採用する。`tauri-plugin-fs` の watch はフロント JS 向けで監視パスを
ACL スコープに事前宣言する前提だが、リンク対象はダイアログで選ぶ任意パスでスコープ設計と相性が悪い。
§4.2.3 の要件（親ディレクトリ監視・`~$` 除外・デバウンス・読取失敗リトライ・「安全判定が Safe の時だけ
自動適用」）はすべてバックエンド側ロジックであり、生イベントを Rust で判断してから合成イベントを emit する。
監視は利便性トリガに徹し、整合性は指紋照合と読込時検証が担う（§4.2.3 責任分界）。

## 3. PR 分割と DoD

各 PR は「cargo test / fmt / clippy green ＋ 該当する §9 受け入れ条件の手動確認記録」を DoD とする。
§9 の全26項を漏れなくどこかの PR に割り当てる（右列）。

| PR | 内容 | ステップ | §9 受け入れ条件 |
|---|---|---|---|
| **PR-0** | **CI 導入**: GitHub Actions で cargo test/fmt/clippy（src-tauri）＋ tsc --noEmit（フロント）。Windows ランナー。**既存 clippy 警告3件（lib.rs:363 `useless_conversion`・lib.rs:483/502 `needless_borrows_for_generic_args`）のベースライン修正を同梱**し、`rust-toolchain.toml` で検証済み toolchain を固定（`-A` での握り潰しはしない） | — | — |
| **PR-1** | **V5 マイグレーション**＋`excel_link/store.rs`＋model 追加。スキーマとクエリのみで挙動変更なし。V4→V5 適用テスト・ロールバック不可の確認・**`bom_link_column` の3状態 CHECK の有効/無効組合せテスト**（app/user/suggest/skipped の valid 各形＋NULL 混在の invalid 形が確実に拒否されること — SQLite の CHECK は NULL で通過するため列挙式で担保） | 4 | 26 |
| **PR-2** | **純ロジック移植**: calc_state.rs / fingerprint.rs / env.rs（PoC テスト 15本＋α を移植） | 4.5 | （12・14 の基盤。単体テストのみ） |
| **PR-3** | **読み取り専用リンク成立**: xlsx.rs（ウォーカー＋inlineStr）・reader.rs・contract.rs（key ベース3判定・19テスト再構成）→ `probe/create/open/unlink`（unlink は §1.3 の1トランザクション規則）。§7.1.1 **読み取り系・座標系4項目**の恒久回帰テスト同梱 | 5 | 1, 2, 8, 11, **14**（junction/.lnk/直接パスの実装＋**未検証経路〔symlink 含む〕の fail closed まで** — canonicalize 前の reparse tag 検査で symlink を no_writeback へ降格〔§2.1 env.rs〕。symlink の実測は §6 の別ゲート）, 19, 20, 21 |
| **PR-4** | **書き戻し**: writeback.rs・backup.rs（6段階移送・§0 の削除述語・保持順位は `origin_bom_id` で計算）→ `apply`。**`quote` の `bom_id` 拡張＋採用スナップショット（`bom_link_quote`）＋世代確定規則（§2.3。スナップショット UPSERT と世代 +1 を同一トランザクション）**。フロント `quote` 呼び出し（src/api/bom.ts・App.tsx）の戻り値型更新と従来 BOM 経路の回帰テスト。patch_cell の穴埋め（セル挿入・文字列値・属性順耐性）。§7.1.1 **競合検出6項目**の恒久回帰テスト＋同型番複数行・部分失敗・cache hit 初採用・他 BOM 取得非干渉の世代テスト同梱 | 6 | 4, **12**（再起動後の stale 維持 = 書き込み後の復元規則）, 13, 15, 16, 18, 22, 23, 24 |
| **PR-5** | **反映待ち・外部変更検知**: pending（latest-wins）→ `status/confirm/resolve_conflict`。**§9-5 は手動経路まで**（Excel を閉じた後の**手動**再試行で反映が成立。「閉じたことの自動検知→自動反映」は PR-7 で完成） | 7 | **5（手動経路）**, 17, 25 |
| **PR-6** | **リンクモード UI**: 読み取り専用化（columns.ts editable・Toolbar 行操作・ColumnManager）・fx/stale/unverified 表示・「Excel で編集」「更新」「Excel へ反映」導線・反映待ち/競合表示・**同時編集非対応警告・版履歴復元導線**（§5 参照）・EC 取得失敗行の警告（§0 決定） | 8 | 3, 6, 9, 10 |
| **PR-7** | **ファイル監視・自動再読込**: watch.rs → `excel_link_watch`。デバウンス幅を実測決定。`~$` オーナーファイル削除の検知で pending を自動再試行 → **§9-5 の完成** | 9 | 7, **5（自動検知の完成）** |

- 順序: PR-0 → PR-1 → PR-2 → PR-3 → PR-4 → PR-5 → PR-6 → PR-7（直列。PR-0 のみ並行可）
- **PR-3 マージ時点で「読み取り専用リンク」が製品価値として成立**する（§5 確定ルール11:
  構造確認完了まで書き込まない、の境界と一致）。PR-4 以降が書き戻し系
- **fixture 方針**: テストコード内で最小 OOXML（sheet.xml / workbook.xml / sharedStrings.xml 断片）を
  合成する方式を基本とし（Excel COM 不要・CI 実行可・PoC の 58 テストと同方式）、実 Excel 由来の
  バイナリ fixture は「実 Excel が保存した属性順・共有数式」の回帰にどうしても必要な最小限のみ
  `src-tauri/tests/fixtures/` にコミット（実型番を含めない・生成手順を README に記録）

## 4. UI 実装の具体（PR-6 の DoD 詳細）

§3.12 が実装要件として必須化した2点の文言案（レビューで調整可）:

- **同時編集非対応の運用警告**（リンク作成ウィザード完了時＋リンク BOM ヘッダの常設インジケータ）:
  > このファイルの同時編集には対応していません。複数の PC・Web で同時に編集すると、
  > あとから同期された変更が本体になり、他方はクラウドの版履歴のみに残ります（アプリは検出できない
  > 場合があります）。編集は1人ずつ行ってください。
- **競合検出時の復元導線**（`sync_status='conflict'` の表示）:
  > 外部の変更と競合しました。外部の版はバックアップ（フォルダを開く）に保全されています。
  > クラウド同期をお使いの場合は、Web の版履歴からも過去の版を復元できます。
- Sheets 由来の指紋誤検出（§3.12 挙動7）: 外部変更検出の説明文に
  「Google スプレッドシート等で開いた場合も変更として検出されることがあります（安全側の動作です）」を明記
- EC 取得失敗行（§0 決定）: 行ステータスに「前回値・手動値の可能性あり」バッジ。Excel へは書かない
  （失敗行のセルはスキップし、成功行のみ更新）

リンク BOM で無効化するもの / 残すもの（§4.8・0003 の AG Grid 上のモード分岐）:

| 無効化 | 残す |
|---|---|
| セル編集（columns.ts `editable`）・行追加/複製/削除（Toolbar）・列追加/削除（ColumnManager）・従来の取込/書き出し導線 | EC 価格・納期取得／履歴閲覧／合計／カート投入／閲覧／**数量倍率の編集**（DB 所有メタ・§4.8 の例外） |

## 5. 従来 BOM との共存（§5 確定ルール27・§9-26）

- **リンク BOM の識別 = `bom_link` 行の存在**。`BomMeta` に `linked: bool`（＋`linkStatus`）を追加し
  フロントはこれで UI を分岐。従来 BOM のコードパス（取込ウィザード・spreadsheet_write）は変更しない
- 回帰なしの担保: PR-1〜7 の各 DoD に「従来 BOM の取込→編集→書き出しの手動スモーク」を含める＋
  既存テスト 9 本＋CI
- 書き戻し無効化の退避経路（§6.2）: `env_verdict='no_writeback'` への降格で実装。
  将来 NO-GO 条件が再燃した場合は環境判定の既定を降格側に倒すだけで §6.2 の退避策になる

## 6. 実装フェーズに持ち越す残作業（本書のスコープ外）

- 3b の symlink 実測（開発者モード or 管理者権限が用意でき次第。それまで fail closed・§3.11）。
  **§9-14 の symlink 部分はこの実測を完了ゲートとする**（PR-3 では「実装＋未検証経路の fail closed」まで。
  Windows ランナーで symlink 作成が可能なら CI テスト化も検討）
- デバウンス幅の実測決定（PR-7 内）
- `calcMode="manual"` 検出ヒューリスティックの実装可否（§4.4.2 検討事項。PR-3 で調査し、
  不成立でも受け入れ条件には影響しない）
- 実務 BOM（YUBI 49行）での構造判定・数式頻度の実測（§3.10 注記。PR-3 後に任意実施）
