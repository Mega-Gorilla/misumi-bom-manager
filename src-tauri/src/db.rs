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
const MIGRATIONS: &[&str] = &[V1];

fn run_migrations(conn: &Connection) -> rusqlite::Result<()> {
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

fn new_id() -> String {
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
            "SELECT name, qty_multiplier, imported_from, updated_at FROM bom WHERE id = ?1",
            [id],
            |r| {
                Ok(BomMeta {
                    name: r.get(0)?,
                    qty_multiplier: r.get(1)?,
                    imported_from: r.get(2)?,
                    updated_at: r.get(3)?,
                })
            },
        )
        .optional()?;
    let Some(meta) = meta else {
        return Ok(None);
    };

    let mut cstmt = conn.prepare(
        "SELECT key, label, kind, editable, width, link_field, link_write \
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
    tx.execute(
        "INSERT INTO bom(id, name, qty_multiplier, imported_from, created_at, updated_at) \
         VALUES(?1, ?2, ?3, ?4, datetime('now'), datetime('now')) \
         ON CONFLICT(id) DO UPDATE SET name = excluded.name, \
           qty_multiplier = excluded.qty_multiplier, imported_from = excluded.imported_from, \
           updated_at = datetime('now')",
        params![
            id,
            doc.meta.name,
            doc.meta.qty_multiplier,
            doc.meta.imported_from
        ],
    )?;

    tx.execute("DELETE FROM bom_column WHERE bom_id = ?1", [&id])?;
    for (i, c) in doc.columns.iter().enumerate() {
        let (lf, lw) = match &c.link {
            Some(l) => (Some(l.field.clone()), Some(l.write.clone())),
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO bom_column(bom_id, key, label, kind, editable, sort_order, link_field, link_write, width) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![id, c.key, c.label, c.kind, c.editable as i64, i as i64, lf, lw, c.width],
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
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, datetime('now'))",
            params![row.id, id, i as i64, row.no, row.parts_name, row.parts_no, row.order, row.qty, row.material, custom_json, supplier_json],
        )?;
    }

    tx.commit()?;
    Ok(id)
}

pub fn delete_bom(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM bom WHERE id = ?1", [id])?; // cascades to columns/rows
    Ok(())
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
                updated_at: None,
            },
            columns: vec![ColumnDef {
                key: "partsNo".into(),
                label: "型番".into(),
                kind: "core".into(),
                editable: true,
                width: None,
                link: None,
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
}
