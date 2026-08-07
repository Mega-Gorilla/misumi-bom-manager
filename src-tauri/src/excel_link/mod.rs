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
    // File IO targets the RESOLVED real path: a .lnk input is not a zip (review R2).
    let io_path = io_path_of(&env, p);
    let sheets = reader::probe_sheets(&io_path)?;
    let mut warnings = Vec::new();
    if let Ok(mut zip) = xlsx::open_zip(&io_path) {
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

/// Case-insensitive identity of a workbook for the 1-workbook=1-link guard: the
/// canonical resolved path when the environment check produced one (junction/.lnk
/// aliases collapse there), the absolute input path otherwise.
fn workbook_identity(resolved_path: Option<&str>, workbook_path: &str) -> String {
    resolved_path
        .map(str::to_string)
        .or_else(|| {
            std::path::absolute(workbook_path)
                .ok()
                .map(|p| env::strip_verbatim(&p))
        })
        .unwrap_or_else(|| workbook_path.to_string())
        .to_lowercase()
}

/// The path all file IO must use: the environment check's resolved real target
/// when available (collapses .lnk / junction doorways), the input otherwise.
fn io_path_of(env: &env::EnvCheck, input: &Path) -> std::path::PathBuf {
    env.resolved_path
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| input.to_path_buf())
}

/// Contract metadata for the frontend (role / EC projection per mapped column).
fn columns_meta(c: &Contract) -> Vec<crate::model::LinkColumnMeta> {
    let mut mapped: Vec<&contract::MappedColumn> = c.mapped.iter().collect();
    mapped.sort_by_key(|m| m.excel_col);
    mapped
        .into_iter()
        .map(|m| crate::model::LinkColumnMeta {
            app_key: m.app_key.clone(),
            excel_col: m.excel_col as i64,
            ownership: if m.app_owned {
                crate::model::LinkOwnership::App
            } else {
                crate::model::LinkOwnership::User
            },
            required: m.required,
            role: m.role.clone(),
            source_field: m.source_field.clone(),
            projection: m.projection,
        })
        .collect()
}

/// Merge cleanly-adopted new user columns (rule 7) into the stored column list.
fn adopt_new_columns(
    existing: &[store::LinkColumn],
    new_cols: &[contract::NewColumn],
) -> Vec<store::LinkColumn> {
    let mut columns = existing.to_vec();
    let mut keys: BTreeSet<String> = columns.iter().filter_map(|c| c.app_key.clone()).collect();
    for nc in new_cols {
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
    columns
}

/// Create a link (§2.3). ALL file reads and the structure verdict happen BEFORE any
/// DB write; creation then commits atomically (BOM row + contract + normalization +
/// display cache + state) — a failing/broken first read leaves the DB exactly as it
/// was. Creation requires a Safe verdict: a Confirm/Broken outcome right after the
/// wizard means the mapping is wrong, and the fix is re-mapping, not persisting a
/// stopped link.
pub fn create_link(
    conn: &mut Connection,
    config: &LinkCreateConfig,
) -> Result<LinkedBomView, String> {
    if config.data_start_row <= config.header_row || config.header_row < 1 {
        return Err("データ開始行はヘッダ行より下である必要があります".into());
    }
    let env = env::check_env(Path::new(&config.workbook_path));

    // BOM id (no insert yet — everything commits at the end).
    let (bom_id, meta, fresh_bom) = match &config.bom_id {
        Some(id) => {
            let doc = db_load(conn, id)?.ok_or_else(|| format!("BOM が見つかりません: {id}"))?;
            (id.clone(), doc.meta, false)
        }
        None => (
            crate::db::new_id(),
            BomMeta {
                name: config.name.clone(),
                imported_from: Some(config.workbook_path.clone()),
                qty_multiplier: 1.0,
                order_no_separator: None,
                updated_at: None,
            },
            true,
        ),
    };
    if store::get_link(conn, &bom_id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Err("この BOM は既に Excel にリンクされています".into());
    }
    // 1 workbook = 1 link: a shared workbook would poison the per-BOM fingerprints
    // (one BOM's write-back reads as the other's "Excel recalculated" and can
    // fabricate Trusted). Aliases (case, junction, .lnk) collapse via the resolved
    // identity.
    let identity = workbook_identity(env.resolved_path.as_deref(), &config.workbook_path);
    for (bid, wpath, rpath) in store::list_link_paths(conn).map_err(|e| e.to_string())? {
        if workbook_identity(rpath.as_deref(), &wpath) == identity {
            return Err(format!(
                "このワークブックは既に別の BOM ({bid}) にリンクされています (1ワークブック=1リンク)"
            ));
        }
    }

    // ---- reads + verdict, all before any DB write ----
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
    let transient_header = store::LinkHeader {
        bom_id: bom_id.clone(),
        workbook_path: config.workbook_path.clone(),
        sheet_name: config.sheet_name.clone(),
        header_row: config.header_row,
        data_start_row: config.data_start_row,
        env_verdict: env.verdict,
        env_resolved_path: env.resolved_path.clone(),
        env_fs_name: env.fs_name.clone(),
        env_checked_at: None,
        created_at: String::new(),
        updated_at: String::new(),
    };
    let base_contract =
        Contract::try_from_store(&transient_header, &columns).map_err(|e| e.to_string())?;
    // File IO targets the resolved real path (review R2: a .lnk input is not a zip).
    let io_path = io_path_of(&env, Path::new(&config.workbook_path));
    let path = io_path.as_path();
    if !path.exists() {
        return Err(format!(
            "リンク先ファイルが見つかりません: {}",
            config.workbook_path
        ));
    }
    let fp = fingerprint::stable_fingerprint(path, FP_RETRIES, FP_BACKOFF)
        .map_err(|e| format!("指紋の取得に失敗しました: {e}"))?;
    let outcome = reader::observe(path, &base_contract)?;
    let verify = contract::verify_structure(&base_contract, &outcome.sheets);
    if verify.severity() != Severity::Safe {
        return Err(format!(
            "リンク作成時の構造検証で問題が見つかりました。マッピングを見直して再実行してください: {}",
            verify
                .anomalies
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    // Adopt columns the wizard did not map (clean new user columns, rule 7).
    let merged = adopt_new_columns(&columns, &verify.new_columns);
    let merged_contract =
        Contract::try_from_store(&transient_header, &merged).map_err(|e| e.to_string())?;
    let doc = reader::compose(&merged_contract, &outcome, &[], meta.clone(), &bom_id);
    let persisted = calc_state::Persisted {
        last_app_write: None,
        recalc_requested: false,
        value_readable: outcome.value_readable,
    };
    let calc = calc_state::restore(&fp, &persisted);
    let mut warnings = read_warnings(&outcome);

    // ---- one transaction: BOM + contract + normalization + cache + state ----
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    if fresh_bom || config.bom_id.is_some() {
        crate::db::upsert_bom_meta(&tx, &bom_id, &meta).map_err(|e| e.to_string())?;
    }
    store::create_link(
        &tx,
        &store::NewLink {
            bom_id: bom_id.clone(),
            workbook_path: config.workbook_path.clone(),
            sheet_name: config.sheet_name.clone(),
            header_row: config.header_row,
            data_start_row: config.data_start_row,
            env_verdict: env.verdict,
            env_resolved_path: env.resolved_path.clone(),
            env_fs_name: env.fs_name.clone(),
            columns: merged,
        },
    )
    .map_err(|e| format!("リンク契約の保存に失敗しました: {e}"))?;
    // §1.3: the one-shot import link semantics do not survive continuous sync.
    crate::db::clear_column_links(&tx, &bom_id).map_err(|e| e.to_string())?;
    crate::db::write_columns_rows(&tx, &bom_id, &doc).map_err(|e| e.to_string())?;
    let mut st = store::get_state(&tx, &bom_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "リンク状態の初期化に失敗しました".to_string())?;
    st.sync_status = SyncStatus::Linked;
    st.calc_state = calc;
    st.last_read_fp = Some(fingerprint::to_hex(&fp));
    st.structure_fp = Some(verify.structure_fp.clone());
    st.data_first_row = Some(merged_contract.data_start_row as i64);
    st.data_last_row = Some(outcome.last_data_row as i64);
    st.row_count = Some(doc.rows.len() as i64);
    store::update_state(&tx, &bom_id, &st).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;

    if env.verdict == crate::model::EnvVerdict::NoWriteback {
        warnings.push(env_warning(&env));
    }
    Ok(LinkedBomView {
        doc,
        verdict: verify.to_verdict(&merged_contract),
        calc_state: calc,
        sync_status: SyncStatus::Linked,
        columns_meta: columns_meta(&merged_contract),
        env,
        formula_cells: outcome.formula_cells,
        truncated: outcome.truncated,
        warnings,
    })
}

fn read_warnings(outcome: &reader::ReadOutcome) -> Vec<String> {
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
    warnings
}

fn env_warning(env: &env::EnvCheck) -> String {
    format!(
        "この環境では Excel への書き戻しが無効です (読み取り専用リンク): {}",
        env.reason.as_deref().unwrap_or("環境判定により降格")
    )
}

/// Unlink (§1.3): one transaction — the display cache in bom_column/bom_row IS the
/// latest safe snapshot (open() persists it on every successful read), so freezing
/// it means simply deleting the bom_link row; contract/state/quotes/pending go via
/// FK cascade, the backup ledger stays.
pub fn unlink(conn: &mut Connection, bom_id: &str) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    // §1.3 "freeze as a conventional BOM": before the contract cascades away,
    // restore the conventional supplier-link semantics onto the frozen cache —
    // app-owned columns get their source_field back as bom_column.link_field so
    // cellDisplayValue keeps rendering the EC columns from the frozen
    // supplier_json (review R2: without this the EC columns go blank).
    tx.execute(
        "UPDATE bom_column SET            link_field = (SELECT lc.source_field FROM bom_link_column lc                           WHERE lc.bom_id = bom_column.bom_id                             AND lc.app_key = bom_column.key AND lc.ownership = 'app'),            link_write = 'overwrite'          WHERE bom_id = ?1 AND key IN            (SELECT app_key FROM bom_link_column WHERE bom_id = ?1 AND ownership = 'app')",
        [bom_id],
    )
    .map_err(|e| e.to_string())?;
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
    // Re-run the environment check on every open: file IO must target the RESOLVED
    // real path, and a retargeted shortcut must not silently swap the linked
    // workbook (review R2 — fail closed below).
    let env = env::check_env(Path::new(&rec.header.workbook_path));

    // Contract invariants + workbook presence: broken (fail closed), not Err —
    // the caller still gets the previous snapshot to display (§1.3).
    let contract = match Contract::try_from_store(&rec.header, &rec.columns) {
        Ok(c) => c,
        Err(e) => {
            return broken_view(conn, bom_id, rec.state, prev_doc, env, vec![e.to_string()]);
        }
    };
    // The retarget guard compares RESOLVED identities and therefore only fires when
    // the current check also resolved: an unresolvable-now path (file gone, .lnk
    // broken) is the MISSING/unreadable case below, and its fallback identity (raw
    // absolute path — possibly an 8.3 short form, as on CI runners) must not fake a
    // retarget.
    if rec.header.env_resolved_path.is_some() && env.resolved_path.is_some() {
        let stored = workbook_identity(
            rec.header.env_resolved_path.as_deref(),
            &rec.header.workbook_path,
        );
        let now = workbook_identity(env.resolved_path.as_deref(), &rec.header.workbook_path);
        if stored != now {
            return broken_view(
                conn,
                bom_id,
                rec.state,
                prev_doc,
                env,
                vec![format!(
                    "E_WORKBOOK_RETARGETED: リンク先の参照先が変更されています                      (登録時: {stored} → 現在: {now})。別のワークブックを自動採用しません"
                )],
            );
        }
    }
    let io_path = io_path_of(&env, Path::new(&rec.header.workbook_path));
    let path = io_path.as_path();
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

    let warnings = read_warnings(&outcome);

    match verify.severity() {
        Severity::Safe => {
            // ALL derivations happen before the transaction; the contract adoption
            // (rule 7), display cache and state then commit atomically — a failure
            // in any read leaves contract/cache/state untouched.
            let merged = adopt_new_columns(&rec.columns, &verify.new_columns);
            let contract =
                Contract::try_from_store(&rec.header, &merged).map_err(|e| e.to_string())?;
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

            let tx = conn.transaction().map_err(|e| e.to_string())?;
            if !verify.new_columns.is_empty() {
                store::replace_columns(&tx, bom_id, &merged).map_err(|e| e.to_string())?;
            }
            crate::db::write_columns_rows(&tx, bom_id, &doc).map_err(|e| e.to_string())?;
            store::update_state(&tx, bom_id, &st).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;

            Ok(LinkedBomView {
                doc,
                verdict: verify.to_verdict(&contract),
                calc_state: calc,
                sync_status: SyncStatus::Linked,
                columns_meta: columns_meta(&contract),
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
                columns_meta: columns_meta(&contract),
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
        columns_meta: vec![], // contract not interpretable in this state
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

    fn expect_err(r: Result<LinkedBomView, String>) -> String {
        match r {
            Err(e) => e,
            Ok(_) => panic!("expected Err"),
        }
    }

    // ---- review round 1 additions ----

    /// Contract metadata (role / source_field / projection) survives into the view:
    /// roles keep driving the existing partNo/source column lookups, and the EC
    /// projection is exposed via columns_meta (separate from the normalized
    /// ColumnDef.link — §1.3).
    #[test]
    fn contract_meta_survives_into_view() {
        let mut conn = mem();
        let path = temp_xlsx(
            "meta.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut cfg = config(&path);
        cfg.columns[0].role = Some("partNo".into());
        cfg.columns[1].role = Some("orderNo1".into());
        let view = create_link(&mut conn, &cfg).unwrap();

        // Roles survive into the composed BomDoc columns (fetch pipeline input).
        let col = |k: &str| {
            view.doc
                .columns
                .iter()
                .find(|c| c.key == k)
                .unwrap()
                .clone()
        };
        assert_eq!(col("partsNo").role.as_deref(), Some("partNo"));
        assert_eq!(col("qty").role.as_deref(), Some("orderNo1"));
        // ColumnDef.link stays normalized (no one-shot import policies).
        assert!(view.doc.columns.iter().all(|c| c.link.is_none()));

        // The EC projection is available via columns_meta.
        let ec = view
            .columns_meta
            .iter()
            .find(|m| m.app_key == "ecUnitPrice")
            .unwrap();
        assert_eq!(ec.source_field.as_deref(), Some("quote.unitPrice"));
        assert_eq!(ec.projection, Some(LinkProjection::Writeback));
        assert_eq!(ec.ownership, LinkOwnership::App);
        assert_eq!(ec.excel_col, 2);

        // And it round-trips through open (read back from the DB contract).
        let bom_id = view.doc.id.clone().unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(
            view.doc
                .columns
                .iter()
                .find(|c| c.key == "partsNo")
                .unwrap()
                .role
                .as_deref(),
            Some("partNo")
        );
        assert!(view
            .columns_meta
            .iter()
            .any(|m| m.source_field.as_deref() == Some("quote.unitPrice")));
    }

    /// 1 workbook = 1 link: a second BOM linking the same workbook is refused —
    /// shared fingerprints would let one BOM's write-back fabricate the other's
    /// Trusted state.
    #[test]
    fn same_workbook_cannot_link_twice() {
        let mut conn = mem();
        let path = temp_xlsx(
            "shared.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        create_link(&mut conn, &config(&path)).unwrap();
        // Identical path.
        let err = expect_err(create_link(&mut conn, &config(&path)));
        assert!(err.contains("1ワークブック=1リンク"), "{err}");
        // Case-different alias of the same file.
        let mut cfg = config(&path);
        cfg.workbook_path = cfg.workbook_path.to_uppercase();
        let err = expect_err(create_link(&mut conn, &cfg));
        assert!(err.contains("1ワークブック=1リンク"), "{err}");
    }

    /// Junction alias of the same workbook is also refused (identity is the
    /// resolved real path, not the doorway).
    #[cfg(windows)]
    #[test]
    fn same_workbook_via_junction_cannot_link_twice() {
        let mut conn = mem();
        let dir = std::env::temp_dir().join("mbm-link-tests-junc");
        let _ = std::fs::remove_dir_all(&dir);
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        for (c, h) in ["型番", "数量", "EC単価"].iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        ws.write_string(1, 0, "TEST-PART-001").unwrap();
        let path = real.join("bom.xlsx");
        wb.save(&path).unwrap();
        let junc = dir.join("junc");
        let ok = std::process::Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/J",
                &junc.to_string_lossy(),
                &real.to_string_lossy(),
            ])
            .status()
            .unwrap()
            .success();
        assert!(ok, "mklink /J failed");

        let mut cfg = config(&path);
        cfg.columns[0].required = false; // rows may be sparse; not the point here
        create_link(&mut conn, &cfg).unwrap();
        let mut cfg2 = config(&junc.join("bom.xlsx"));
        cfg2.columns[0].required = false;
        let err = expect_err(create_link(&mut conn, &cfg2));
        assert!(err.contains("1ワークブック=1リンク"), "{err}");
        let _ = std::fs::remove_dir(&junc);
    }

    /// Atomic creation: a failing first read (corrupt xlsx) or a non-Safe verdict
    /// persists NOTHING — no bom row, no link, no state (retry works cleanly).
    #[test]
    fn failed_create_leaves_no_trace() {
        let mut conn = mem();
        let dir = std::env::temp_dir().join("mbm-link-tests");
        std::fs::create_dir_all(&dir).unwrap();

        // (a) corrupt file: reads fail after the env check → Err, nothing persisted.
        let corrupt = dir.join("corrupt.xlsx");
        std::fs::write(&corrupt, b"this is not a zip").unwrap();
        assert!(create_link(&mut conn, &config(&corrupt)).is_err());

        // (b) mapping mismatch (required 型番 not present): non-Safe → Err.
        let wrong = temp_xlsx(
            "wrong.xlsx",
            &["品番", "数量", "EC単価"],
            &[&["x", "1", ""]],
        );
        let err = expect_err(create_link(&mut conn, &config(&wrong)));
        assert!(err.contains("構造検証"), "{err}");

        for table in ["bom", "bom_link", "bom_link_column", "bom_link_state"] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} must stay empty after failed create");
        }

        // (c) linking an EXISTING BOM against a corrupt file: the BOM survives
        // untouched, no link rows appear, and the display cache keeps its links.
        let doc = crate::model::BomDoc {
            id: Some("keep".into()),
            version: 1,
            meta: Default::default(),
            columns: vec![crate::model::ColumnDef {
                key: "partsNo".into(),
                label: "型番".into(),
                kind: "core".into(),
                editable: true,
                width: None,
                link: Some(crate::model::ColumnLink {
                    field: "product.name".into(),
                    write: "overwrite".into(),
                }),
                role: None,
            }],
            rows: vec![],
        };
        crate::db::save_bom(&mut conn, &doc).unwrap();
        let mut cfg = config(&corrupt);
        cfg.bom_id = Some("keep".into());
        assert!(create_link(&mut conn, &cfg).is_err());
        let kept = crate::db::load_bom(&conn, "keep").unwrap().unwrap();
        assert!(
            kept.columns[0].link.is_some(),
            "old link must NOT be cleared"
        );
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM bom_link", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// Atomic Safe adoption on open: when the read fails, no partial contract
    /// update leaks (contract, cache and state move together or not at all).
    #[test]
    fn failed_open_leaves_contract_cache_state_unchanged() {
        let mut conn = mem();
        let path = temp_xlsx(
            "atomic.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        let before_cols = store::get_link(&conn, &bom_id).unwrap().unwrap().columns;
        let before_state = store::get_state(&conn, &bom_id).unwrap().unwrap();

        // Corrupt the workbook: observe() fails BEFORE any write.
        std::fs::write(&path, b"broken zip").unwrap();
        assert!(open_link(&mut conn, &bom_id).is_err());

        let rec = store::get_link(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(rec.columns, before_cols);
        assert_eq!(
            store::get_state(&conn, &bom_id).unwrap().unwrap(),
            before_state
        );
        assert_eq!(
            crate::db::load_bom(&conn, &bom_id)
                .unwrap()
                .unwrap()
                .rows
                .len(),
            1
        );
    }

    // ---- review round 2 additions ----

    /// §1.3 freeze: after unlink, the frozen cache must still render EC columns —
    /// the contract's source_field returns to bom_column.link_field before the
    /// contract cascades away (review R2-1).
    #[test]
    fn unlink_restores_supplier_link_fields() {
        let mut conn = mem();
        let path = temp_xlsx(
            "unlinkmeta.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        store::record_quote_ok(&conn, &bom_id, "TEST-PART-001", &mk_quote("115"), 1).unwrap();
        open_link(&mut conn, &bom_id).unwrap();

        unlink(&mut conn, &bom_id).unwrap();

        // quote採用→open→unlink→bom_load: the EC column keeps its projection
        // (link_field restored) and the frozen row keeps its supplier payload, so
        // the conventional cellDisplayValue can still show 115.
        let frozen = crate::db::load_bom(&conn, &bom_id).unwrap().unwrap();
        let ec = frozen
            .columns
            .iter()
            .find(|c| c.key == "ecUnitPrice")
            .expect("EC column survives");
        let link = ec.link.as_ref().expect("link_field restored on unlink");
        assert_eq!(link.field, "quote.unitPrice");
        assert_eq!(
            frozen.rows[0]
                .supplier
                .as_ref()
                .and_then(|s| s.quote.as_ref())
                .and_then(|q| q.unit_price.as_deref()),
            Some("115")
        );
        // User columns stay unlinked.
        assert!(frozen
            .columns
            .iter()
            .filter(|c| c.key != "ecUnitPrice")
            .all(|c| c.link.is_none()));
    }

    /// .lnk end-to-end (review R2-2): probe/create/open resolve the shortcut for
    /// file IO, the direct path cannot be double-linked, and a retargeted shortcut
    /// is refused instead of silently adopting another workbook.
    #[cfg(windows)]
    #[test]
    fn lnk_probe_create_open_and_retarget_guard() {
        let mut conn = mem();
        let dir = std::env::temp_dir().join("mbm-link-tests-lnk");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let make_wb = |name: &str, part: &str| {
            let mut wb = rust_xlsxwriter::Workbook::new();
            let ws = wb.add_worksheet();
            for (c, h) in ["型番", "数量", "EC単価"].iter().enumerate() {
                ws.write_string(0, c as u16, *h).unwrap();
            }
            ws.write_string(1, 0, part).unwrap();
            ws.write_number(1, 1, 1.0).unwrap();
            let p = dir.join(name);
            wb.save(&p).unwrap();
            p
        };
        let real_a = make_wb("a.xlsx", "TEST-PART-001");
        let real_b = make_wb("b.xlsx", "TEST-PART-002");
        let lnk = dir.join("bom.lnk");
        crate::excel_link::env::write_lnk_for_tests(&lnk, &real_a);

        // probe reads the resolved xlsx, not the .lnk bytes.
        let probe = probe(&lnk.to_string_lossy()).unwrap();
        assert!(probe.sheets.iter().any(|s| s.name == "Sheet1"));
        assert!(probe.sheets[0]
            .preview
            .first()
            .map(|r| r.contains(&"型番".to_string()))
            .unwrap_or(false));

        // create + open through the shortcut work against the target xlsx.
        let cfg = config(&lnk);
        let view = create_link(&mut conn, &cfg).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked, "{:?}", view.verdict);
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
        let bom_id = view.doc.id.clone().unwrap();

        // The direct path is the same real workbook → 1 workbook = 1 link.
        let err = expect_err(create_link(&mut conn, &config(&real_a)));
        assert!(err.contains("1ワークブック=1リンク"), "{err}");

        // Retarget the shortcut to another xlsx: open must fail closed, not adopt.
        crate::excel_link::env::write_lnk_for_tests(&lnk, &real_b);
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Broken);
        match view.verdict {
            StructureVerdict::Broken { reasons } => {
                assert!(reasons[0].contains("E_WORKBOOK_RETARGETED"), "{reasons:?}")
            }
            v => panic!("{v:?}"),
        }
        // The previous snapshot is still what the user sees.
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
    }
}
