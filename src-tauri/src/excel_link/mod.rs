// Excel link mode (docs/plans/0018-excel-link-mode/ plan.md §4 + implementation.md §2.1).
//
// Module layout follows implementation.md §2.1. Shipped so far: persistence
// (`store` — PR-1), pure logic (`calc_state` / `fingerprint` / `env` core — PR-2),
// and the read-only link (env resolution, `xlsx` walkers, `contract` checker,
// `reader`, and the probe/create/open/unlink orchestration below — PR-3). Later
// PRs add: writeback / backup (PR-4), pending/confirm orchestration (PR-5),
// watch (PR-7).

pub mod backup;
pub mod calc_state;
pub mod contract;
pub mod env;
pub mod fingerprint;
pub mod reader;
pub mod store;
pub mod writeback;
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
    // File IO targets the READ target: a .lnk input is not a zip (review R2/R3).
    let Some(io_path) = io_path_of(&env, p) else {
        return Err(format!(
            "リンク先を解決できません: {}",
            env.reason.as_deref().unwrap_or("参照先が見つかりません")
        ));
    };
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

/// The path all file IO must use: the READ target (the .lnk chain followed and
/// canonicalized — available even in NoWriteback environments, §4.10 gates
/// writeback only). None = nothing readable: a .lnk whose target is gone must
/// never be fed to fingerprint/calamine/zip as if it were an xlsx (review R3).
fn io_path_of(env: &env::EnvCheck, input: &Path) -> Option<std::path::PathBuf> {
    if let Some(rt) = &env.read_target {
        return Some(std::path::PathBuf::from(rt));
    }
    let is_lnk = input
        .extension()
        .map(|e| e.eq_ignore_ascii_case("lnk"))
        .unwrap_or(false);
    if is_lnk {
        None // unresolvable shortcut — there is no workbook to read
    } else {
        Some(input.to_path_buf()) // plain path: let the exists-check report MISSING
    }
}

/// The identity a link stores/compares (1WB=1リンク guard, retarget guard): what the
/// BOM actually READS. Falls back to the verified path, then the raw input.
fn env_identity(env: &env::EnvCheck) -> Option<String> {
    env.read_target
        .clone()
        .or_else(|| env.resolved_path.clone())
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
    let identity = workbook_identity(env_identity(&env).as_deref(), &config.workbook_path);
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
    // File IO targets the read target (review R2/R3: a .lnk input is not a zip).
    let Some(io_path) = io_path_of(&env, Path::new(&config.workbook_path)) else {
        return Err(format!(
            "リンク先を解決できません: {}",
            env.reason.as_deref().unwrap_or("参照先が見つかりません")
        ));
    };
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
            env_resolved_path: env_identity(&env),
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
    if rec.header.env_resolved_path.is_some() && env_identity(&env).is_some() {
        let stored = workbook_identity(
            rec.header.env_resolved_path.as_deref(),
            &rec.header.workbook_path,
        );
        let now = workbook_identity(env_identity(&env).as_deref(), &rec.header.workbook_path);
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
    let Some(io_path) = io_path_of(&env, Path::new(&rec.header.workbook_path)) else {
        // A previously-resolved shortcut whose target is now gone/broken: fail
        // closed as Broken with the previous snapshot — never feed the .lnk bytes
        // to the readers, never surface a plain Err (review R3 case A).
        return broken_view(
            conn,
            bom_id,
            rec.state,
            prev_doc,
            env.clone(),
            vec![format!(
                "E_WORKBOOK_UNRESOLVABLE: リンク先を解決できません: {}",
                env.reason.as_deref().unwrap_or("参照先が見つかりません")
            )],
        );
    };
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
    let (fp, outcome) = read_stable(path, &contract)?;
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

// ---- write path orchestration (PR-4: §4.2.2 steps 1-2 and 9 around writeback) ----

const STABLE_READ_ATTEMPTS: usize = 3;

/// Sandwiched read (PR #29/#30 handover): fingerprint → both read channels →
/// fingerprint again; only identical before/after fingerprints prove the two
/// channels saw the same version. A writer racing us (share-permitting saves)
/// makes the sandwich differ → retry, then fail (never trust a torn read).
fn read_stable(
    path: &Path,
    contract: &Contract,
) -> Result<(fingerprint::Fingerprint, reader::ReadOutcome), String> {
    for _ in 0..STABLE_READ_ATTEMPTS {
        let fp1 = fingerprint::stable_fingerprint(path, FP_RETRIES, FP_BACKOFF)
            .map_err(|e| format!("指紋の取得に失敗しました: {e}"))?;
        let outcome = reader::observe(path, contract)?;
        let fp2 = fingerprint::file_fingerprint(path).map_err(|e| e.to_string())?;
        if fp1 == fp2 {
            return Ok((fp1, outcome));
        }
    }
    Err("安定した読み取りができませんでした (他プロセスが書き込み中の可能性があります)".into())
}

/// Adopt a quote run into the linked BOM's snapshot + advance the generation when
/// the snapshot MEANINGFULLY changed (implementation.md §2.3). `quotes` is one
/// entry per unique part number (the command's dedup unit). Returns None when the
/// BOM is not linked or nothing changed. Everything commits in ONE transaction.
pub fn adopt_quotes(
    conn: &mut Connection,
    bom_id: &str,
    quotes: &[(String, crate::model::SupplierQuote)],
) -> Result<Option<i64>, String> {
    let Some(rec) = store::get_link(conn, bom_id).map_err(|e| e.to_string())? else {
        return Ok(None); // conventional BOM: shared cache only, no snapshot
    };
    let prev: std::collections::HashMap<(String, String), String> =
        store::list_quotes(conn, bom_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|q| {
                let p = q.payload_json?;
                Some(((q.supplier_code, q.parts_no), p))
            })
            .collect();

    // Meaning of "changed" (implementation.md §2.3): the SEMANTIC payload —
    // fetched_at and raw are excluded, otherwise every re-fetch would advance the
    // generation and "全件同値は進めない" could never hold.
    fn semantic(v: &serde_json::Value) -> serde_json::Value {
        let mut v = v.clone();
        if let Some(o) = v.as_object_mut() {
            o.remove("fetchedAt");
            o.remove("raw");
        }
        v
    }
    let mut any_changed = false;
    let mut oks: Vec<&(String, crate::model::SupplierQuote)> = Vec::new();
    let mut errs: Vec<&(String, crate::model::SupplierQuote)> = Vec::new();
    for pair in quotes {
        let (part, q) = pair;
        if q.status == "ok" && q.errors.is_empty() && q.fetched_at.is_some() {
            let new_v = serde_json::to_value(q).map_err(|e| e.to_string())?;
            let changed = match prev.get(&(q.supplier_code.clone(), part.clone())) {
                None => true, // first adoption (cache hit or fresh — both count)
                Some(old) => {
                    let old_v: serde_json::Value =
                        serde_json::from_str(old).unwrap_or(serde_json::Value::Null);
                    semantic(&old_v) != semantic(&new_v)
                }
            };
            any_changed |= changed;
            oks.push(pair);
        } else {
            errs.push(pair);
        }
    }
    if oks.is_empty() && errs.is_empty() {
        return Ok(None);
    }

    let gen = if any_changed {
        rec.state.ec_generation + 1
    } else {
        rec.state.ec_generation
    };
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (part, q) in &oks {
        store::record_quote_ok(&tx, bom_id, part, q, gen).map_err(|e| e.to_string())?;
    }
    for (part, q) in &errs {
        let msg = if q.errors.is_empty() {
            "取得に失敗しました".to_string()
        } else {
            q.errors.join("; ")
        };
        store::record_quote_error(
            &tx,
            bom_id,
            &q.supplier_code,
            part,
            "quote_error",
            Some(&msg),
        )
        .map_err(|e| e.to_string())?;
    }
    if any_changed {
        let mut st = rec.state.clone();
        st.ec_generation = gen;
        store::update_state(&tx, bom_id, &st).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(any_changed.then_some(gen))
}

/// 「Excel へ反映」 = §4.2.2 steps 1-9 (measured in 3c). ALL reads, the verdict and
/// the write plan happen before any file/DB mutation; the replace itself is the
/// backup-carrying atomic swap whose backup/F0 comparison closes the race window.
pub fn apply_link(
    conn: &mut Connection,
    bom_id: &str,
    backup_dir: &Path,
) -> Result<crate::model::ApplyOutcome, String> {
    use crate::model::{ApplyOutcome, RefuseReason};

    let rec = store::get_link(conn, bom_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "この BOM は Excel にリンクされていません".to_string())?;
    let meta = db_load(conn, bom_id)?
        .ok_or_else(|| format!("BOM が見つかりません: {bom_id}"))?
        .meta;

    // Environment: writeback verdict + read target + retarget guard (same rules as
    // open — a NoWriteback environment or a swapped shortcut never gets written).
    let env = env::check_env(Path::new(&rec.header.workbook_path));
    if env.verdict == crate::model::EnvVerdict::NoWriteback {
        return Ok(ApplyOutcome::Refused {
            reason: RefuseReason::Env,
        });
    }
    if rec.header.env_resolved_path.is_some() && env_identity(&env).is_some() {
        let stored = workbook_identity(
            rec.header.env_resolved_path.as_deref(),
            &rec.header.workbook_path,
        );
        let now = workbook_identity(env_identity(&env).as_deref(), &rec.header.workbook_path);
        if stored != now {
            mark_stopped(
                conn,
                bom_id,
                &rec.state,
                SyncStatus::Broken,
                "E_WORKBOOK_RETARGETED",
            )?;
            return Ok(ApplyOutcome::Refused {
                reason: RefuseReason::Structure,
            });
        }
    }
    let Some(io_path) = io_path_of(&env, Path::new(&rec.header.workbook_path)) else {
        mark_stopped(
            conn,
            bom_id,
            &rec.state,
            SyncStatus::Broken,
            "E_WORKBOOK_UNRESOLVABLE",
        )?;
        return Ok(ApplyOutcome::Refused {
            reason: RefuseReason::Structure,
        });
    };
    let path = io_path.as_path();
    if !path.exists() {
        mark_stopped(
            conn,
            bom_id,
            &rec.state,
            SyncStatus::Broken,
            "E_WORKBOOK_MISSING",
        )?;
        return Ok(ApplyOutcome::Refused {
            reason: RefuseReason::Structure,
        });
    }

    let contract =
        Contract::try_from_store(&rec.header, &rec.columns).map_err(|e| e.to_string())?;

    // Steps 1-2 merged: the fresh sandwiched read IS the re-read, and comparing its
    // fingerprint against last_read_fp IS the §4.6.2 check — F0 is exactly the
    // fingerprint of the content the values below are derived from (rule 20).
    let (f0, outcome) = read_stable(path, &contract)?;
    let f0_hex = fingerprint::to_hex(&f0);
    if rec.state.last_read_fp.as_deref() != Some(f0_hex.as_str()) {
        return Ok(ApplyOutcome::Refused {
            reason: RefuseReason::FingerprintChanged, // §9-15/24: reload (open) first
        });
    }
    let verify = contract::verify_structure(&contract, &outcome.sheets);
    if verify.severity() != Severity::Safe {
        let (status, code) = if verify.severity() == Severity::Broken {
            (SyncStatus::Broken, "E_STRUCTURE_BROKEN")
        } else {
            (SyncStatus::NeedsReview, "E_STRUCTURE_NEEDS_REVIEW")
        };
        mark_stopped(conn, bom_id, &rec.state, status, code)?;
        return Ok(ApplyOutcome::Refused {
            reason: RefuseReason::Structure,
        });
    }

    // Steps 3-4: plan + guards (fail closed, no side effects yet).
    let mut zip = xlsx::open_zip(path)?;
    let sheet_part = xlsx::sheet_part_for(&mut zip, &contract.sheet_name)?;
    let sheet_xml = xlsx::read_part(&mut zip, &sheet_part)?;
    drop(zip);
    let quotes = store::list_quotes(conn, bom_id).map_err(|e| e.to_string())?;
    let plan = match writeback::plan_writeback(
        &contract,
        &outcome,
        &quotes,
        meta.qty_multiplier,
        rec.state.ec_generation,
        &sheet_xml,
    )? {
        Ok(p) => p,
        Err(reason) => return Ok(ApplyOutcome::Refused { reason }),
    };

    // Step 5: surgical temp in the SAME directory (ReplaceFileW volume constraint).
    let dir = path
        .parent()
        .ok_or("リンク先の親ディレクトリがありません")?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bom".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(".{stem}.mbm-temp-{nanos}.xlsx"));
    xlsx::write_patched(path, &temp, &sheet_part, &plan.edits, true)?;
    let new_fp = fingerprint::file_fingerprint(&temp).map_err(|e| e.to_string())?;

    // Step 6 (speed-up only, F0 is NOT re-taken — rule 20).
    match fingerprint::file_fingerprint(path) {
        Ok(now) if now == f0 => {}
        _ => {
            let _ = std::fs::remove_file(&temp);
            return Ok(ApplyOutcome::Refused {
                reason: RefuseReason::FingerprintChanged,
            });
        }
    }

    // Step 7: backup-carrying atomic replace.
    let backup_path = backup::unique_backup_path(dir, &stem);
    match writeback::replace_with_backup(path, &temp, &backup_path) {
        Ok(()) => {}
        Err(writeback::ReplaceError::SharingViolation) => {
            let _ = std::fs::remove_file(&temp);
            store::upsert_pending(conn, bom_id, plan.generation).map_err(|e| e.to_string())?;
            store::record_pending_attempt(conn, bom_id, "file_open").map_err(|e| e.to_string())?;
            return Ok(ApplyOutcome::Pending {
                reason: "fileOpen".into(),
            });
        }
        Err(writeback::ReplaceError::Other(e)) => {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
    }

    // Step 8: post-hoc conflict detection (backup vs F0).
    let (conflict, backup_fp) = writeback::verify_backup(&backup_path, &f0)?;

    // Step 9 + ledger, in ONE transaction (crash-safe: the ledger row exists from
    // the moment the backup file exists in the DB's view).
    let new_hex = fingerprint::to_hex(&new_fp);
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let now = crate::db::now_string(&tx).map_err(|e| e.to_string())?;
    let ledger_id = store::insert_backup(
        &tx,
        &store::NewBackup {
            origin_bom_id: bom_id.to_string(),
            workbook_path: rec.header.workbook_path.clone(),
            backup_path: backup_path.to_string_lossy().into_owned(),
            backup_fp: fingerprint::to_hex(&backup_fp),
            f0_fp: f0_hex.clone(),
        },
    )
    .map_err(|e| e.to_string())?;
    let mut st = rec.state.clone();
    st.last_app_write_fp = Some(new_hex.clone());
    st.last_app_write_at = Some(now.clone());
    st.last_read_fp = Some(new_hex.clone());
    st.last_read_at = Some(now);
    st.calc_state = crate::model::CalcState::Stale; // §4.4.1: every app write stales the caches
    st.recalc_requested = true;
    st.applied_generation = plan.generation;
    if conflict {
        st.sync_status = SyncStatus::Conflict;
        st.sync_error = Some("E_POST_REPLACE_CONFLICT".into());
    } else {
        st.sync_status = SyncStatus::Linked;
        st.sync_error = None;
    }
    store::update_state(&tx, bom_id, &st).map_err(|e| e.to_string())?;
    if let Some(p) = store::get_pending(&tx, bom_id).map_err(|e| e.to_string())? {
        if p.requested_generation <= plan.generation {
            store::clear_pending(&tx, bom_id).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;

    // §4.2.2 transfer (backup must not stay in a cloud-synced folder — case-08) +
    // retention. Failures are warnings: the write itself succeeded.
    let mut warnings = Vec::new();
    if let Err(w) = backup::transfer(conn, ledger_id, &backup_path, backup_dir) {
        warnings.push(w);
    }
    warnings.extend(backup::run_retention(conn, bom_id));

    let final_backup_path: String = conn
        .query_row(
            "SELECT backup_path FROM bom_link_backup WHERE id = ?1",
            [ledger_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if conflict {
        Ok(crate::model::ApplyOutcome::Conflict {
            backup_id: ledger_id,
            backup_path: final_backup_path,
            warnings,
        })
    } else {
        Ok(crate::model::ApplyOutcome::Applied {
            generation: plan.generation,
            fingerprint: new_hex,
            warnings,
        })
    }
}

/// Record a sync stop (broken/needs_review) discovered during apply.
fn mark_stopped(
    conn: &Connection,
    bom_id: &str,
    state: &store::LinkState,
    status: SyncStatus,
    code: &str,
) -> Result<(), String> {
    let mut st = state.clone();
    st.sync_status = status;
    st.sync_error = Some(code.to_string());
    store::update_state(conn, bom_id, &st).map_err(|e| e.to_string())
}

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

    // ---- review round 3 additions ----

    /// R3 case A: a linked .lnk whose TARGET disappears must become Broken with the
    /// previous snapshot — never a plain Err, and the .lnk bytes must never reach
    /// fingerprint/calamine/zip.
    #[cfg(windows)]
    #[test]
    fn lnk_with_deleted_target_is_broken_not_err() {
        let mut conn = mem();
        let dir = std::env::temp_dir().join("mbm-link-tests-lnkgone");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        for (c, h) in ["型番", "数量", "EC単価"].iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        ws.write_string(1, 0, "TEST-PART-001").unwrap();
        let real = dir.join("a.xlsx");
        wb.save(&real).unwrap();
        let lnk = dir.join("bom.lnk");
        crate::excel_link::env::write_lnk_for_tests(&lnk, &real);

        let view = create_link(&mut conn, &config(&lnk)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();

        std::fs::remove_file(&real).unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap(); // Broken, NOT Err
        assert_eq!(view.sync_status, SyncStatus::Broken);
        match view.verdict {
            StructureVerdict::Broken { reasons } => assert!(
                reasons[0].contains("E_WORKBOOK_UNRESOLVABLE"),
                "{reasons:?}"
            ),
            v => panic!("{v:?}"),
        }
        // Previous snapshot survives, and the state moved to broken.
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));
        let st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(st.sync_status, SyncStatus::Broken);
    }

    /// R3 case B: a workbook in a NoWriteback environment (here: reached through a
    /// symlinked directory) still supports probe/create/open as a READ-ONLY link —
    /// §4.10 degrades writeback only. Skips (recorded) when symlink creation needs
    /// privileges this environment does not have.
    #[cfg(windows)]
    #[test]
    fn nowriteback_environment_still_links_read_only() {
        let dir = std::env::temp_dir().join("mbm-link-tests-symread");
        let _ = std::fs::remove_dir_all(&dir);
        let real_dir = dir.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        let sym = dir.join("sym");
        match std::os::windows::fs::symlink_dir(&real_dir, &sym) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(1314) => {
                eprintln!("SKIP(symlink): ERROR_PRIVILEGE_NOT_HELD — read-only degradation is covered by env unit tests");
                return;
            }
            Err(e) => panic!("unexpected: {e}"),
        }
        let mut wb = rust_xlsxwriter::Workbook::new();
        let ws = wb.add_worksheet();
        for (c, h) in ["型番", "数量", "EC単価"].iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        ws.write_string(1, 0, "TEST-PART-001").unwrap();
        wb.save(real_dir.join("bom.xlsx")).unwrap();

        let via_sym = sym.join("bom.xlsx");
        // probe reads the sheet even though writeback is refused.
        let probe = probe(&via_sym.to_string_lossy()).unwrap();
        assert_eq!(probe.env.verdict, crate::model::EnvVerdict::NoWriteback);
        assert!(probe
            .env
            .reason
            .as_deref()
            .unwrap()
            .starts_with("env:symlink"));
        assert!(probe.sheets.iter().any(|s| s.name == "Sheet1"));

        // create + open succeed as a read-only link.
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&via_sym)).unwrap();
        assert_eq!(view.env.verdict, crate::model::EnvVerdict::NoWriteback);
        assert_eq!(view.sync_status, SyncStatus::Linked, "{:?}", view.verdict);
        assert!(view.warnings.iter().any(|w| w.contains("書き戻しが無効")));
        let bom_id = view.doc.id.clone().unwrap();
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.sync_status, SyncStatus::Linked);
        assert_eq!(view.doc.rows[0].parts_no.as_deref(), Some("TEST-PART-001"));

        // Reviewer-suggested combination pin: a .lnk POINTING UNDER the symlink —
        // the exact two-path mix behind review R3 — must behave identically
        // (read via the resolved real file, writeback refused, link works).
        let lnk = dir.join("via.lnk");
        crate::excel_link::env::write_lnk_for_tests(&lnk, &via_sym);
        let probe2 = super::probe(&lnk.to_string_lossy()).unwrap();
        assert_eq!(probe2.env.verdict, crate::model::EnvVerdict::NoWriteback);
        assert!(probe2.sheets.iter().any(|s| s.name == "Sheet1"));
        // Same real workbook → the 1-workbook=1-link guard must catch the alias.
        let err = expect_err(create_link(&mut conn, &config(&lnk)));
        assert!(err.contains("1ワークブック=1リンク"), "{err}");

        // PR-4: a NoWriteback environment refuses apply outright (§4.10 gates
        // writeback only — reading linked fine above).
        let out = apply_link(&mut conn, &bom_id, &dir.join("bk")).unwrap();
        assert!(matches!(
            out,
            crate::model::ApplyOutcome::Refused {
                reason: crate::model::RefuseReason::Env
            }
        ));

        let _ = std::fs::remove_dir(&sym);
    }

    // ---- PR-4: writeback (§4.2.2 steps 1-9) ----------------------------------

    use crate::model::{ApplyOutcome, CalcState, RefuseReason};

    fn apply_bk(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mbm-apply-bk-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        d // transfer() creates it
    }

    fn build_xlsx(
        name: &str,
        build: impl FnOnce(&mut rust_xlsxwriter::Workbook),
    ) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("mbm-link-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut wb = rust_xlsxwriter::Workbook::new();
        build(&mut wb);
        let _ = std::fs::remove_file(&path);
        wb.save(&path).unwrap();
        path
    }

    fn std_headers(ws: &mut rust_xlsxwriter::Worksheet) {
        for (c, h) in ["型番", "数量", "EC単価"].iter().enumerate() {
            ws.write_string(0, c as u16, *h).unwrap();
        }
    }

    fn sheet_xml_of(path: &Path, sheet: &str) -> String {
        let mut zip = xlsx::open_zip(path).unwrap();
        let part = xlsx::sheet_part_for(&mut zip, sheet).unwrap();
        xlsx::read_part(&mut zip, &part).unwrap()
    }

    fn fp_hex(path: &Path) -> String {
        fingerprint::to_hex(&fingerprint::file_fingerprint(path).unwrap())
    }

    fn adopt(conn: &mut Connection, bom_id: &str, quotes: &[(&str, SupplierQuote)]) -> Option<i64> {
        let v: Vec<(String, SupplierQuote)> = quotes
            .iter()
            .map(|(p, q)| (p.to_string(), q.clone()))
            .collect();
        adopt_quotes(conn, bom_id, &v).unwrap()
    }

    fn mk_quote_moq(price: &str, moq: i64) -> SupplierQuote {
        let mut q = mk_quote(price);
        q.quote.as_mut().unwrap().moq = Some(moq);
        q
    }

    fn mk_quote_err(msg: &str) -> SupplierQuote {
        SupplierQuote {
            supplier_code: "MISUMI".into(),
            status: "error".into(),
            product: None,
            quote: None,
            errors: vec![msg.into()],
            warnings: vec![],
            fetched_at: None,
            raw: None,
        }
    }

    /// No leftover work files matching both fragments beside the workbook.
    fn assert_no_litter(path: &Path, stem_frag: &str, kind_frag: &str) {
        let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains(stem_frag) && n.contains(kind_frag))
            .collect();
        assert!(leftovers.is_empty(), "litter: {leftovers:?}");
    }

    /// 正常系 (§4.2.2 手順1〜9) + §9-12 (restart keeps Stale) + §9-15/24 (external
    /// change refuses the next apply, file untouched).
    #[test]
    fn apply_writes_values_updates_state_and_transfers_backup() {
        let path = temp_xlsx(
            "apply-ok.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""], &["TEST-PART-002", "3", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        let f0_hex = fp_hex(&path);
        assert_eq!(
            adopt(
                &mut conn,
                &bom_id,
                &[
                    ("TEST-PART-001", mk_quote("115")),
                    ("TEST-PART-002", mk_quote("230"))
                ]
            ),
            Some(1)
        );

        let bk = apply_bk("ok");
        let out = apply_link(&mut conn, &bom_id, &bk).unwrap();
        let ApplyOutcome::Applied {
            generation,
            fingerprint,
            warnings,
        } = out
        else {
            panic!("expected Applied");
        };
        assert_eq!(generation, 1);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(fingerprint, fp_hex(&path), "fingerprint = the written file");

        // Values landed in the app-owned column (inserted cells, no styles before).
        let xml = sheet_xml_of(&path, "Sheet1");
        assert!(xml.contains(r#"<c r="C2"><v>115</v></c>"#), "{xml}");
        assert!(xml.contains(r#"<c r="C3"><v>230</v></c>"#), "{xml}");
        // fullCalcOnLoad forces recalculation on next Excel open (§4.4.1).
        let mut zip = xlsx::open_zip(&path).unwrap();
        let wb = xlsx::read_part(&mut zip, "xl/workbook.xml").unwrap();
        drop(zip);
        assert!(wb.contains(r#"fullCalcOnLoad="1""#));

        // Step 9 state: both fingerprints = the new file, Stale until Excel saves.
        let st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(st.last_app_write_fp.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(st.last_read_fp.as_deref(), Some(fingerprint.as_str()));
        assert_eq!(st.calc_state, CalcState::Stale);
        assert!(st.recalc_requested);
        assert_eq!(st.applied_generation, 1);
        assert_eq!(st.sync_status, SyncStatus::Linked);
        assert!(st.sync_error.is_none());

        // Ledger: one non-conflict backup, TRANSFERRED into app data (§4.2.2).
        let backups = store::list_backups(&conn, &bom_id).unwrap();
        assert_eq!(backups.len(), 1);
        let b = &backups[0];
        assert!(!b.is_conflict);
        assert_eq!(b.f0_fp, f0_hex);
        assert_eq!(b.location, crate::model::BackupLocation::AppData);
        assert!(Path::new(&b.backup_path).starts_with(&bk));
        assert!(Path::new(&b.backup_path).exists());
        // Nothing left beside the workbook (backup moved, temp consumed).
        assert_no_litter(&path, "apply-ok", "mbm-backup");
        assert_no_litter(&path, "apply-ok", "mbm-temp");

        // §9-12: reopening (= app restart) keeps Stale — only an Excel-saved read
        // may promote the calc state.
        let view = open_link(&mut conn, &bom_id).unwrap();
        assert_eq!(view.calc_state, CalcState::Stale);
        assert_eq!(view.sync_status, SyncStatus::Linked);

        // §9-15/24 (手順1): an external change after the last read refuses apply
        // and leaves the file byte-identical.
        temp_xlsx(
            "apply-ok.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-009", "9", ""]],
        );
        let external = fp_hex(&path);
        let out = apply_link(&mut conn, &bom_id, &bk).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::FingerprintChanged
            }
        ));
        assert_eq!(
            fp_hex(&path),
            external,
            "refused apply must not touch the file"
        );
    }

    /// Structure damage found during apply stops sync exactly like open does.
    #[test]
    fn apply_refuses_structure_change_and_stops_sync() {
        let path = temp_xlsx(
            "apply-structure.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);

        // Rename a user header (Confirm-level). Sync the stored fingerprint so
        // step 1 passes and the STRUCTURE check is what fires.
        temp_xlsx(
            "apply-structure.xlsx",
            &["型番", "数", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        st.last_read_fp = Some(fp_hex(&path));
        store::update_state(&conn, &bom_id, &st).unwrap();

        let out = apply_link(&mut conn, &bom_id, &apply_bk("structure")).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::Structure
            }
        ));
        let st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(st.sync_status, SyncStatus::NeedsReview);
        assert_eq!(st.sync_error.as_deref(), Some("E_STRUCTURE_NEEDS_REVIEW"));
    }

    #[test]
    fn apply_refuses_when_nothing_to_write() {
        let path = temp_xlsx(
            "apply-nothing.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        // No adopted snapshot → no edits → refuse, never an empty replace.
        let out = apply_link(&mut conn, &bom_id, &apply_bk("nothing")).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::NothingToWrite
            }
        ));
    }

    /// §4.4 / §9-13: a formula appearing in an app-owned column AFTER creation is
    /// caught by the STRUCTURE gate (E_FORMULA_IN_APP_COL is Broken) before the
    /// plan is even computed. The plan-level FormulaCell guard is defense in depth
    /// — covered directly by writeback::tests.
    #[test]
    fn apply_refuses_formula_appearing_in_app_column() {
        let path = temp_xlsx(
            "apply-formula.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);

        // User puts a formula into the EC column. Sync the stored fingerprint so
        // step 1 passes and the structure gate is what fires.
        build_xlsx("apply-formula.xlsx", |wb| {
            let ws = wb.add_worksheet();
            std_headers(ws);
            ws.write_string(1, 0, "TEST-PART-001").unwrap();
            ws.write_number(1, 1, 2.0).unwrap();
            ws.write_formula(1, 2, "=1+1").unwrap();
        });
        let mut st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        st.last_read_fp = Some(fp_hex(&path));
        store::update_state(&conn, &bom_id, &st).unwrap();
        let before = fp_hex(&path);

        let out = apply_link(&mut conn, &bom_id, &apply_bk("formula")).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::Structure
            }
        ));
        let st = store::get_state(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(st.sync_status, SyncStatus::Broken);
        assert_eq!(st.sync_error.as_deref(), Some("E_STRUCTURE_BROKEN"));
        assert_eq!(fp_hex(&path), before, "the formula must survive untouched");
    }

    /// §4.4.2 / §9-13: an array/spill range crossing a target cell blocks the write
    /// even though the target cell itself has no formula.
    #[test]
    fn apply_refuses_spill_intersection() {
        let path = build_xlsx("apply-spill.xlsx", |wb| {
            let ws = wb.add_worksheet();
            std_headers(ws);
            ws.write_string(1, 0, "TEST-PART-001").unwrap();
            // Anchor at B2 spilling across B2:C2 — C2 (the write target) is spill
            // area, not a formula cell.
            ws.write_dynamic_array_formula(1, 1, 1, 2, "=TRANSPOSE({2;0})")
                .unwrap();
        });
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);
        let out = apply_link(&mut conn, &bom_id, &apply_bk("spill")).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::Spill
            }
        ));
    }

    /// §3.2 / §9-23: a workbook over MAX_ROWS is readable-but-truncated — apply
    /// must refuse before computing any edit.
    #[test]
    fn apply_refuses_truncated_workbook() {
        let path = build_xlsx("apply-truncated.xlsx", |wb| {
            let ws = wb.add_worksheet();
            std_headers(ws);
            for r in 0..5001u32 {
                ws.write_string(r + 1, 0, format!("TEST-PART-{r:05}"))
                    .unwrap();
            }
        });
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        let out = apply_link(&mut conn, &bom_id, &apply_bk("truncated")).unwrap();
        assert!(matches!(
            out,
            ApplyOutcome::Refused {
                reason: RefuseReason::Truncated
            }
        ));
    }

    /// Excel holding the workbook → Pending (§4.2.1), retry after release applies.
    #[cfg(windows)]
    #[test]
    fn apply_pending_while_file_is_held_then_applies_after_release() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = temp_xlsx(
            "apply-pend.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);
        let before = fp_hex(&path);

        // Excel-style hold: others may READ (apply's own reads must work) but the
        // delete/rename access ReplaceFileW needs is denied.
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1) // FILE_SHARE_READ
            .open(&path)
            .unwrap();
        let bk = apply_bk("pend");
        let out = apply_link(&mut conn, &bom_id, &bk).unwrap();
        match out {
            ApplyOutcome::Pending { reason } => assert_eq!(reason, "fileOpen"),
            other => panic!("expected Pending, got {other:?}"),
        }
        let p = store::get_pending(&conn, &bom_id).unwrap().unwrap();
        assert_eq!(p.requested_generation, 1);
        assert_eq!(p.attempt_count, 1);
        assert_eq!(p.blocked_reason.as_deref(), Some("file_open"));
        assert_eq!(fp_hex(&path), before, "held file must be untouched");
        assert_no_litter(&path, "apply-pend", "mbm-temp");
        drop(hold);

        // Released → the same apply succeeds and the pending row clears.
        let out = apply_link(&mut conn, &bom_id, &bk).unwrap();
        assert!(matches!(out, ApplyOutcome::Applied { .. }));
        assert!(store::get_pending(&conn, &bom_id).unwrap().is_none());
    }

    /// §9-18: same part on two rows — subtotal is ROW-wise (qty × multiplier) and
    /// the MOQ note warns exactly the under-MOQ row while clearing the other.
    #[test]
    fn apply_writes_row_wise_subtotal_and_moq_note() {
        let path = temp_xlsx(
            "apply-s18.xlsx",
            &["型番", "数量", "小計", "MOQ"],
            &[
                &["TEST-PART-001", "2", "", ""],
                &["TEST-PART-001", "5", "", ""],
            ],
        );
        let col = |excel_col: i64,
                   label: &str,
                   key: &str,
                   own: LinkOwnership,
                   required: bool,
                   source: Option<&str>,
                   proj: Option<LinkProjection>| LinkColumnConfig {
            excel_col,
            header_label: Some(label.into()),
            app_key: Some(key.into()),
            ownership: own,
            required,
            role: None,
            source_field: source.map(str::to_string),
            projection: proj,
        };
        let cfg = LinkCreateConfig {
            bom_id: None,
            name: Some("s18".into()),
            workbook_path: path.to_string_lossy().into_owned(),
            sheet_name: "Sheet1".into(),
            header_row: 1,
            data_start_row: 2,
            columns: vec![
                col(0, "型番", "partsNo", LinkOwnership::User, true, None, None),
                col(1, "数量", "qty", LinkOwnership::User, false, None, None),
                col(
                    2,
                    "小計",
                    "subtotal",
                    LinkOwnership::App,
                    false,
                    Some("quote.subtotal"),
                    Some(LinkProjection::Writeback),
                ),
                col(
                    3,
                    "MOQ",
                    "moqNote",
                    LinkOwnership::App,
                    false,
                    Some("quote.moqNote"),
                    Some(LinkProjection::Writeback),
                ),
            ],
        };
        let mut conn = mem();
        let view = create_link(&mut conn, &cfg).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        conn.execute("UPDATE bom SET qty_multiplier = 2 WHERE id = ?1", [&bom_id])
            .unwrap();
        adopt(
            &mut conn,
            &bom_id,
            &[("TEST-PART-001", mk_quote_moq("100", 3))],
        );

        let out = apply_link(&mut conn, &bom_id, &apply_bk("s18")).unwrap();
        assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");
        let xml = sheet_xml_of(&path, "Sheet1");
        // 100 × 2 × 2 = 400 / 100 × 5 × 2 = 1000 (columns.ts の式・§4.5).
        assert!(xml.contains(r#"<c r="C2"><v>400</v></c>"#), "{xml}");
        assert!(xml.contains(r#"<c r="C3"><v>1000</v></c>"#), "{xml}");
        // qty2 < MOQ3 → warn; qty5 ≥ MOQ3 → CLEARED (empty inline string).
        assert!(
            xml.contains(r#"<c r="D2" t="inlineStr"><is><t>MOQ 3 未満</t></is></c>"#),
            "{xml}"
        );
        assert!(
            xml.contains(r#"<c r="D3" t="inlineStr"><is><t></t></is></c>"#),
            "{xml}"
        );
    }

    /// §9-4: user formulas (skipped column), a second sheet and cell styles all
    /// survive the surgical replace byte-for-byte.
    #[test]
    fn apply_preserves_formulas_second_sheet_and_styles() {
        let path = build_xlsx("apply-preserve.xlsx", |wb| {
            let bold = rust_xlsxwriter::Format::new().set_bold();
            let ws = wb.add_worksheet();
            std_headers(ws);
            ws.write_string(0, 3, "備考式").unwrap();
            ws.write_string(1, 0, "TEST-PART-001").unwrap();
            ws.write_number(1, 1, 2.0).unwrap();
            ws.write_blank(1, 2, &bold).unwrap(); // styled empty EC cell
            ws.write_formula(1, 3, "=B2*10").unwrap(); // user formula column
            let ws2 = wb.add_worksheet();
            ws2.write_formula(0, 0, "=1+2").unwrap();
        });
        let mut cfg = config(&path);
        cfg.columns.push(LinkColumnConfig {
            excel_col: 3,
            header_label: Some("備考式".into()),
            app_key: None,
            ownership: LinkOwnership::Skipped,
            required: false,
            role: None,
            source_field: None,
            projection: None,
        });
        let mut conn = mem();
        let view = create_link(&mut conn, &cfg).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);

        let out = apply_link(&mut conn, &bom_id, &apply_bk("preserve")).unwrap();
        assert!(matches!(out, ApplyOutcome::Applied { .. }), "{out:?}");

        let xml = sheet_xml_of(&path, "Sheet1");
        assert!(xml.contains("<f>B2*10</f>"), "user formula lost: {xml}");
        // The styled cell kept its s= attribute through the replace.
        assert!(xml.contains(r#"<c r="C2" s="#), "style lost: {xml}");
        assert!(xml.contains("<v>115</v>"), "{xml}");
        let xml2 = sheet_xml_of(&path, "Sheet2");
        assert!(xml2.contains("<f>1+2</f>"), "second sheet lost: {xml2}");
    }

    /// §9-22: a manual edit to an app-owned cell is NEVER adopted — the app keeps
    /// showing the snapshot and the next apply rewrites the cell.
    #[test]
    fn manual_edit_to_app_cell_is_not_adopted_and_gets_rewritten() {
        let path = temp_xlsx(
            "apply-manual.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]);
        let bk = apply_bk("manual");
        assert!(matches!(
            apply_link(&mut conn, &bom_id, &bk).unwrap(),
            ApplyOutcome::Applied { .. }
        ));

        // User hand-edits the EC cell to 999 and the app re-opens the link.
        temp_xlsx(
            "apply-manual.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", "999"]],
        );
        let view = open_link(&mut conn, &bom_id).unwrap();
        let q = view.doc.rows[0].supplier.as_ref().unwrap();
        assert_eq!(
            q.quote.as_ref().unwrap().unit_price.as_deref(),
            Some("115"),
            "compose must show the SNAPSHOT, not the manual 999 (§9-22)"
        );

        // Next apply rewrites the cell from the snapshot.
        assert!(matches!(
            apply_link(&mut conn, &bom_id, &bk).unwrap(),
            ApplyOutcome::Applied { .. }
        ));
        let xml = sheet_xml_of(&path, "Sheet1");
        assert!(xml.contains("<v>115</v>"), "{xml}");
        assert!(
            !xml.contains(">999<"),
            "manual value must be replaced: {xml}"
        );
    }

    /// implementation.md §2.3 の世代確定規則 (9項目).
    #[test]
    fn adopt_quotes_generation_rules() {
        let mut conn = mem();
        // (a) not a linked BOM → None, nothing recorded.
        assert_eq!(
            adopt(&mut conn, "no-such-bom", &[("P", mk_quote("1"))]),
            None
        );

        let path = temp_xlsx(
            "adopt-rules.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""], &["TEST-PART-002", "3", ""]],
        );
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let bom_id = view.doc.id.clone().unwrap();
        let gen_of = |conn: &Connection| {
            store::get_state(conn, &bom_id)
                .unwrap()
                .unwrap()
                .ec_generation
        };
        let row_of = |conn: &Connection, part: &str| {
            store::list_quotes(conn, &bom_id)
                .unwrap()
                .into_iter()
                .find(|q| q.parts_no == part)
                .unwrap()
        };

        // (b) first adoption advances: 0 → 1.
        assert_eq!(
            adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("115"))]),
            Some(1)
        );
        assert_eq!(row_of(&conn, "TEST-PART-001").generation, Some(1));

        // (c) same SEMANTIC content, different fetchedAt → no advance, but the
        // attempt/fetch bookkeeping still moves.
        let mut same = mk_quote("115");
        same.fetched_at = Some("2026-08-02 10:00:00".into());
        assert_eq!(adopt(&mut conn, &bom_id, &[("TEST-PART-001", same)]), None);
        assert_eq!(gen_of(&conn), 1);
        assert_eq!(
            row_of(&conn, "TEST-PART-001").fetched_at.as_deref(),
            Some("2026-08-02 10:00:00")
        );

        // (d) a value change advances: 1 → 2.
        assert_eq!(
            adopt(&mut conn, &bom_id, &[("TEST-PART-001", mk_quote("120"))]),
            Some(2)
        );

        // (e) first adoption of ANOTHER part — even a cache hit with an older
        // fetchedAt — is a change: 2 → 3.
        let mut cached = mk_quote("50");
        cached.fetched_at = Some("2026-07-01 00:00:00".into());
        assert_eq!(
            adopt(&mut conn, &bom_id, &[("TEST-PART-002", cached)]),
            Some(3)
        );
        assert_eq!(row_of(&conn, "TEST-PART-001").generation, Some(2)); // untouched

        // (f) failures alone never advance; the previous payload stays adopted and
        // the failure is persisted (§0 keep-and-warn across restarts).
        assert_eq!(
            adopt(
                &mut conn,
                &bom_id,
                &[("TEST-PART-001", mk_quote_err("timeout"))]
            ),
            None
        );
        assert_eq!(gen_of(&conn), 3);
        let r = row_of(&conn, "TEST-PART-001");
        assert_eq!(r.last_attempt_status, store::AttemptStatus::Error);
        assert_eq!(r.last_error_message.as_deref(), Some("timeout"));
        assert_eq!(
            r.generation,
            Some(2),
            "payload adoption survives the failure"
        );
        assert!(r.payload_json.as_deref().unwrap().contains("120"));

        // (g) partial success advances once; the failed row records in the SAME run.
        assert_eq!(
            adopt(
                &mut conn,
                &bom_id,
                &[
                    ("TEST-PART-001", mk_quote("130")),
                    ("TEST-PART-002", mk_quote_err("out of stock"))
                ]
            ),
            Some(4)
        );
        assert_eq!(row_of(&conn, "TEST-PART-001").generation, Some(4));
        let r2 = row_of(&conn, "TEST-PART-002");
        assert_eq!(r2.last_attempt_status, store::AttemptStatus::Error);
        assert!(r2.payload_json.as_deref().unwrap().contains("50"));

        // (h) another linked BOM has its own generation line.
        let path2 = temp_xlsx(
            "adopt-rules-2.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "1", ""]],
        );
        let view2 = create_link(&mut conn, &config(&path2)).unwrap();
        let bom2 = view2.doc.id.clone().unwrap();
        assert_eq!(
            adopt(&mut conn, &bom2, &[("TEST-PART-001", mk_quote("115"))]),
            Some(1)
        );
        assert_eq!(gen_of(&conn), 4, "other BOM's adoption must not leak in");

        // (i) an empty run is a no-op.
        assert_eq!(adopt(&mut conn, &bom_id, &[]), None);
        assert_eq!(gen_of(&conn), 4);
    }

    /// The sandwiched read returns the fingerprint of exactly the content both
    /// channels observed (PR #29/#30 の引き継ぎ安全要件).
    #[test]
    fn read_stable_returns_fingerprint_of_observed_content() {
        let path = temp_xlsx(
            "read-stable.xlsx",
            &["型番", "数量", "EC単価"],
            &[&["TEST-PART-001", "2", ""]],
        );
        let mut conn = mem();
        let view = create_link(&mut conn, &config(&path)).unwrap();
        let rec = store::get_link(&conn, &view.doc.id.clone().unwrap())
            .unwrap()
            .unwrap();
        let contract = Contract::try_from_store(&rec.header, &rec.columns).unwrap();
        let (fp, outcome) = read_stable(&path, &contract).unwrap();
        assert_eq!(fingerprint::to_hex(&fp), fp_hex(&path));
        assert_eq!(
            outcome.values.get(&(1, 0)).map(String::as_str),
            Some("TEST-PART-001")
        );
    }
}
