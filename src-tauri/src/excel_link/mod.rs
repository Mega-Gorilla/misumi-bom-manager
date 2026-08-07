// Excel link mode (docs/plans/0018-excel-link-mode/ plan.md §4 + implementation.md §2.1).
//
// Module layout follows implementation.md §2.1. Shipped so far: persistence
// (`store` — PR-1), pure logic (`calc_state` / `fingerprint` / `env` core — PR-2),
// and the read-only link (env resolution, `xlsx` walkers, `contract` checker,
// `reader`, and the probe/create/open/unlink orchestration below — PR-3). Later
// PRs add: writeback / backup (PR-4), pending/confirm orchestration (PR-5),
// watch (PR-7).

pub mod calc_state;
pub mod contract;
pub mod env;
pub mod fingerprint;
pub mod reader;
pub mod store;
pub mod xlsx;

use crate::model::{
    BomDoc, BomMeta, LinkCreateConfig, LinkProbe, LinkedBomView, StructureVerdict, SyncStatus,
};
use contract::{Contract, Severity};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

const FP_RETRIES: u32 = 3;
const FP_BACKOFF: Duration = Duration::from_millis(200);

/// Wizard probe: read-only look at a workbook + environment verdict (§2.3).
pub fn probe(path: &str) -> Result<LinkProbe, String> {
    let p = Path::new(path);
    let env = env::check_env(p);
    let sheets = reader::probe_sheets(p)?;
    let mut warnings = Vec::new();
    if let Ok(mut zip) = xlsx::open_zip(p) {
        if let Ok(wb) = xlsx::read_part(&mut zip, "xl/workbook.xml") {
            if xlsx::calc_mode(&wb).as_deref() == Some("manual") {
                warnings.push(calc_mode_warning());
            }
        }
    }
    Ok(LinkProbe {
        env,
        sheets,
        warnings,
    })
}

fn calc_mode_warning() -> String {
    "このブックは手動計算モード (calcMode=\"manual\") です。数式のキャッシュ値がリンク時点で\
     既に古い可能性があります (§4.4.2)"
        .to_string()
}

/// Create a link: environment check → contract persistence → first open (§2.3).
pub fn create_link(
    conn: &mut Connection,
    config: &LinkCreateConfig,
) -> Result<LinkedBomView, String> {
    if config.data_start_row <= config.header_row || config.header_row < 1 {
        return Err("データ開始行はヘッダ行より下である必要があります".into());
    }
    let env = env::check_env(Path::new(&config.workbook_path));

    // BOM row (FK target): reuse or create fresh.
    let bom_id = match &config.bom_id {
        Some(id) => {
            if db_load(conn, id)?.is_none() {
                return Err(format!("BOM が見つかりません: {id}"));
            }
            id.clone()
        }
        None => {
            let doc = BomDoc {
                id: None,
                version: 1,
                meta: BomMeta {
                    name: config.name.clone(),
                    imported_from: Some(config.workbook_path.clone()),
                    qty_multiplier: 1.0,
                    order_no_separator: None,
                    updated_at: None,
                },
                columns: vec![],
                rows: vec![],
            };
            crate::db::save_bom(conn, &doc).map_err(|e| e.to_string())?
        }
    };
    if store::get_link(conn, &bom_id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("この BOM は既に Excel にリンクされています".into());
    }

    let columns: Vec<store::LinkColumn> = config
        .columns
        .iter()
        .map(|c| store::LinkColumn {
            excel_col: c.excel_col,
            header_label: c.header_label.clone(),
            app_key: c.app_key.clone(),
            ownership: c.ownership,
            required: c.required,
            role: c.role.clone(),
            source_field: c.source_field.clone(),
            projection: c.projection,
        })
        .collect();
    store::create_link(
        conn,
        &store::NewLink {
            bom_id: bom_id.clone(),
            workbook_path: config.workbook_path.clone(),
            sheet_name: config.sheet_name.clone(),
            header_row: config.header_row,
            data_start_row: config.data_start_row,
            env_verdict: env.verdict,
            env_resolved_path: env.resolved_path.clone(),
            env_fs_name: env.fs_name.clone(),
            columns,
        },
    )
    .map_err(|e| format!("リンク契約の保存に失敗しました: {e}"))?;
    // §1.3: the one-shot import link semantics do not survive continuous sync.
    crate::db::clear_column_links(conn, &bom_id).map_err(|e| e.to_string())?;

    open_link(conn, &bom_id)
}

/// Unlink (§1.3): one transaction — the display cache in bom_column/bom_row IS the
/// latest safe snapshot (open() persists it on every successful read), so freezing
/// it means simply deleting the bom_link row; contract/state/quotes/pending go via
/// FK cascade, the backup ledger stays.
pub fn unlink(conn: &mut Connection, bom_id: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let n = tx
        .execute("DELETE FROM bom_link WHERE bom_id = ?1", [bom_id])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("この BOM は Excel にリンクされていません".into());
    }
    tx.commit().map_err(|e| e.to_string())
}

fn db_load(conn: &Connection, bom_id: &str) -> Result<Option<BomDoc>, String> {
    crate::db::load_bom(conn, bom_id).map_err(|e| e.to_string())
}

/// Open/refresh a linked BOM: read → verify → restore → compose (§2.3 — the common
/// entry for first display, the manual "更新" button and (PR-7) auto reload).
pub fn open_link(conn: &mut Connection, bom_id: &str) -> Result<LinkedBomView, String> {
    let rec = store::get_link(conn, bom_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "この BOM は Excel にリンクされていません".to_string())?;
    let prev_doc =
        db_load(conn, bom_id)?.ok_or_else(|| format!("BOM が見つかりません: {bom_id}"))?;
    let env = env::EnvCheck {
        verdict: rec.header.env_verdict,
        resolved_path: rec.header.env_resolved_path.clone(),
        fs_name: rec.header.env_fs_name.clone(),
        reason: None,
    };

    // Contract invariants + workbook presence: broken (fail closed), not Err —
    // the caller still gets the previous snapshot to display (§1.3).
    let contract = match Contract::try_from_store(&rec.header, &rec.columns) {
        Ok(c) => c,
        Err(e) => {
            return broken_view(conn, bom_id, rec.state, prev_doc, env, vec![e.to_string()]);
        }
    };
    let path = Path::new(&rec.header.workbook_path);
    if !path.exists() {
        return broken_view(
            conn,
            bom_id,
            rec.state,
            prev_doc,
            env,
            vec![format!(
                "E_WORKBOOK_MISSING: リンク先ファイルが見つかりません: {}",
                rec.header.workbook_path
            )],
        );
    }

    // Stable content fingerprint (§4.6.1) and the two read channels (§3.5: works
    // while Excel holds the file open — read sharing is allowed).
    let fp = fingerprint::stable_fingerprint(path, FP_RETRIES, FP_BACKOFF)
        .map_err(|e| format!("指紋の取得に失敗しました: {e}"))?;
    let outcome = reader::observe(path, &contract)?;
    let verify = contract::verify_structure(&contract, &outcome.sheets);

    let mut warnings = Vec::new();
    if outcome.calc_mode.as_deref() == Some("manual") {
        warnings.push(calc_mode_warning());
    }
    if outcome.truncated {
        warnings.push(format!(
            "行数が {} 行を超えています。表示は打ち切られ、Excel への書き込みは行われません (§3.2)",
            reader::MAX_ROWS
        ));
    }

    match verify.severity() {
        Severity::Safe => {
            // Auto-adopt clean new user columns into the contract (rule 7).
            let mut columns = rec.columns.clone();
            if !verify.new_columns.is_empty() {
                let mut keys: BTreeSet<String> =
                    columns.iter().filter_map(|c| c.app_key.clone()).collect();
                for nc in &verify.new_columns {
                    let key = fresh_user_key(&keys, nc.excel_col);
                    keys.insert(key.clone());
                    columns.push(store::LinkColumn {
                        excel_col: nc.excel_col as i64,
                        header_label: Some(nc.label.clone()),
                        app_key: Some(key),
                        ownership: crate::model::LinkOwnership::User,
                        required: false,
                        role: None,
                        source_field: None,
                        projection: None,
                    });
                }
                store::replace_columns(conn, bom_id, &columns).map_err(|e| e.to_string())?;
            }
            let contract = {
                // Re-derive so newly adopted columns join the composed view.
                let rec2 = store::get_link(conn, bom_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "リンクが消失しました".to_string())?;
                Contract::try_from_store(&rec2.header, &rec2.columns).map_err(|e| e.to_string())?
            };

            let quotes = store::list_quotes(conn, bom_id).map_err(|e| e.to_string())?;
            let doc = reader::compose(&contract, &outcome, &quotes, prev_doc.meta.clone(), bom_id);

            // Restore rule (§4.4.2) from the persisted state + this read.
            let persisted = calc_state::persisted_from_state(&rec.state, outcome.value_readable)
                .map_err(|e| e.to_string())?;
            let calc = calc_state::restore(&fp, &persisted);

            let mut st = rec.state.clone();
            st.sync_status = SyncStatus::Linked;
            st.sync_error = None;
            st.calc_state = calc;
            st.last_read_fp = Some(fingerprint::to_hex(&fp));
            st.structure_fp = Some(verify.structure_fp.clone());
            st.data_first_row = verify
                .effective_header_row
                .map(|h| contract.effective_data_start(h) as i64);
            st.data_last_row = Some(outcome.last_data_row as i64);
            st.row_count = Some(doc.rows.len() as i64);

            // Display cache + state in ONE transaction (§1.1: the cache is what
            // unlink freezes as the latest safe snapshot).
            let tx = conn.transaction().map_err(|e| e.to_string())?;
            crate::db::write_columns_rows(&tx, bom_id, &doc).map_err(|e| e.to_string())?;
            store::update_state(&tx, bom_id, &st).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;

            Ok(LinkedBomView {
                doc,
                verdict: verify.to_verdict(&contract),
                calc_state: calc,
                sync_status: SyncStatus::Linked,
                env,
                formula_cells: outcome.formula_cells,
                truncated: outcome.truncated,
                warnings,
            })
        }
        Severity::Confirm | Severity::Broken => {
            let broken = verify.severity() == Severity::Broken;
            let mut st = rec.state.clone();
            st.sync_status = if broken {
                SyncStatus::Broken
            } else {
                SyncStatus::NeedsReview
            };
            st.sync_error = Some(
                verify
                    .anomalies
                    .iter()
                    .map(|a| a.code())
                    .collect::<Vec<_>>()
                    .join(";"),
            );
            st.structure_fp = Some(verify.structure_fp.clone());
            store::update_state(conn, bom_id, &st).map_err(|e| e.to_string())?;
            Ok(LinkedBomView {
                doc: prev_doc,
                verdict: verify.to_verdict(&contract),
                calc_state: rec.state.calc_state,
                sync_status: st.sync_status,
                env,
                formula_cells: outcome.formula_cells,
                truncated: outcome.truncated,
                warnings,
            })
        }
    }
}

/// Contract/workbook-level failure: mark broken, return the previous snapshot.
fn broken_view(
    conn: &mut Connection,
    bom_id: &str,
    mut state: store::LinkState,
    prev_doc: BomDoc,
    env: env::EnvCheck,
    reasons: Vec<String>,
) -> Result<LinkedBomView, String> {
    state.sync_status = SyncStatus::Broken;
    state.sync_error = Some(reasons.join("; "));
    store::update_state(conn, bom_id, &state).map_err(|e| e.to_string())?;
    Ok(LinkedBomView {
        doc: prev_doc,
        verdict: StructureVerdict::Broken { reasons },
        calc_state: state.calc_state,
        sync_status: SyncStatus::Broken,
        env,
        formula_cells: vec![],
        truncated: false,
        warnings: vec![],
    })
}

/// Stable-after-creation key for an auto-adopted user column. Uniqueness is checked
/// against the live contract (a column that moved away may have left its old key).
fn fresh_user_key(existing: &BTreeSet<String>, excel_col: u32) -> String {
    let base = format!("ucol{excel_col}");
    if !existing.contains(&base) {
        return base;
    }
    let mut i = 2;
    loop {
        let k = format!("{base}_{i}");
        if !existing.contains(&k) {
            return k;
        }
        i += 1;
    }
}

// Re-export used by model.rs (LinkedBomView / LinkProbe embed it).
pub use env::EnvCheck;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{LinkColumnConfig, LinkOwnership, LinkProjection, SupplierQuote};

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    fn temp_xlsx(name: &str, headers: &[&str], rows: &[&[&str]]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("mbm-link-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        for (c, h) in headers.iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if let Ok(n) = v.parse::<f64>() {
                    ws.write_number(r as u32 + 1, c as u16, n).unwrap();
                } else if !v.is_empty() {
                    ws.write_string(r as u32 + 1, c as u16, *v).unwrap();
                }
            }
        }
        let _ = std::fs::remove_file(&path);
        wb.save(&path).unwrap();
        path
    }

    fn config(path: &std::path::Path) -> LinkCreateConfig {
        let col = |excel_col: i64,
                   label: &str,
                   key: Option<&str>,
                   own: LinkOwnership,
                   required: bool,
                   source: Option<&str>,
                   proj: Option<LinkProjection>| LinkColumnConfig {
            excel_col,
            header_label: Some(label.into()),
            app_key: key.map(str::to_string),
            ownership: own,
            required,
            role: None,
            source_field: source.map(str::to_string),
            projection: proj,
        };
        LinkCreateConfig {
            bom_id: None,
            name: Some("linked".into()),
            workbook_path: path.to_string_lossy().into_owned(),
            sheet_name: "Sheet1".into(), // rust_xlsxwriter's default sheet name
            header_row: 1,
            data_start_row: 2,
            columns: vec![
                col(
                    0,
                    "型番",
                    Some("partsNo"),
                    LinkOwnership::User,
                    true,
                    None,
                    None,
                ),
                col(
                    1,
                    "数量",
                    Some("qty"),
                    LinkOwnership::User,
                    false,
                    None,
                    None,
                ),
                col(
                    2,
                    "EC単価",
                    Some("ecUnitPrice"),
                    LinkOwnership::App,
                    false,
                    Some("quote.unitPrice"),
                    Some(LinkProjection::Writeback),
                ),
            ],
        }
    }

    fn mk_quote(price: &str) -> SupplierQuote {
        SupplierQuote {
            supplier_code: "MISUMI".into(),
            status: "ok".into(),
            product: None,
            quote: Some(crate::model::SupplierPricing {
                currency: Some("JPY".into()),
                unit_price: Some(price.into()),
                ..Default::default()
            }),
            errors: vec![],
            warnings: vec![],
            fetched_at: Some("2026-08-01 09:00:00".into()),
            raw: None,
        }
    }

    /// End-to-end lifecycle: create → Safe open persists the display cache →
    /// quote adoption composes into rows → Confirm/Broken keep the previous
    /// snapshot and contract → unlink freezes the cache (§1.3).
    #[test]
    fn link_lifecycle_create_open_confirm_broken_unlink() {
        let mut conn = mem();
        let path = temp_xlsx(
            "lifecycle.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""], &["TEST-PART-002", "3", ""]],
        );

        // ---- create (fresh BOM) + first open: Safe ----
        let view = create_link(&mut conn, &config(&path)).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked, "{:?}", view.verdict);
        assert!(matches!(view.verdict, StructureVerdict::Safe { .. }));
        assert_eq!(view.doc.rows.len(), 2);
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
        assert_eq!(view.doc.rows[0].qty, Some(2.0));
        assert_eq!(view.calc_state, crate::model::CalcState::Unverified); // §9-11
        let bom_id = view.doc.id.clone().unwrap();

        // Display cache persisted (this is what unlink freezes).
        let cached = crate::db::load_bom(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(cached.rows.len(), 2);
        assert_eq!(cached.columns.len(), 3);
        assert!(cached.columns.iter().all(|c| c.link.is_none())); // §1.3 normalized
                                                                  // State recorded the read.
        let st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        assert!(st.last_read_fp.is_some());
        assert!(st.structure_fp.is_some());
        assert_eq!(st.row_count, Some(2));

        // ---- adopted quote snapshot composes into the view (§9-1) ----
        store::record_quote_ok(&conn, &bom_id, "TEST-PART-001", &mk_quote("115"), 1).unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap();
        let q = view.doc.rows[0].supplier.as_ref().expect("quote composed");
        assert_eq!(q.quote.as_ref().unwrap().unit_price.as_deref(), Some("115"));
        assert!(view.doc.rows[1].supplier.is_none()); // no snapshot for part 2

        // ---- Confirm: renamed user header stops sync, keeps contract + snapshot ----
        let path2 = temp_xlsx(
            "lifecycle.xlsx",
            &["型番", "数", "EC単価"],
            &[&["TEST-PART-999", "9", ""]],
        );
        assert_eq!(path2, path);
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(
            view.sync_status,
            SyncStatus::NeedsReview,
            "{:?}",
            view.verdict
        );
        assert!(matches!(view.verdict, StructureVerdict::Confirm { .. }));

        // Previous snapshot is returned, NOT the renamed file's rows (§9-20).
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
        // Contract untouched.
        let rec = store::get_link(&conn, &bom_id).unwrap().unwrap();
        assert!(rec
            .columns
            .iter()
            .any(|c| c.header_label.as_deref() == Some("数量")));

        // ---- Broken: required column gone (§9-21). NOTE: replacing the header
        // text in place reads as a RENAME candidate (Confirm, rule 8) — deleting
        // the COLUMN is what makes the required label unmatchable → Broken. ----
        temp_xlsx("lifecycle.xlsx", &["数量", "EC単価"], &[&["1", ""]]);
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Broken);
        assert!(matches!(view.verdict, StructureVerdict::Broken { .. }));

        // ---- back to pristine: recovers to Linked ----
        temp_xlsx(
            "lifecycle.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked);
        assert_eq!(view.doc.rows.len(), 1);

        // ---- unlink (§1.3): cache survives as a conventional BOM ----
        unlink(&mut conn, &bom_id).unwrap();
        assert!(store::get_link(&conn, &bom_id).unwrap().is_none());
        let frozen = crate::db::load_bom(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(frozen.rows.len(), 1);
        for table in ["bom_link_column", "bom_link_state", "bom_link_quote"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} not cascaded");
        }
        assert!(unlink(&mut conn, &bom_id).is_err()); // second unlink refused
    }

    /// New clean user column: auto-adopted into the contract (rule 7 / §9-19 side).
    #[test]
    fn safe_new_user_column_is_auto_adopted() {
        let mut conn = mem();
        let path = temp_xlsx(
            "newcol.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();

        temp_xlsx(
            "newcol.xlsx",
            &["型番", "数量", "EC単価", "備考"],
            &[&["TEST-PART-001", "2", "", "急ぎ"]],
        );
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked, "{:?}", view.verdict);
        match &view.verdict {
            StructureVerdict::Safe { new_columns } => {
                assert_eq!(new_columns, &vec!["備考".to_string()])
            }
            v => panic!("{v:?}"),
        }
        // Contract gained the user column; the composed doc carries its value.
        let rec = store::get_link(&conn, &bom_id).unwrap().unwrap();
        assert!(rec.columns.iter().any(
            |c| c.header_label.as_deref() == Some("備考") && c.ownership == LinkOwnership::User
        ));
        let col_key = rec
            .columns
            .iter()
            .find(|c| c.header_label.as_deref() == Some("備考"))
            .and_then(|c| c.app_key.clone())
            .unwrap();
        assert_eq!(
            view.doc.rows[0].custom.get(&col_key).map(String::as_str),
            Some("急ぎ")
        );
    }

    /// §9-19: row add/delete/reorder — the view is rebuilt from the CURRENT rows.
    #[test]
    fn rows_are_rebuilt_from_current_excel() {
        let mut conn = mem();
        let path = temp_xlsx(
            "rows.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "1", ""], &["TEST-PART-002", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();

        // Reorder + add + duplicate part number with different qty (§9-18 input).
        temp_xlsx(
            "rows.xlsx",
            &["型番", "数量", "EC単価"],
            &[
                &["TEST-PART-002", "2", ""],
                &["TEST-PART-001", "5", ""],
                &["TEST-PART-001", "1", ""],
            ],
        );
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked);
        let parts: Vec<_> = view
            .doc
            .rows
            .iter()
            .map(|r| (r.parts_no.clone().unwrap(), r.qty.unwrap()))
            .collect();
        assert_eq!(
            parts,
            vec![
                ("TEST-PART-002".into(), 2.0),
                ("TEST-PART-001".into(), 5.0),
                ("TEST-PART-001".into(), 1.0)
            ]
        );
    }

    /// §9-2: the app can read while another process holds the file open the way
    /// Excel does (write-denying share). All read channels open read-share.
    #[cfg(windows)]
    #[test]
    fn open_succeeds_while_file_is_held_open() {
        use std::os::windows::fs::OpenOptionsExt;
        let mut conn = mem();
        let path = temp_xlsx(
            "held.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();

        // FILE_SHARE_READ only: like Excel, others may read but not write.
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1)
            .open(&path)
            .unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked);
        assert_eq!(view.doc.rows.len(), 1);
    }

    /// Missing workbook → broken (fail closed) with the previous snapshot (§1.3).
    #[test]
    fn missing_workbook_is_broken_not_err() {
        let mut conn = mem();
        let path = temp_xlsx(
            "missing.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        std::fs::remove_file(&path).unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Broken);
        assert_eq!(view.doc.rows.len(), 1); // previous snapshot
        match view.verdict {
            StructureVerdict::Broken { reasons } => {
                assert!(reasons[0].contains("E_WORKBOOK_MISSING"))
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn double_link_and_bad_rows_are_rejected() {
        let mut conn = mem();
        let path = temp_xlsx(
            "double.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        let mut cfg = config(&path);
        cfg.bom_id = Some(bom_id);
        assert!(create_link(&mut conn, &cfg).is_err()); // already linked
        let mut cfg = config(&path);
        cfg.data_start_row = 1;
        assert!(create_link(&mut conn, &cfg).is_err()); // bad row order
    }
}
