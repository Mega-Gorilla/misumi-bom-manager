// Read side of Excel link mode (implementation.md §2.1 reader.rs): observe the
// workbook for the structure checker, and compose the BomDoc view (§4.9 rebuild —
// the current Excel rows are the source, never a diff against old DB rows).
//
// Two read channels on purpose:
// - calamine supplies VALUES and header labels (it resolves sharedStrings,
//   inlineStr, XML entities and ruby runs — the PoC walker's known gaps) plus
//   formula STRINGS via worksheet_formula. §3.4.1: the formula Range has its own
//   origin; every index MUST be offset by range.start() before pairing.
// - the xlsx.rs walker supplies formula POSITIONS (shared-formula followers
//   included) and per-cell value presence (missing `<v>` = §4.4 "missing").

use crate::excel_link::contract::{Contract, SheetObs};
use crate::excel_link::{calc_state, xlsx};
use crate::model::{BomDoc, BomMeta, BomRow, ColumnDef, FormulaCell, SupplierQuote};
use calamine::{open_workbook_auto, Data, Reader};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Read cap, aligned with the conventional import path (plan §3.2): files beyond
/// this are readable but truncated, and PR-4 refuses to write to them.
pub(crate) const MAX_ROWS: usize = 5000;

pub(crate) struct ReadOutcome {
    /// Observation for verify_structure, every sheet.
    pub sheets: Vec<(String, SheetObs)>,
    /// Target sheet's cell values as display strings: (row0, col0) -> value.
    pub values: BTreeMap<(u32, u32), String>,
    /// Formula cells of the target sheet (walker positions ∪ calamine strings).
    pub formula_cells: Vec<FormulaCell>,
    /// False when any formula cell in the target sheet's data area has no cached
    /// value (§4.4 missing — overrides everything in the restore rule).
    pub value_readable: bool,
    /// Last 1-based row with any content on the target sheet (before capping).
    pub last_data_row: u32,
    pub truncated: bool,
    /// workbook.xml calcMode when not the default ("manual" warns — §4.4.2).
    pub calc_mode: Option<String>,
}

fn data_to_string(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{}", *f as i64)
            } else {
                f.to_string()
            }
        }
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => b.to_string(),
        Data::Error(e) => format!("#ERR:{e:?}"),
        Data::DateTime(dt) => dt.to_string(),
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
    }
}

/// Observe the whole workbook for the checker and collect the target sheet's
/// values/formulas. Readable while Excel has the file open (§3.5 / §9-2) because
/// both channels open the file read-share.
pub(crate) fn observe(path: &Path, contract: &Contract) -> Result<ReadOutcome, String> {
    // ---- channel 1: calamine (labels + values + formula strings) ----
    let mut wb = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let sheet_names = wb.sheet_names().to_owned();

    let mut labels_by_sheet: BTreeMap<String, BTreeMap<u32, Vec<(u32, String)>>> = BTreeMap::new();
    let mut values: BTreeMap<(u32, u32), String> = BTreeMap::new();
    let mut last_data_row = 0u32;
    let mut truncated = false;
    for name in &sheet_names {
        let range = match wb.worksheet_range(name) {
            Ok(r) => r,
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let start = range.start().unwrap_or((0, 0));
        let labels = labels_by_sheet.entry(name.clone()).or_default();
        for (r, row) in range.rows().enumerate() {
            let abs_row0 = start.0 + r as u32;
            for (c, cell) in row.iter().enumerate() {
                let abs_col = start.1 + c as u32;
                if let Data::String(s) = cell {
                    labels
                        .entry(abs_row0 + 1) // SheetObs rows are 1-based
                        .or_default()
                        .push((abs_col, s.clone()));
                }
                if *name == contract.sheet_name {
                    if !matches!(cell, Data::Empty) {
                        last_data_row = last_data_row.max(abs_row0 + 1);
                    }
                    if (abs_row0 as usize) < MAX_ROWS {
                        let s = data_to_string(cell);
                        if !s.is_empty() {
                            values.insert((abs_row0, abs_col), s);
                        }
                    } else {
                        truncated = true;
                    }
                }
            }
        }
    }

    // Formula strings of the target sheet, offset-corrected (§3.4.1).
    let mut formula_strings: BTreeMap<(u32, u32), String> = BTreeMap::new();
    if sheet_names.contains(&contract.sheet_name) {
        let frange = wb
            .worksheet_formula(&contract.sheet_name)
            .map_err(|e| e.to_string())?;
        let fstart = frange.start().unwrap_or((0, 0));
        for (r, row) in frange.rows().enumerate() {
            for (c, f) in row.iter().enumerate() {
                if !f.is_empty() {
                    formula_strings.insert((fstart.0 + r as u32, fstart.1 + c as u32), f.clone());
                }
            }
        }
    }

    // ---- channel 2: xlsx walker (formula positions + value presence) ----
    let mut zip = xlsx::open_zip(path)?;
    let calc_mode = xlsx::calc_mode(&xlsx::read_part(&mut zip, "xl/workbook.xml")?);
    let parts = xlsx::sheet_parts(&mut zip)?;
    let mut sheets: Vec<(String, SheetObs)> = Vec::new();
    let mut formula_cells: Vec<FormulaCell> = Vec::new();
    let mut value_readable = true;
    for (name, part) in parts {
        let xml = match xlsx::read_part(&mut zip, &part) {
            Ok(x) => x,
            Err(_) => {
                // Sheet part absent — empty observation (PoC behaviour).
                sheets.push((name, SheetObs::default()));
                continue;
            }
        };
        let mut obs = SheetObs {
            labels: labels_by_sheet.remove(&name).unwrap_or_default(),
            ..Default::default()
        };
        let is_target = name == contract.sheet_name;
        for scan in xlsx::cells(&xml) {
            let Some((row0, col0)) = calc_state::parse_cell(&scan.cell_ref) else {
                continue;
            };
            let row1 = row0 + 1;
            if scan.has_formula {
                obs.formulas.push((row1, col0));
                if is_target {
                    formula_cells.push(FormulaCell {
                        row: row0 as i64,
                        col: col0 as i64,
                        formula: formula_strings.get(&(row0, col0)).cloned(),
                    });
                    if !scan.has_value && row1 >= contract.data_start_row {
                        value_readable = false; // §4.4 missing
                    }
                }
            }
            if scan.has_content() && row1 >= contract.data_start_row {
                obs.occupied_data_cols.insert(col0);
            }
        }
        sheets.push((name, obs));
    }

    Ok(ReadOutcome {
        sheets,
        values,
        formula_cells,
        value_readable,
        last_data_row,
        truncated,
        calc_mode,
    })
}

/// Core BomRow field keys (mirror of src/types/bom.ts core columns).
fn is_core(key: &str) -> bool {
    matches!(
        key,
        "no" | "partsName" | "partsNo" | "order" | "qty" | "material"
    )
}

/// §4.9 rebuild: compose the BomDoc view from the CURRENT Excel rows + the adopted
/// quote snapshot. App-owned Excel values are NEVER imported (rule 12) — supplier
/// data comes from bom_link_quote alone; handles duplicate part numbers per row.
pub(crate) fn compose(
    contract: &Contract,
    outcome: &ReadOutcome,
    quotes: &[crate::excel_link::store::QuoteRecord],
    meta: BomMeta,
    bom_id: &str,
) -> BomDoc {
    let quote_by_key: HashMap<(String, String), &crate::excel_link::store::QuoteRecord> = quotes
        .iter()
        .map(|q| ((q.supplier_code.clone(), q.parts_no.clone()), q))
        .collect();

    // Columns: contract order by excel_col; app-owned → supplier kind (values are
    // synthesized from quotes), user-owned → core/custom by key.
    let mut mapped: Vec<&crate::excel_link::contract::MappedColumn> =
        contract.mapped.iter().collect();
    mapped.sort_by_key(|m| m.excel_col);
    let columns: Vec<ColumnDef> = mapped
        .iter()
        .map(|m| ColumnDef {
            key: m.app_key.clone(),
            label: m.header_label.clone(),
            kind: if m.app_owned {
                "supplier".into()
            } else if is_core(&m.app_key) {
                "core".into()
            } else {
                "custom".into()
            },
            editable: !m.app_owned,
            width: None,
            link: None, // normalized for linked BOMs (§1.3) — projection lives in columns_meta
            role: m.role.clone(), // fetch-pipeline role must survive into the view (partNo/source/orderNo)
        })
        .collect();

    let data_rows: Vec<u32> = (contract.data_start_row..=outcome.last_data_row)
        .take(MAX_ROWS)
        .collect();
    let mut rows: Vec<BomRow> = Vec::new();
    for row1 in data_rows {
        let row0 = row1 - 1;
        let cell = |m: &crate::excel_link::contract::MappedColumn| -> Option<String> {
            outcome.values.get(&(row0, m.excel_col)).cloned()
        };
        let mut row = BomRow {
            id: format!("r{row1}"),
            ..Default::default()
        };
        let mut any = false;
        for m in &mapped {
            if m.app_owned {
                continue; // rule 12: app-owned Excel values are not imported
            }
            let Some(v) = cell(m) else { continue };
            any = true;
            match m.app_key.as_str() {
                "no" => row.no = v.parse().ok(),
                "partsName" => row.parts_name = Some(v),
                "partsNo" => row.parts_no = Some(v),
                "order" => row.order = Some(v),
                "qty" => row.qty = v.parse().ok(),
                "material" => row.material = Some(v),
                key => {
                    row.custom.insert(key.to_string(), v);
                }
            }
        }
        if !any {
            continue; // fully empty user cells → not a BOM row
        }
        // Adopted EC snapshot for this (supplier, part) if present.
        if let Some(parts_no) = row.parts_no.clone() {
            let supplier = row.order.clone().unwrap_or_else(|| "MISUMI".into());
            if let Some(q) = quote_by_key.get(&(supplier, parts_no)) {
                row.supplier = q
                    .payload_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str::<SupplierQuote>(s).ok());
            }
        }
        rows.push(row);
    }

    BomDoc {
        id: Some(bom_id.to_string()),
        version: 1,
        meta,
        columns,
        rows,
    }
}

/// Wizard probe of one sheet: preview grid + naive header suggestion.
pub(crate) fn probe_sheets(path: &Path) -> Result<Vec<crate::model::SheetProbe>, String> {
    const PREVIEW_ROWS: usize = 20;
    const PREVIEW_COLS: usize = 30;
    let mut wb = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names = wb.sheet_names().to_owned();
    let mut out = Vec::new();
    for name in names {
        let range = match wb.worksheet_range(&name) {
            Ok(r) => r,
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let start = range.start().unwrap_or((0, 0));
        let total_rows = range.rows().count();
        let mut preview: Vec<Vec<String>> = Vec::new();
        let mut best: Option<(usize, u32)> = None; // (string cells, 1-based row)
        for (r, row) in range.rows().enumerate().take(PREVIEW_ROWS) {
            let abs_row1 = start.0 + r as u32 + 1;
            let strings = row
                .iter()
                .filter(|c| matches!(c, Data::String(s) if !s.trim().is_empty()))
                .count();
            if strings > 0 && best.map(|(n, _)| strings > n).unwrap_or(true) {
                best = Some((strings, abs_row1));
            }
            preview.push(
                row.iter()
                    .take(PREVIEW_COLS)
                    .map(data_to_string)
                    .collect::<Vec<_>>(),
            );
        }
        out.push(crate::model::SheetProbe {
            name,
            preview,
            suggested_header_row: best.map(|(_, r)| r as i64),
            truncated: total_rows > MAX_ROWS,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excel_link::contract::Contract;
    use std::path::PathBuf;

    /// Real-Excel fixture (committed binary; generated once by
    /// tests/fixtures/gen-readfix.ps1 — see that script for the layout).
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/readfix.xlsx")
    }

    fn contract_for(sheet: &str) -> Contract {
        Contract {
            sheet_name: sheet.into(),
            header_row: 1,
            data_start_row: 2,
            mapped: vec![],
            skipped: vec![],
        }
    }

    fn has_formula_at(o: &ReadOutcome, row: i64, col: i64) -> bool {
        o.formula_cells.iter().any(|f| f.row == row && f.col == col)
    }

    fn formula_str(o: &ReadOutcome, row: i64, col: i64) -> Option<String> {
        o.formula_cells
            .iter()
            .find(|f| f.row == row && f.col == col)
            .and_then(|f| f.formula.clone())
    }

    /// §7.1.1 read-side permanent regressions, all four items on the real-Excel
    /// fixture. The coordinate-offset failure mode does NOT raise an error — it
    /// silently pairs the wrong value (§3.4.1) — so these assertions are the only
    /// thing standing between a regression and corrupted data.
    #[test]
    fn read_side_permanent_regressions() {
        let outcome = observe(&fixture(), &contract_for("BOM")).unwrap();

        // (1) Value range starts at A1, formula range at B1: with a naive
        // range-relative pairing, B1's formula would land on A1. The offset-fixed
        // reader reports B1=(0,1) with its own formula text.
        assert!(!has_formula_at(&outcome, 0, 0), "A1 is a plain value");
        assert_eq!(formula_str(&outcome, 0, 1).as_deref(), Some("A1*2"));
        assert!(formula_str(&outcome, 0, 2)
            .as_deref()
            .unwrap()
            .contains("CONCATENATE"));

        // (2) Sparse placement: H7 far away from the cluster.
        assert!(has_formula_at(&outcome, 6, 7), "sparse formula H7");
        assert!(!has_formula_at(&outcome, 5, 7), "no phantom above it");

        // (3) Multiple sheets: 'Other' is observed separately; its formula (C3)
        // appears in ITS observation and never leaks into the target's cells.
        let other = outcome
            .sheets
            .iter()
            .find(|(n, _)| n == "Other")
            .map(|(_, o)| o)
            .expect("second sheet observed");
        assert!(other.formulas.contains(&(3, 2)), "Other!C3 formula");
        assert!(
            !has_formula_at(&outcome, 2, 2),
            "BOM C3 is a value, not Other's formula"
        );

        // (4) Shared formula E2:E4 — the FOLLOWERS (E3/E4, stored as
        // <f t=\"shared\" si/>) must be formula positions too. Array formulas are
        // different OOXML: only the ANCHOR carries <f t=\"array\" ref>, follower
        // cells hold plain <v> — their protection is range-based (spill_ranges,
        // asserted in fixture_spill_ranges_are_readable), not per-cell.
        for row in 1..=3 {
            assert!(has_formula_at(&outcome, row, 4), "shared E{}", row + 1);
        }
        assert!(has_formula_at(&outcome, 1, 5), "array anchor F2");
        assert!(
            !has_formula_at(&outcome, 2, 5),
            "array follower F3 has no <f>"
        );
        assert!(has_formula_at(&outcome, 1, 6), "dynamic array anchor G2");

        // Values / bookkeeping.
        assert_eq!(
            outcome.values.get(&(1, 0)).map(String::as_str),
            Some("TEST-PART-001")
        );
        assert!(outcome.value_readable, "all formulas have cached values");
        assert!(!outcome.truncated);
        assert!(outcome.last_data_row >= 7);
        assert!(outcome.calc_mode.as_deref() != Some("manual"));
    }

    /// The array/spill ranges of the fixture are readable for the PR-4 write guard
    /// (fail-closed input): at least the legacy array F2:F4 must surface.
    #[test]
    fn fixture_spill_ranges_are_readable() {
        let mut zip = xlsx::open_zip(&fixture()).unwrap();
        let part = xlsx::sheet_part_for(&mut zip, "BOM").unwrap();
        let xml = xlsx::read_part(&mut zip, &part).unwrap();
        let (ranges, unresolved) = xlsx::spill_ranges(&xml);
        assert!(!unresolved);
        assert!(
            ranges.iter().any(|r| r == "F2:F4"),
            "array formula range readable: {ranges:?}"
        );
    }

    #[test]
    fn labels_flow_into_the_observation() {
        let outcome = observe(&fixture(), &contract_for("BOM")).unwrap();
        let bom = outcome
            .sheets
            .iter()
            .find(|(n, _)| n == "BOM")
            .map(|(_, o)| o)
            .unwrap();
        // Data-row string cell (calamine label channel, 1-based rows).
        assert!(bom
            .labels
            .get(&2)
            .map(|r| r.contains(&(0, "TEST-PART-001".into())))
            .unwrap_or(false));
        // occupied_data_cols respects data_start_row (row 1 cells alone don't count).
        assert!(bom.occupied_data_cols.contains(&0));
    }
}
