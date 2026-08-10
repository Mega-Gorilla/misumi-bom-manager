// SQLite data layer (system-of-record). See docs/plans/0003-bom-editor/plan.md §5.1.
//
// Threading: rusqlite::Connection is !Sync, so it is held behind a Mutex in
// managed state (see lib.rs DbState). DB commands are synchronous (no .await
// while holding the lock).

use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

/// Open (or create) the DB, enable WAL/FK, and run migrations.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    run_migrations(&conn)?;
    Ok(conn)
}

// Append-only list of migrations. Index i => schema version i+1.
// NEVER edit a shipped migration string — only append a new one.
const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6];

pub(crate) fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
    let mut v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    while (v as usize) < MIGRATIONS.len() {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(MIGRATIONS[v as usize])?;
        tx.execute_batch(&format!("PRAGMA user_version = {};", v + 1))?;
        tx.commit()?;
        v += 1;
    }
    Ok(())
}

const V1: &str = r#"
CREATE TABLE bom (
  id TEXT PRIMARY KEY,
  name TEXT,
  qty_multiplier REAL NOT NULL DEFAULT 1,
  imported_from TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE supplier (
  code TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  transport TEXT NOT NULL,
  currency TEXT
);
INSERT INTO supplier(code, name, transport, currency) VALUES ('MISUMI', 'MISUMI', 'webview', 'JPY');
CREATE TABLE bom_column (
  bom_id TEXT NOT NULL REFERENCES bom(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  label TEXT NOT NULL,
  kind TEXT NOT NULL,
  editable INTEGER NOT NULL DEFAULT 1,
  sort_order INTEGER NOT NULL,
  link_field TEXT,
  link_write TEXT,
  width REAL,
  PRIMARY KEY (bom_id, key)
);
CREATE TABLE bom_row (
  id TEXT NOT NULL,
  bom_id TEXT NOT NULL REFERENCES bom(id) ON DELETE CASCADE,
  sort_no INTEGER NOT NULL,
  no INTEGER,
  parts_name TEXT,
  parts_no TEXT,
  "order" TEXT,
  qty REAL,
  material TEXT,
  custom_json TEXT NOT NULL DEFAULT '{}',
  supplier_json TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (bom_id, id)
);
CREATE INDEX idx_bom_row_bom ON bom_row(bom_id, sort_no);
CREATE TABLE supplier_cache (
  supplier_code TEXT NOT NULL,
  parts_no TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  currency TEXT,
  fetched_at TEXT NOT NULL,
  PRIMARY KEY (supplier_code, parts_no)
);
CREATE TABLE supplier_price_history (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  supplier_code TEXT NOT NULL,
  parts_no TEXT NOT NULL,
  unit_price TEXT,
  currency TEXT,
  ship_date TEXT,
  fetched_at TEXT NOT NULL
);
CREATE TABLE mapping_template (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  config_json TEXT NOT NULL
);
"#;

// V2: column roles for the fetch pipeline (型番列 / EC発注先列). Backfill existing BOMs
// so the core partsNo/order columns keep playing those roles.
const V2: &str = r#"
ALTER TABLE bom_column ADD COLUMN role TEXT;
UPDATE bom_column SET role = 'partNo' WHERE key = 'partsNo';
UPDATE bom_column SET role = 'source' WHERE key = 'order';
"#;

// V3: record immediate-shippable stock alongside price/ship-date history so the history
// view can trend stock over time. Accumulates from here on — rows written before this
// column existed keep NULL stock (past stock was never captured).
const V3: &str = r#"
ALTER TABLE supplier_price_history ADD COLUMN stock INTEGER;
"#;

// V4: per-BOM separator for joining お客様注文番号1/2/3 columns into the single
// customerItemSubReference at cart-add time. NULL → frontend default (space).
const V4: &str = r#"
ALTER TABLE bom ADD COLUMN order_no_separator TEXT;
"#;

// V5: Excel link mode structure contract + calc state + pending + backup ledger
// (docs/plans/0018-excel-link-mode/plan.md §4.7 / §4.4.2 / §4.2.1 / §4.2.2 / §4.10;
// DDL agreed in docs/plans/0018-excel-link-mode/implementation.md §1.2).
// Downgrade paths do not exist: migrations are append-only and one-way (see MIGRATIONS).
const V5: &str = r#"
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
  -- projection の比較は NULL 安全な IS を使う: projection = 'writeback' は projection が NULL のとき
  -- NULL になり CHECK 全体が素通しになる (= 列挙式でも等号比較経由で同じ穴が再発する)。
  CHECK (
       (ownership = 'app'     AND app_key IS NOT NULL AND source_field IS NOT NULL
                              AND projection IS 'writeback')
    OR (ownership = 'user'    AND app_key IS NOT NULL
                              AND ( (source_field IS NULL     AND projection IS NULL)
                                 OR (source_field IS NOT NULL AND projection IS 'suggest') ))
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
  ec_generation        INTEGER NOT NULL DEFAULT 0,  -- EC 取得世代カウンタ (発行元。implementation.md §2.3)
  applied_generation   INTEGER NOT NULL DEFAULT 0   -- Excel へ反映済みの世代
);

-- BOM 単位の採用スナップショット。supplier_cache は (supplier_code, parts_no) キーの全 BOM 共有で
-- 「取得の最適化」に限定し、この BOM が採用した EC 値の正本はここに置く。
-- これにより ec_generation が指す DB 状態が BOM 境界で閉じる (他 BOM の再取得が共有 cache を
-- 更新しても、この BOM のスナップショット・世代は変わらない)。reader/writeback はここを参照する。
-- 採用 payload と直近試行結果を分離して持つ: 「失敗行は既存値を残し警告」(§6.2.1 決定) を
-- 再起動・再 open 後も再現するため、失敗の事実を永続化する (UI のその場警告だけでは消える)。
CREATE TABLE bom_link_quote (
  bom_id        TEXT NOT NULL REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  supplier_code TEXT NOT NULL,
  parts_no      TEXT NOT NULL,
  payload_json  TEXT,              -- 採用済み SupplierQuote (supplier_cache と同形)。NULL = 成功採用がまだ無い
  currency      TEXT,
  fetched_at    TEXT,              -- 採用 payload の取得時刻 (payload と対で更新)
  generation    INTEGER,           -- この行を採用した世代。NULL = 未採用 (初回失敗のみの行)
  last_attempt_at     TEXT NOT NULL,   -- 直近試行の時刻 (成功・失敗を問わず更新)
  last_attempt_status TEXT NOT NULL CHECK (last_attempt_status IN ('ok','error')),
  last_error_code     TEXT,        -- 機械判別コード (error 時のみ)
  last_error_message  TEXT,        -- 表示用メッセージ
  PRIMARY KEY (bom_id, supplier_code, parts_no),
  CHECK ((payload_json IS NULL) = (generation IS NULL)),
  CHECK ((payload_json IS NULL) = (fetched_at IS NULL)),
  CHECK (last_attempt_status <> 'error' OR last_error_code IS NOT NULL),
  CHECK (last_attempt_status <> 'ok' OR (last_error_code IS NULL AND last_error_message IS NULL))
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
  is_conflict    INTEGER NOT NULL DEFAULT 0 CHECK (is_conflict IN (0, 1)),
                                       -- backup_fp != f0_fp (§4.2.2 手順8)
  created_at     TEXT NOT NULL,
  transferred_at TEXT,                 -- app_data への移送完了時刻 (NULL = 元 backup が残置)
  resolved_at    TEXT,                 -- 競合をユーザーが解決した時刻
  deleted_at     TEXT,                 -- 保持ポリシーによるファイル削除時刻 (台帳行は残す)
  CHECK (bom_id IS NULL OR bom_id = origin_bom_id),  -- 生存中 FK と不変 ID の取り違え防止
  -- 競合は backup_fp != f0_fp から一意に決まる事実であり入力値を信じない (誤った 0 は
  -- unresolved_conflicts と retention の競合除外から漏れ、外部版の唯一の退避先を失わせる)。
  CHECK (is_conflict = (backup_fp <> f0_fp)),
  -- 所在と移送時刻の整合: volume_temp ⇔ 未移送 / app_data ⇔ 移送完了 (mark_transferred のみが遷移)。
  -- transferred_at 無しの app_data 行が「移送途中の保護 (location='volume_temp' 除外)」を
  -- すり抜けて retention 候補になることを構造的に防ぐ。
  CHECK ((location = 'volume_temp') = (transferred_at IS NULL))
);
CREATE INDEX idx_bom_link_backup_bom ON bom_link_backup(origin_bom_id, created_at);
CREATE INDEX idx_bom_link_backup_open_conflict
  ON bom_link_backup(is_conflict) WHERE is_conflict = 1 AND resolved_at IS NULL;
"#;

// V6: 書き込みジャーナル (plan.md §4.2.1 fs+DB 非原子性への回復プロトコル)。
// ReplaceFileW の直前に「復旧に必要な全情報」を永続化し、置換成功〜台帳/state commit の
// 間でクラッシュ・障害が起きても、次回 open/apply の reconcile が台帳化を完遂できるようにする。
// ReplaceFileW は原子的なので「backup_path のファイルが存在する ⟺ 置換は実行された」が
// 復旧時の判定基準になる (excel_link::reconcile_write_journal)。
// 正常系では置換後の 1 トランザクション (台帳+state+pending) が本行を同時に削除する。
const V6: &str = r#"
CREATE TABLE bom_link_write_journal (
  bom_id      TEXT PRIMARY KEY REFERENCES bom_link(bom_id) ON DELETE CASCADE,
  backup_path TEXT NOT NULL,   -- 置換で選んだ backup の一意名 (存在チェックが復旧判定)
  temp_path   TEXT NOT NULL,   -- 置換前 temp (置換未実行のまま残置された場合の掃除対象)
  fp_algo     TEXT NOT NULL DEFAULT 'sha256-v1',
  f0_fp       TEXT NOT NULL,   -- 書き込みの基になった内容の指紋 F0 (§4.2.2 手順2)
  new_fp      TEXT NOT NULL,   -- アプリが書いた temp の指紋 (復旧時の state 確定に使用)
  generation  INTEGER NOT NULL,-- 反映しようとした EC 世代 (applied_generation の復旧値)
  created_at  TEXT NOT NULL
);
"#;

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("bom-{t}-{n}")
}

pub fn list_boms(conn: &Connection) -> rusqlite::Result<Vec<BomSummary>> {
    let mut stmt = conn.prepare(
        "SELECT b.id, b.name, b.updated_at, \
         (SELECT COUNT(*) FROM bom_row r WHERE r.bom_id = b.id) AS rc \
         FROM bom b ORDER BY b.updated_at DESC",
    )?;
    let it = stmt.query_map([], |r| {
        Ok(BomSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            updated_at: r.get(2)?,
            row_count: r.get(3)?,
        })
    })?;
    it.collect()
}

pub fn load_bom(conn: &Connection, id: &str) -> rusqlite::Result<Option<BomDoc>> {
    let meta = conn
        .query_row(
            "SELECT name, qty_multiplier, imported_from, updated_at, order_no_separator \
             FROM bom WHERE id = ?1",
            [id],
            |r| {
                Ok(BomMeta {
                    name: r.get(0)?,
                    qty_multiplier: r.get(1)?,
                    imported_from: r.get(2)?,
                    updated_at: r.get(3)?,
                    order_no_separator: r.get(4)?,
                })
            },
        )
        .optional()?;
    let Some(meta) = meta else {
        return Ok(None);
    };

    let mut cstmt = conn.prepare(
        "SELECT key, label, kind, editable, width, link_field, link_write, role \
         FROM bom_column WHERE bom_id = ?1 ORDER BY sort_order",
    )?;
    let columns = cstmt
        .query_map([id], |r| {
            let link_field: Option<String> = r.get(5)?;
            let link_write: Option<String> = r.get(6)?;
            let link = link_field.map(|field| ColumnLink {
                field,
                write: link_write.unwrap_or_else(|| "fillEmpty".into()),
            });
            Ok(ColumnDef {
                key: r.get(0)?,
                label: r.get(1)?,
                kind: r.get(2)?,
                editable: r.get::<_, i64>(3)? != 0,
                width: r.get(4)?,
                link,
                role: r.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut rstmt = conn.prepare(
        "SELECT id, no, parts_name, parts_no, \"order\", qty, material, custom_json, supplier_json \
         FROM bom_row WHERE bom_id = ?1 ORDER BY sort_no",
    )?;
    let rows = rstmt
        .query_map([id], |r| {
            let custom_json: Option<String> = r.get(7)?;
            let supplier_json: Option<String> = r.get(8)?;
            let custom: HashMap<String, String> = custom_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            let supplier: Option<SupplierQuote> = supplier_json
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            Ok(BomRow {
                id: r.get(0)?,
                no: r.get(1)?,
                parts_name: r.get(2)?,
                parts_no: r.get(3)?,
                order: r.get(4)?,
                qty: r.get(5)?,
                material: r.get(6)?,
                custom,
                supplier,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Some(BomDoc {
        id: Some(id.to_string()),
        version: 1,
        meta,
        columns,
        rows,
    }))
}

/// Full-replace upsert (columns + rows) inside a transaction. Returns the BOM id.
pub fn save_bom(conn: &mut Connection, doc: &BomDoc) -> rusqlite::Result<String> {
    let id = doc.id.clone().unwrap_or_else(new_id);
    let tx = conn.transaction()?;
    upsert_bom_meta(&tx, &id, &doc.meta)?;
    write_columns_rows(&tx, &id, doc)?;
    tx.commit()?;
    Ok(id)
}

/// Upsert the `bom` row (meta only) inside the caller's transaction. Shared by
/// save_bom and the atomic linked-BOM creation (excel_link::create_link).
pub(crate) fn upsert_bom_meta(
    tx: &rusqlite::Transaction,
    id: &str,
    meta: &BomMeta,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO bom(id, name, qty_multiplier, imported_from, order_no_separator, created_at, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, datetime('now', 'localtime'), datetime('now', 'localtime')) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
           qty_multiplier = excluded.qty_multiplier, imported_from = excluded.imported_from, \
           order_no_separator = excluded.order_no_separator, \
           updated_at = datetime('now', 'localtime')",
        params![
            id,
            meta.name,
            meta.qty_multiplier,
            meta.imported_from,
            meta.order_no_separator
        ],
    )?;
    Ok(())
}

/// Full-replace of bom_column/bom_row inside the caller's transaction. Shared by
/// save_bom and the linked-BOM display-cache persistence (excel_link open), which
/// must run in the same transaction as its state updates.
pub(crate) fn write_columns_rows(
    tx: &rusqlite::Transaction,
    id: &str,
    doc: &BomDoc,
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM bom_column WHERE bom_id = ?1", [&id])?;
    for (i, c) in doc.columns.iter().enumerate() {
        let (lf, lw) = match &c.link {
            Some(l) => (Some(l.field.clone()), Some(l.write.clone())),
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO bom_column(bom_id, key, label, kind, editable, sort_order, link_field, link_write, width, role) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, c.key, c.label, c.kind, c.editable as i64, i as i64, lf, lw, c.width, c.role],
        )?;
    }

    tx.execute("DELETE FROM bom_row WHERE bom_id = ?1", [&id])?;
    for (i, row) in doc.rows.iter().enumerate() {
        let custom_json = serde_json::to_string(&row.custom).unwrap_or_else(|_| "{}".into());
        let supplier_json = row
            .supplier
            .as_ref()
            .and_then(|s| serde_json::to_string(s).ok());
        tx.execute(
            "INSERT INTO bom_row(id, bom_id, sort_no, no, parts_name, parts_no, \"order\", qty, material, custom_json, supplier_json, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now', 'localtime'))",
            params![row.id, id, i as i64, row.no, row.parts_name, row.parts_no, row.order, row.qty, row.material, custom_json, supplier_json],
        )?;
    }
    Ok(())
}

/// Normalize away the one-shot import link semantics on a linked BOM's display
/// cache (§1.3): fillEmpty/overwrite do not survive continuous sync — the contract's
/// source_field carries the projection from here on.
pub(crate) fn clear_column_links(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_column SET link_field = NULL, link_write = NULL WHERE bom_id = ?1",
        [id],
    )?;
    Ok(())
}

pub fn delete_bom(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    // A pending Excel-link write journal cascades away with the BOM, and it is the
    // only recovery pointer for a displaced backup that never reached the ledger
    // (V6; 3rd review #1). Deleting is fine once a link open has reconciled it.
    let pending: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bom_link_write_journal WHERE bom_id = ?1)",
        [id],
        |r| r.get(0),
    )?;
    if pending {
        return Err(rusqlite::Error::ToSqlConversionFailure(
            "中断された書き込みの復旧が完了していません。先にリンクを開いて復旧してから削除してください"
                .to_string()
                .into(),
        ));
    }
    conn.execute("DELETE FROM bom WHERE id = ?1", [id])?; // cascades to columns/rows
    Ok(())
}

// ---- supplier cache (cross-BOM, keyed by (supplier_code, parts_no)) ----

/// Read a cached normalized quote for (supplier, parts_no), if present.
pub fn cache_get(
    conn: &Connection,
    supplier: &str,
    parts_no: &str,
) -> rusqlite::Result<Option<SupplierQuote>> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM supplier_cache WHERE supplier_code = ?1 AND parts_no = ?2",
            params![supplier, parts_no],
            |r| r.get(0),
        )
        .optional()?;
    Ok(payload.and_then(|s| serde_json::from_str(&s).ok()))
}

/// Upsert the cache and append a price-history row. Uses `quote.fetched_at` (set by
/// the caller) for the timestamp so cache payload and column agree.
pub fn cache_put(
    conn: &Connection,
    supplier: &str,
    parts_no: &str,
    quote: &SupplierQuote,
) -> rusqlite::Result<()> {
    let payload = serde_json::to_string(quote).unwrap_or_else(|_| "{}".into());
    let currency = quote.quote.as_ref().and_then(|p| p.currency.clone());
    let fetched = quote.fetched_at.clone();
    conn.execute(
        "INSERT INTO supplier_cache(supplier_code, parts_no, payload_json, currency, fetched_at) \
         VALUES(?1, ?2, ?3, ?4, COALESCE(?5, datetime('now', 'localtime'))) \
         ON CONFLICT(supplier_code, parts_no) DO UPDATE SET \
           payload_json = excluded.payload_json, currency = excluded.currency, \
           fetched_at = excluded.fetched_at",
        params![supplier, parts_no, payload, currency, fetched],
    )?;
    let unit_price = quote.quote.as_ref().and_then(|p| p.unit_price.clone());
    let ship_date = quote.quote.as_ref().and_then(|p| p.ship_date.clone());
    let stock = quote.quote.as_ref().and_then(|p| p.stock);
    conn.execute(
        "INSERT INTO supplier_price_history(supplier_code, parts_no, unit_price, currency, ship_date, stock, fetched_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, COALESCE(?7, datetime('now', 'localtime')))",
        params![supplier, parts_no, unit_price, currency, ship_date, stock, fetched],
    )?;
    Ok(())
}

/// Price/delivery history for a (supplier, part number), newest first (Phase 3).
/// Reads the append-only `supplier_price_history` rows written by `cache_put`.
pub fn price_history(
    conn: &Connection,
    supplier: &str,
    parts_no: &str,
    limit: i64,
) -> rusqlite::Result<Vec<PriceHistoryEntry>> {
    let mut stmt = conn.prepare(
        "SELECT fetched_at, unit_price, currency, ship_date, stock FROM supplier_price_history \
         WHERE supplier_code = ?1 AND parts_no = ?2 ORDER BY fetched_at DESC, id DESC LIMIT ?3",
    )?;
    let it = stmt.query_map(params![supplier, parts_no, limit], |r| {
        Ok(PriceHistoryEntry {
            fetched_at: r.get(0)?,
            unit_price: r.get(1)?,
            currency: r.get(2)?,
            ship_date: r.get(3)?,
            stock: r.get(4)?,
        })
    })?;
    it.collect()
}

/// SQLite's current timestamp string (for stamping a batch of quotes consistently).
pub fn now_string(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("SELECT datetime('now', 'localtime')", [], |r| r.get(0))
}

/// Local calendar date "YYYY-MM-DD" (for same-day cache freshness checks).
pub fn today_local(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("SELECT date('now', 'localtime')", [], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    #[test]
    fn save_load_list_delete_roundtrip() {
        let mut conn = mem();
        let mut custom = HashMap::new();
        custom.insert("note".to_string(), "x".to_string());
        let doc = BomDoc {
            id: Some("b1".into()),
            version: 1,
            meta: BomMeta {
                name: Some("test".into()),
                imported_from: None,
                qty_multiplier: 3.0,
                order_no_separator: None,
                updated_at: None,
            },
            columns: vec![ColumnDef {
                key: "partsNo".into(),
                label: "型番".into(),
                kind: "core".into(),
                editable: true,
                width: None,
                link: None,
                role: Some("partNo".into()),
            }],
            rows: vec![BomRow {
                id: "r1".into(),
                no: Some(1),
                parts_name: None,
                parts_no: Some("CBT3-8".into()),
                order: Some("MISUMI".into()),
                qty: Some(2.0),
                material: None,
                custom,
                supplier: None,
            }],
        };

        let id = save_bom(&mut conn, &doc).unwrap();
        assert_eq!(id, "b1");

        let loaded = load_bom(&conn, "b1").unwrap().unwrap();
        assert_eq!(loaded.meta.qty_multiplier, 3.0);
        assert_eq!(loaded.columns.len(), 1);
        assert_eq!(loaded.columns[0].role.as_deref(), Some("partNo"));
        assert_eq!(loaded.rows.len(), 1);
        assert_eq!(loaded.rows[0].parts_no.as_deref(), Some("CBT3-8"));
        assert_eq!(
            loaded.rows[0].custom.get("note").map(String::as_str),
            Some("x")
        );

        let list = list_boms(&conn).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].row_count, 1);

        // Re-save with fewer rows (full-replace) works.
        let mut doc2 = doc.clone();
        doc2.rows.clear();
        save_bom(&mut conn, &doc2).unwrap();
        assert_eq!(load_bom(&conn, "b1").unwrap().unwrap().rows.len(), 0);

        delete_bom(&conn, "b1").unwrap();
        assert!(load_bom(&conn, "b1").unwrap().is_none());
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bom_row", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, 0); // cascade
    }

    #[test]
    fn price_history_appends_and_reads_newest_first() {
        let conn = mem();
        let mk = |price: &str, ship: &str, at: &str| SupplierQuote {
            supplier_code: "MISUMI".into(),
            status: "ok".into(),
            product: None,
            quote: Some(SupplierPricing {
                currency: Some("JPY".into()),
                unit_price: Some(price.into()),
                ship_date: Some(ship.into()),
                stock: Some(price.parse::<i64>().unwrap_or(0) + 1), // arbitrary but distinct
                ..Default::default()
            }),
            errors: vec![],
            warnings: vec![],
            fetched_at: Some(at.into()),
            raw: None,
        };
        // Two observations for the same part on different days (append-only history).
        cache_put(
            &conn,
            "MISUMI",
            "CBT3-8",
            &mk("115", "2026-07-14", "2026-07-05 09:10:00"),
        )
        .unwrap();
        cache_put(
            &conn,
            "MISUMI",
            "CBT3-8",
            &mk("120", "2026-07-15", "2026-07-06 15:20:00"),
        )
        .unwrap();

        let hist = price_history(&conn, "MISUMI", "CBT3-8", 60).unwrap();
        assert_eq!(hist.len(), 2);
        // Newest first.
        assert_eq!(hist[0].unit_price.as_deref(), Some("120"));
        assert_eq!(hist[0].ship_date.as_deref(), Some("2026-07-15"));
        assert_eq!(hist[0].stock, Some(121)); // recorded stock (V3)
        assert_eq!(hist[1].unit_price.as_deref(), Some("115"));

        // Unrelated part number has no history.
        assert!(price_history(&conn, "MISUMI", "OTHER", 60)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn v5_creates_link_tables_and_reopen_is_noop() {
        let conn = mem();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len()); // fresh DB lands on the latest version

        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'bom_link%' \
                 ORDER BY name",
            )
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            tables,
            vec![
                "bom_link",
                "bom_link_backup",
                "bom_link_column",
                "bom_link_pending",
                "bom_link_quote",
                "bom_link_state",
                "bom_link_write_journal",
            ]
        );

        // Re-running migrations on an up-to-date DB is a no-op (idempotent reopen).
        // There is no downgrade path by design: MIGRATIONS is append-only and one-way.
        run_migrations(&conn).unwrap();
        let v2: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, v2);
    }

    #[test]
    fn v4_to_v5_upgrade_preserves_existing_data() {
        // Simulate a shipped V4 database: apply only V1..V4, then store a BOM the way
        // the app would have, and verify run_migrations upgrades to V5 without touching it.
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        for m in &MIGRATIONS[..4] {
            conn.execute_batch(m).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 4;").unwrap();
        conn.execute(
            "INSERT INTO bom(id, name, qty_multiplier, created_at, updated_at) \
             VALUES('legacy', 'v4 bom', 2.0, datetime('now'), datetime('now'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bom_column(bom_id, key, label, kind, sort_order, role) \
             VALUES('legacy', 'partsNo', '型番', 'core', 0, 'partNo')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO bom_row(id, bom_id, sort_no, parts_no, custom_json, updated_at) \
             VALUES('r1', 'legacy', 0, 'TEST-PART-001', '{}', datetime('now'))",
            [],
        )
        .unwrap();

        run_migrations(&conn).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());

        let doc = load_bom(&conn, "legacy").unwrap().unwrap();
        assert_eq!(doc.meta.qty_multiplier, 2.0);
        assert_eq!(doc.columns.len(), 1);
        assert_eq!(doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
        // And the new tables are present and empty.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM bom_link", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn bomrow_id_defaults_when_missing() {
        // JSON import: rows without an "id" field must still parse (id => ""),
        // so bom_import can assign a fresh id. Regression for PR #6 review.
        let json = r#"{"version":1,"meta":{"name":"x","qtyMultiplier":1},
            "columns":[],"rows":[{"partsNo":"CBT3-8","custom":{}}]}"#;
        let doc: BomDoc = serde_json::from_str(json).unwrap();
        assert_eq!(doc.rows.len(), 1);
        assert_eq!(doc.rows[0].id, "");
        assert_eq!(doc.rows[0].parts_no.as_deref(), Some("CBT3-8"));
    }
}
