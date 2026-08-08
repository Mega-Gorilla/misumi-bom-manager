// Write path of Excel link mode = plan.md §4.2.2 steps 1-9 (measured in 3c, GO).
//
// This module owns steps 3-8: row-wise recomputation of app-owned values (§4.5),
// the write guards (formula cells, array/spill ranges — §4.4.2), the surgical temp
// generation, ReplaceFileW with backup, and the post-hoc backup/F0 comparison that
// closes the race window as optimistic concurrency control. Orchestration (steps
// 1-2 and 9, DB state) lives in mod.rs::apply_link.

use crate::excel_link::contract::{col_name, Contract};
use crate::excel_link::fingerprint::Fingerprint;
use crate::excel_link::reader::ReadOutcome;
use crate::excel_link::store::{AttemptStatus, QuoteRecord};
use crate::excel_link::{calc_state, fingerprint, xlsx};
use crate::model::RefuseReason;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Everything needed to execute the replace, computed BEFORE any file/DB change.
pub struct WritePlan {
    pub edits: BTreeMap<String, xlsx::CellValue>,
    /// The generation these values came from (state.ec_generation at plan time).
    pub generation: i64,
}

/// Steps 3-4: derive the app-owned cell values from the CURRENT rows (§4.5:
/// appOwnedValue = f(supplier, partNo, row qty, qtyMultiplier) — recomputed live,
/// never stamped) and run the §4.4.2 write guards against the sheet XML.
/// Failed/unadopted quote rows are skipped (§2.3 / §6.2.1 keep-and-warn).
pub(crate) fn plan_writeback(
    contract: &Contract,
    outcome: &ReadOutcome,
    quotes: &[QuoteRecord],
    qty_multiplier: f64,
    generation: i64,
    sheet_xml: &str,
) -> Result<Result<WritePlan, RefuseReason>, String> {
    if outcome.truncated {
        return Ok(Err(RefuseReason::Truncated)); // §3.2 / §9-23
    }

    // Fetch-key columns resolve via ROLE first, legacy core app_key as fallback —
    // the exact rule the fetch pipeline uses (partNoColumn/sourceColumn in
    // types/bom.ts, mirrored by Contract::part_no_column). Using fixed app_keys
    // here would make a role-moved BOM fetch by one column and write back by
    // another (PR-4 review #1).
    let part_col = contract.part_no_column();
    let source_col = contract.source_column();
    let qty_col = contract.mapped.iter().find(|m| m.app_key == "qty");
    let cell_of = |row0: u32, m: Option<&crate::excel_link::contract::MappedColumn>| {
        m.and_then(|m| outcome.values.get(&(row0, m.excel_col)).cloned())
    };

    // Adopted snapshots, parsed once. Only rows whose LATEST attempt succeeded are
    // written (§2.3: failed rows keep their previous Excel value and warn).
    let payload_by_key: HashMap<(String, String), serde_json::Value> = quotes
        .iter()
        .filter(|q| q.last_attempt_status == AttemptStatus::Ok)
        .filter_map(|q| {
            let v = serde_json::from_str(q.payload_json.as_deref()?).ok()?;
            Some(((q.supplier_code.clone(), q.parts_no.clone()), v))
        })
        .collect();

    let app_columns: Vec<_> = contract.mapped.iter().filter(|m| m.app_owned).collect();
    let mut edits: BTreeMap<String, xlsx::CellValue> = BTreeMap::new();
    let last = outcome.last_data_row;
    for row1 in contract.data_start_row..=last {
        let row0 = row1 - 1;
        let Some(parts_no) = cell_of(row0, part_col).filter(|p| !p.trim().is_empty()) else {
            continue;
        };
        let supplier = cell_of(row0, source_col)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "MISUMI".into());
        let Some(payload) = payload_by_key.get(&(supplier, parts_no)) else {
            continue; // no adopted success snapshot → skip the row's app cells
        };
        // Missing/unparseable Qty counts as 1 — `row.qty ?? 1` in columns.ts
        // (compose parses the same cell with `.parse().ok()`, so None there = 1 in
        // the UI too). An explicit 0 stays 0 (PR-4 review #2).
        let qty: f64 = cell_of(row0, qty_col)
            .and_then(|q| q.parse().ok())
            .unwrap_or(1.0);
        for m in &app_columns {
            let Some(source) = m.source_field.as_deref() else {
                continue;
            };
            if let Some(v) = resolve_value(payload, source, qty, qty_multiplier) {
                edits.insert(format!("{}{row1}", col_name(m.excel_col)), v);
            }
        }
    }
    if edits.is_empty() {
        return Ok(Err(RefuseReason::NothingToWrite));
    }

    // Step 4 guards (fail closed): never into a formula cell, never intersecting an
    // array/spill range, never when a range could not be identified.
    for cell_ref in edits.keys() {
        if xlsx::cell_has_formula(sheet_xml, cell_ref) {
            return Ok(Err(RefuseReason::FormulaCell));
        }
    }
    let (ranges, unresolved) = xlsx::spill_ranges(sheet_xml);
    let targets: Vec<&str> = edits.keys().map(String::as_str).collect();
    let range_refs: Vec<&str> = ranges.iter().map(String::as_str).collect();
    if calc_state::write_blocked(&targets, &range_refs, unresolved) {
        return Ok(Err(RefuseReason::Spill));
    }

    Ok(Ok(WritePlan { edits, generation }))
}

/// Resolve a contract source_field against the adopted payload + row context.
/// Two COMPUTED fields (documented in implementation.md §2.3) exist because their
/// §4.5 values depend on the row: `quote.subtotal` = unitPrice × qty × multiplier
/// (the exact formula the frontend uses live — columns.ts) and `quote.moqNote` =
/// a warning string when the row qty is under the MOQ (§9-18). Everything else is
/// a dotted path into the payload. Missing values skip the cell (no clobbering
/// with empties), except moqNote which must CLEAR a stale warning.
fn resolve_value(
    payload: &serde_json::Value,
    source_field: &str,
    qty: f64,
    qty_multiplier: f64,
) -> Option<xlsx::CellValue> {
    match source_field {
        "quote.subtotal" => {
            let unit: f64 = lookup(payload, "quote.unitPrice")?
                .as_str()
                .and_then(|s| s.parse().ok())?;
            // `qtyMultiplier || 1` in columns.ts: 0/NaN fall back to 1, any other
            // value (including negative) is used as-is (PR-4 review #2).
            let mult = if qty_multiplier == 0.0 || qty_multiplier.is_nan() {
                1.0
            } else {
                qty_multiplier
            };
            Some(xlsx::CellValue::Number(unit * qty * mult))
        }
        "quote.moqNote" => {
            let moq = lookup(payload, "quote.moq")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            // Float comparison like the UI (`qty < moq` in columns.ts) — an i64
            // cast would truncate 2.5 to 2 and change the verdict.
            if moq > 0 && qty < moq as f64 {
                Some(xlsx::CellValue::Text(format!("MOQ {moq} 未満")))
            } else {
                Some(xlsx::CellValue::Text(String::new())) // clear a stale warning
            }
        }
        path => {
            let v = lookup(payload, path)?;
            match v {
                serde_json::Value::Number(n) => n.as_f64().map(xlsx::CellValue::Number),
                serde_json::Value::String(s) => Some(match s.parse::<f64>() {
                    Ok(n) => xlsx::CellValue::Number(n),
                    Err(_) => xlsx::CellValue::Text(s.clone()),
                }),
                serde_json::Value::Bool(b) => Some(xlsx::CellValue::Text(b.to_string())),
                _ => None,
            }
        }
    }
}

fn lookup<'a>(payload: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut v = payload;
    for seg in path.split('.') {
        v = v.get(seg)?;
    }
    if v.is_null() {
        None
    } else {
        Some(v)
    }
}

/// Step 7: backup-carrying atomic replace (ReplaceFileW — measured on local NTFS
/// and the Drive mirror, §3.8/§3.12).
#[derive(Debug)]
pub enum ReplaceError {
    /// The write handle could not be taken (Excel holds the file) → Pending (§4.2.1).
    SharingViolation,
    Other(String),
}

#[cfg(windows)]
pub fn replace_with_backup(target: &Path, temp: &Path, backup: &Path) -> Result<(), ReplaceError> {
    use std::os::windows::ffi::OsStrExt;
    let wide = |p: &Path| -> Vec<u16> {
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (t, r, b) = (wide(target), wide(temp), wide(backup));
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::ReplaceFileW(
            t.as_ptr(),
            r.as_ptr(),
            b.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(32) | Some(33) => Err(ReplaceError::SharingViolation),
        _ => Err(ReplaceError::Other(format!(
            "ReplaceFileW failed: {err} (target={}, backup={})",
            target.display(),
            backup.display()
        ))),
    }
}

#[cfg(not(windows))]
pub fn replace_with_backup(
    _target: &Path,
    _temp: &Path,
    _backup: &Path,
) -> Result<(), ReplaceError> {
    Err(ReplaceError::Other(
        "backup-carrying atomic replace is Windows-only".into(),
    ))
}

/// Step 8: the post-hoc conflict check — hash(backup) vs F0 (§4.2.2). `.0` true =
/// the displaced content is NOT what the write was based on = conflict; the backup
/// is then the only copy of the external version and must never be auto-deleted.
/// `.1` is the backup's fingerprint (stored in the ledger, reused by the transfer
/// verification).
pub fn verify_backup(backup: &Path, f0: &Fingerprint) -> Result<(bool, Fingerprint), String> {
    let fp = fingerprint::file_fingerprint(backup)
        .map_err(|e| format!("backup の指紋取得に失敗しました: {e}"))?;
    Ok((fp != *f0, fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> serde_json::Value {
        json!({
            "supplierCode": "MISUMI",
            "status": "ok",
            "quote": { "unitPrice": "100", "moq": 3, "shipDate": "3日目" }
        })
    }

    #[test]
    fn resolve_value_computes_subtotal_from_row_context() {
        // §4.5 / §9-18: unitPrice × row qty × qtyMultiplier — the columns.ts formula.
        let v = resolve_value(&payload(), "quote.subtotal", 2.0, 2.0).unwrap();
        assert_eq!(v, xlsx::CellValue::Number(400.0));
        // `qtyMultiplier || 1` semantics (columns.ts): 0 and NaN fall back to 1,
        // anything else — negative included — is used as-is (PR-4 review #2).
        let v = resolve_value(&payload(), "quote.subtotal", 5.0, 0.0).unwrap();
        assert_eq!(v, xlsx::CellValue::Number(500.0));
        let v = resolve_value(&payload(), "quote.subtotal", 5.0, f64::NAN).unwrap();
        assert_eq!(v, xlsx::CellValue::Number(500.0));
        let v = resolve_value(&payload(), "quote.subtotal", 2.0, -1.0).unwrap();
        assert_eq!(v, xlsx::CellValue::Number(-200.0));
        // No unit price → skip the cell, never write 0.
        let p = json!({ "quote": {} });
        assert!(resolve_value(&p, "quote.subtotal", 2.0, 1.0).is_none());
    }

    #[test]
    fn resolve_value_moq_note_warns_below_and_clears_at_or_above() {
        let warn = resolve_value(&payload(), "quote.moqNote", 2.0, 1.0).unwrap();
        assert_eq!(warn, xlsx::CellValue::Text("MOQ 3 未満".into()));
        // At the MOQ (and above) the note must CLEAR — an empty string, not a skip,
        // so a stale warning written earlier disappears.
        let clear = resolve_value(&payload(), "quote.moqNote", 3.0, 1.0).unwrap();
        assert_eq!(clear, xlsx::CellValue::Text(String::new()));
        // No MOQ in the payload → nothing to warn about, still clears.
        let p = json!({ "quote": { "unitPrice": "1" } });
        assert_eq!(
            resolve_value(&p, "quote.moqNote", 1.0, 1.0).unwrap(),
            xlsx::CellValue::Text(String::new())
        );
    }

    #[test]
    fn resolve_value_dotted_paths_pick_number_vs_text() {
        // Numeric strings (MISUMI prices come as strings) are written as numbers.
        assert_eq!(
            resolve_value(&payload(), "quote.unitPrice", 0.0, 1.0).unwrap(),
            xlsx::CellValue::Number(100.0)
        );
        assert_eq!(
            resolve_value(&payload(), "quote.shipDate", 0.0, 1.0).unwrap(),
            xlsx::CellValue::Text("3日目".into())
        );
        assert_eq!(
            resolve_value(&payload(), "quote.moq", 0.0, 1.0).unwrap(),
            xlsx::CellValue::Number(3.0)
        );
        // Absent / null paths skip the cell.
        assert!(resolve_value(&payload(), "quote.stock", 0.0, 1.0).is_none());
        assert!(resolve_value(&payload(), "product.name", 0.0, 1.0).is_none());
    }

    // ---- plan_writeback guards (§4.2.2 steps 3-4, fail closed) ----

    fn contract3() -> Contract {
        use crate::excel_link::store::{LinkColumn, LinkHeader};
        use crate::model::{EnvVerdict, LinkOwnership, LinkProjection};
        let header = LinkHeader {
            bom_id: "B1".into(),
            workbook_path: "wb.xlsx".into(),
            sheet_name: "Sheet1".into(),
            header_row: 1,
            data_start_row: 2,
            env_verdict: EnvVerdict::Allow,
            env_resolved_path: None,
            env_fs_name: None,
            env_checked_at: None,
            created_at: "2026-08-01".into(),
            updated_at: "2026-08-01".into(),
        };
        let col = |excel_col: i64, key: &str, own, source: Option<&str>, proj| LinkColumn {
            excel_col,
            header_label: Some(key.into()),
            app_key: Some(key.into()),
            ownership: own,
            required: excel_col == 0,
            role: None,
            source_field: source.map(str::to_string),
            projection: proj,
        };
        let cols = vec![
            col(0, "partsNo", LinkOwnership::User, None, None),
            col(1, "qty", LinkOwnership::User, None, None),
            col(
                2,
                "ecUnitPrice",
                LinkOwnership::App,
                Some("quote.unitPrice"),
                Some(LinkProjection::Writeback),
            ),
        ];
        Contract::try_from_store(&header, &cols).unwrap()
    }

    fn outcome_rows(rows: &[(&str, &str)]) -> ReadOutcome {
        let mut values = std::collections::BTreeMap::new();
        let mut last = 1u32;
        for (i, (part, qty)) in rows.iter().enumerate() {
            let r0 = 1 + i as u32; // data_start_row=2 → first data row0 is 1
            values.insert((r0, 0u32), part.to_string());
            values.insert((r0, 1u32), qty.to_string());
            last = r0 + 1;
        }
        ReadOutcome {
            sheets: vec![],
            values,
            formula_cells: vec![],
            value_readable: true,
            last_data_row: last,
            truncated: false,
            calc_mode: None,
        }
    }

    fn quote_rec(part: &str, price: &str, ok: bool) -> QuoteRecord {
        QuoteRecord {
            supplier_code: "MISUMI".into(),
            parts_no: part.into(),
            payload_json: Some(json!({ "quote": { "unitPrice": price } }).to_string()),
            currency: Some("JPY".into()),
            fetched_at: Some("2026-08-01 09:00:00".into()),
            generation: Some(1),
            last_attempt_at: "2026-08-01 09:00:00".into(),
            last_attempt_status: if ok {
                AttemptStatus::Ok
            } else {
                AttemptStatus::Error
            },
            last_error_code: None,
            last_error_message: None,
        }
    }

    const PLAIN_SHEET: &str = r#"<sheetData><row r="2"><c r="A2" t="inlineStr"><is><t>TEST-PART-001</t></is></c><c r="B2"><v>2</v></c></row></sheetData>"#;

    #[test]
    fn plan_builds_edits_for_ok_rows_and_skips_failed_rows() {
        let c = contract3();
        let outcome = outcome_rows(&[("TEST-PART-001", "2"), ("TEST-PART-002", "3")]);
        // Row 2's latest attempt FAILED → its cell is skipped (§2.3 keep-and-warn).
        let quotes = vec![
            quote_rec("TEST-PART-001", "100", true),
            quote_rec("TEST-PART-002", "200", false),
        ];
        let plan = plan_writeback(&c, &outcome, &quotes, 1.0, 7, PLAIN_SHEET)
            .unwrap()
            .unwrap_or_else(|r| panic!("refused: {r:?}"));
        assert_eq!(plan.generation, 7);
        assert_eq!(plan.edits.len(), 1, "{:?}", plan.edits);
        assert_eq!(plan.edits.get("C2"), Some(&xlsx::CellValue::Number(100.0)));
    }

    /// PR-4 review #1: the partNo/source fetch keys must resolve via the ROLE
    /// columns (fallback core app_key) — the same rule as partNoColumn/sourceColumn
    /// on the frontend — or a role-moved BOM writes values fetched by one column
    /// using lookups against another.
    #[test]
    fn plan_resolves_part_and_source_via_role_columns() {
        use crate::excel_link::store::{LinkColumn, LinkHeader};
        use crate::model::{EnvVerdict, LinkOwnership, LinkProjection};
        let header = LinkHeader {
            bom_id: "B1".into(),
            workbook_path: "wb.xlsx".into(),
            sheet_name: "Sheet1".into(),
            header_row: 1,
            data_start_row: 2,
            env_verdict: EnvVerdict::Allow,
            env_resolved_path: None,
            env_fs_name: None,
            env_checked_at: None,
            created_at: "2026-08-01".into(),
            updated_at: "2026-08-01".into(),
        };
        let col = |excel_col: i64, key: &str, role: Option<&str>| LinkColumn {
            excel_col,
            header_label: Some(key.into()),
            app_key: Some(key.into()),
            ownership: LinkOwnership::User,
            required: false,
            role: role.map(str::to_string),
            source_field: None,
            projection: None,
        };
        let cols = vec![
            // A stale legacy partsNo column WITHOUT the role — must NOT be used.
            col(0, "partsNo", None),
            // The role-designated fetch keys live on custom columns.
            col(1, "modelCode", Some("partNo")),
            col(2, "srcCol", Some("source")),
            col(3, "qty", None),
            LinkColumn {
                excel_col: 4,
                header_label: Some("EC単価".into()),
                app_key: Some("ecUnitPrice".into()),
                ownership: LinkOwnership::App,
                required: false,
                role: None,
                source_field: Some("quote.unitPrice".into()),
                projection: Some(LinkProjection::Writeback),
            },
        ];
        let c = Contract::try_from_store(&header, &cols).unwrap();

        let mut values = std::collections::BTreeMap::new();
        values.insert((1u32, 0u32), "OLD-STALE".to_string()); // legacy column
        values.insert((1u32, 1u32), "TEST-PART-001".to_string()); // role: partNo
        values.insert((1u32, 2u32), "MISUMI".to_string()); // role: source
        values.insert((1u32, 3u32), "2".to_string());
        let outcome = ReadOutcome {
            sheets: vec![],
            values,
            formula_cells: vec![],
            value_readable: true,
            last_data_row: 2,
            truncated: false,
            calc_mode: None,
        };
        // Both parts have adopted snapshots — the written value proves which
        // column keyed the lookup.
        let quotes = vec![
            quote_rec("TEST-PART-001", "115", true),
            quote_rec("OLD-STALE", "999", true),
        ];
        let plan = plan_writeback(&c, &outcome, &quotes, 1.0, 1, PLAIN_SHEET)
            .unwrap()
            .unwrap_or_else(|r| panic!("refused: {r:?}"));
        assert_eq!(
            plan.edits.get("E2"),
            Some(&xlsx::CellValue::Number(115.0)),
            "must key on the role column, not the stale partsNo: {:?}",
            plan.edits
        );
    }

    /// PR-4 review #2: a missing/unparseable Qty cell counts as 1 (`row.qty ?? 1`
    /// in columns.ts — compose's `.parse().ok()` yields None for the same cells),
    /// so the subtotal is unitPrice × 1, never 0. An explicit 0 stays 0.
    #[test]
    fn plan_defaults_missing_qty_to_one() {
        use crate::excel_link::store::{LinkColumn, LinkHeader};
        use crate::model::{EnvVerdict, LinkOwnership, LinkProjection};
        let header = LinkHeader {
            bom_id: "B1".into(),
            workbook_path: "wb.xlsx".into(),
            sheet_name: "Sheet1".into(),
            header_row: 1,
            data_start_row: 2,
            env_verdict: EnvVerdict::Allow,
            env_resolved_path: None,
            env_fs_name: None,
            env_checked_at: None,
            created_at: "2026-08-01".into(),
            updated_at: "2026-08-01".into(),
        };
        let user = |excel_col: i64, key: &str| LinkColumn {
            excel_col,
            header_label: Some(key.into()),
            app_key: Some(key.into()),
            ownership: LinkOwnership::User,
            required: false,
            role: None,
            source_field: None,
            projection: None,
        };
        let cols = vec![
            user(0, "partsNo"),
            user(1, "qty"),
            LinkColumn {
                excel_col: 2,
                header_label: Some("小計".into()),
                app_key: Some("subtotal".into()),
                ownership: LinkOwnership::App,
                required: false,
                role: None,
                source_field: Some("quote.subtotal".into()),
                projection: Some(LinkProjection::Writeback),
            },
        ];
        let c = Contract::try_from_store(&header, &cols).unwrap();

        let mut values = std::collections::BTreeMap::new();
        values.insert((1u32, 0u32), "TEST-PART-001".to_string()); // row2: no qty cell
        values.insert((2u32, 0u32), "TEST-PART-001".to_string()); // row3: garbage qty
        values.insert((2u32, 1u32), "x個".to_string());
        values.insert((3u32, 0u32), "TEST-PART-001".to_string()); // row4: explicit 0
        values.insert((3u32, 1u32), "0".to_string());
        let outcome = ReadOutcome {
            sheets: vec![],
            values,
            formula_cells: vec![],
            value_readable: true,
            last_data_row: 4,
            truncated: false,
            calc_mode: None,
        };
        let quotes = vec![quote_rec("TEST-PART-001", "100", true)];
        let plan = plan_writeback(&c, &outcome, &quotes, 1.0, 1, PLAIN_SHEET)
            .unwrap()
            .unwrap_or_else(|r| panic!("refused: {r:?}"));
        assert_eq!(plan.edits.get("C2"), Some(&xlsx::CellValue::Number(100.0)));
        assert_eq!(plan.edits.get("C3"), Some(&xlsx::CellValue::Number(100.0)));
        assert_eq!(plan.edits.get("C4"), Some(&xlsx::CellValue::Number(0.0)));
    }

    #[test]
    fn plan_guards_fail_closed() {
        let c = contract3();
        let outcome = outcome_rows(&[("TEST-PART-001", "2")]);
        let quotes = vec![quote_rec("TEST-PART-001", "100", true)];
        let refused = |o: Result<Result<WritePlan, RefuseReason>, String>| match o.unwrap() {
            Err(r) => r,
            Ok(_) => panic!("expected a refusal"),
        };

        // Truncated wins before anything else (§9-23).
        let mut trunc = outcome_rows(&[("TEST-PART-001", "2")]);
        trunc.truncated = true;
        assert_eq!(
            refused(plan_writeback(&c, &trunc, &quotes, 1.0, 1, PLAIN_SHEET)),
            RefuseReason::Truncated
        );

        // A formula in the TARGET cell blocks the whole write (§4.4 / §9-13) —
        // the defense-in-depth layer under the structure gate.
        let sheet = r#"<sheetData><row r="2"><c r="A2"/><c r="B2"><v>2</v></c><c r="C2"><f>1+1</f><v>2</v></c></row></sheetData>"#;
        assert_eq!(
            refused(plan_writeback(&c, &outcome, &quotes, 1.0, 1, sheet)),
            RefuseReason::FormulaCell
        );

        // An array/spill range CROSSING the target cell blocks the write even
        // though C2 itself carries no formula (§4.4.2).
        let sheet = r#"<sheetData><row r="2"><c r="A2"/><c r="B2"><f t="array" ref="B2:C2">X</f><v>2</v></c></row></sheetData>"#;
        assert_eq!(
            refused(plan_writeback(&c, &outcome, &quotes, 1.0, 1, sheet)),
            RefuseReason::Spill
        );

        // An array formula whose range cannot be read → same refusal (fail closed).
        let sheet =
            r#"<sheetData><row r="2"><c r="B2"><f t="array">X</f><v>2</v></c></row></sheetData>"#;
        assert_eq!(
            refused(plan_writeback(&c, &outcome, &quotes, 1.0, 1, sheet)),
            RefuseReason::Spill
        );

        // No adopted snapshot at all → nothing to write, never an empty replace.
        assert_eq!(
            refused(plan_writeback(&c, &outcome, &[], 1.0, 1, PLAIN_SHEET)),
            RefuseReason::NothingToWrite
        );
    }

    // ---- §7.1.1 replace/verify: the post-hoc conflict check on real files ----

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("mbm-writeback-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(windows)]
    #[test]
    fn replace_detects_external_write_between_plan_and_replace() {
        let d = dir("conflict");
        let target = d.join("t.xlsx");
        let temp = d.join(".t.mbm-temp.xlsx");
        let backup = d.join("t.mbm-backup.xlsx");

        // F0 is the content the write plan was derived from.
        std::fs::write(&target, b"A0 original").unwrap();
        let f0 = fingerprint::file_fingerprint(&target).unwrap();
        std::fs::write(&temp, b"A1 app-written").unwrap();

        // The §7.1.1 race: an external writer lands F1 AFTER the plan (and after a
        // step-6 quick check would have passed), BEFORE the replace.
        std::fs::write(&target, b"F1 external").unwrap();
        let f1 = fingerprint::file_fingerprint(&target).unwrap();

        replace_with_backup(&target, &temp, &backup).unwrap();

        // Target now holds the app version, backup holds the DISPLACED external
        // version — and the backup/F0 comparison detects the conflict.
        assert_eq!(std::fs::read(&target).unwrap(), b"A1 app-written");
        assert_eq!(std::fs::read(&backup).unwrap(), b"F1 external");
        let (conflict, fp) = verify_backup(&backup, &f0).unwrap();
        assert!(conflict, "backup≠F0 must be detected");
        assert_eq!(fp, f1, "ledger stores the external version's fingerprint");

        // No race → backup == F0 → no conflict.
        std::fs::write(&temp, b"A2 second write").unwrap();
        let f_now = fingerprint::file_fingerprint(&target).unwrap();
        let backup2 = d.join("t.mbm-backup-2.xlsx");
        replace_with_backup(&target, &temp, &backup2).unwrap();
        let (conflict, fp) = verify_backup(&backup2, &f_now).unwrap();
        assert!(!conflict);
        assert_eq!(fp, f_now);
    }

    #[cfg(windows)]
    #[test]
    fn failed_replace_leaves_target_untouched() {
        // §7.1.1 item 6: an occupied backup destination must not damage the target.
        let d = dir("occupied");
        let target = d.join("t.xlsx");
        let temp = d.join(".t.mbm-temp.xlsx");
        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&temp, b"new").unwrap();
        let backup = d.join("blocked");
        std::fs::create_dir_all(&backup).unwrap(); // a DIRECTORY at the backup path

        let err = replace_with_backup(&target, &temp, &backup);
        assert!(matches!(err, Err(ReplaceError::Other(_))));
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"original",
            "target must be intact after a failed replace"
        );
    }

    #[cfg(windows)]
    #[test]
    fn replace_reports_sharing_violation_when_target_is_held() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = dir("held");
        let target = d.join("t.xlsx");
        let temp = d.join(".t.mbm-temp.xlsx");
        let backup = d.join("t.mbm-backup.xlsx");
        std::fs::write(&target, b"original").unwrap();
        std::fs::write(&temp, b"new").unwrap();

        // Exclusive handle (share_mode 0) = Excel-style lock on the workbook.
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&target)
            .unwrap();
        let err = replace_with_backup(&target, &temp, &backup);
        assert!(matches!(err, Err(ReplaceError::SharingViolation)));
        drop(_hold);
        assert_eq!(std::fs::read(&target).unwrap(), b"original");

        // Handle released → the same replace succeeds.
        replace_with_backup(&target, &temp, &backup).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }
}
