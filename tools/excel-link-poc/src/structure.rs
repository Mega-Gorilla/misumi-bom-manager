//! Structure-change verdict for the link mode (plan.md §4.9), step 3.
//!
//! The link-mode principle is "never guess an unknown structure into a sync". After the user
//! edits the workbook in Excel, the app re-reads it and must decide, mechanically:
//!
//!   Safe    - take the changes automatically (row edits, a clean new user column)
//!   Confirm - stop syncing, show candidates (columns moved, header renamed, sheet renamed,
//!             header row moved, app-owned column moved, data under a missing header)
//!   Broken  - do not ingest AND do not write (required column gone, duplicate headers,
//!             target sheet gone, app-owned column deleted/renamed, a formula typed into an
//!             app-owned column's data area)
//!
//! ANOMALY-COLLECTING design (PR #22 review finding 1): every check runs and records what it
//! found; the final verdict is decided by severity `Broken > Confirm > Safe`. An early-return
//! design let a Broken hide behind a Confirm on compound edits (e.g. header row moved AND a
//! formula typed into an app column — reproduced before the fix).
//!
//! Pure logic — no I/O, no Excel. The DECISION SKELETON is liftable into src-tauri, but the
//! Contract here is a PoC cut: the real structure contract (§4.7, DB migration V5) must key
//! columns by the stable `ColumnDef.key` + `role` + `link.field`, treat the header label as
//! "last confirmed display name" (not an identity), and carry per-column Excel positions and
//! skipped columns. See the README's "not proven" list.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    User,
    App,
}

/// PoC cut of the §4.7 structure contract (see module docs for what the real one needs).
pub struct Contract {
    pub sheet: &'static str,
    pub header_row: u32, // 1-based, as users see it
    /// (label, ownership) in contract column order, starting at column A.
    pub columns: &'static [(&'static str, Ownership)],
    /// Labels whose columns must exist for the BOM to be usable at all.
    pub required: &'static [&'static str],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Safe { new_user_columns: Vec<String> },
    Confirm(String),
    Broken(String),
}

/// What we observed in one worksheet. Built from the raw sheet XML by the caller
/// (`stale_scan::cells` + `shared_strings`); kept as plain data so tests can hand-craft it.
#[derive(Debug, Default, Clone)]
pub struct SheetObs {
    /// row (1-based) -> [(col 0-based, label)] for every string cell.
    pub labels: BTreeMap<u32, Vec<(u32, String)>>,
    /// (row 1-based, col 0-based) of every formula cell.
    pub formulas: Vec<(u32, u32)>,
    /// Column indices holding ANY real content (value or formula) on any row. Style-only cells
    /// do not count. Catches "data under a missing/unsupported header" (review finding 4).
    pub occupied_cols: BTreeSet<u32>,
}

impl SheetObs {
    fn row_labels(&self, row: u32) -> &[(u32, String)] {
        self.labels.get(&row).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

/// Does this row carry the full contract header (all labels present, any order)?
fn row_has_all_labels(c: &Contract, row: &[(u32, String)]) -> bool {
    c.columns
        .iter()
        .all(|(label, _)| row.iter().any(|(_, l)| l == label))
}

/// Position of a label within a row, if it appears exactly once.
fn unique_pos(row: &[(u32, String)], label: &str) -> Option<u32> {
    let mut hits = row.iter().filter(|(_, l)| l == label);
    let first = hits.next()?;
    if hits.next().is_some() {
        None // duplicated
    } else {
        Some(first.0)
    }
}

fn finalize(brokens: Vec<String>, confirms: Vec<String>, new_cols: Vec<String>) -> Verdict {
    if !brokens.is_empty() {
        Verdict::Broken(brokens.join("; "))
    } else if !confirms.is_empty() {
        Verdict::Confirm(confirms.join("; "))
    } else {
        Verdict::Safe {
            new_user_columns: new_cols,
        }
    }
}

/// §4.9 as code. `sheets` is every worksheet in the workbook (order irrelevant — resolution by
/// name / header constellation, never by position).
pub fn verify_structure(c: &Contract, sheets: &[(String, SheetObs)]) -> Verdict {
    let mut brokens: Vec<String> = Vec::new();
    let mut confirms: Vec<String> = Vec::new();

    // ---- Phase A: locate the target sheet -------------------------------------------------
    let obs = match sheets.iter().find(|(name, _)| name == c.sheet) {
        Some((_, obs)) => obs,
        None => {
            // Contract sheet is gone. Exactly one sheet carrying the header constellation means
            // "renamed" — still only a Confirm candidate, and the remaining checks CONTINUE on
            // that sheet so a Broken inside it is not masked.
            let candidates: Vec<&(String, SheetObs)> = sheets
                .iter()
                .filter(|(_, o)| o.labels.values().any(|row| row_has_all_labels(c, row)))
                .collect();
            match candidates.as_slice() {
                [one] => {
                    confirms.push(format!("target sheet renamed to '{}'", one.0));
                    &one.1
                }
                [] => return Verdict::Broken(format!("target sheet '{}' deleted", c.sheet)),
                _ => {
                    return Verdict::Broken(format!(
                        "target sheet '{}' deleted and header found on {} sheets (ambiguous)",
                        c.sheet,
                        candidates.len()
                    ))
                }
            }
        }
    };

    // ---- Phase B: find the effective header row -------------------------------------------
    // If the constellation moved to a different row, record a Confirm and keep checking ON THE
    // MOVED ROW — a duplicate header or an app-column formula there must still surface as Broken.
    let contract_row_ok = row_has_all_labels(c, obs.row_labels(c.header_row));
    let hdr_row = if contract_row_ok {
        c.header_row
    } else {
        let rows_with_header: Vec<u32> = obs
            .labels
            .iter()
            .filter(|(_, row)| row_has_all_labels(c, row))
            .map(|(r, _)| *r)
            .collect();
        match rows_with_header.as_slice() {
            [row] => {
                confirms.push(format!(
                    "header row moved: row {} -> row {} (user must confirm)",
                    c.header_row, row
                ));
                *row
            }
            [] => {
                // Constellation nowhere. Diagnose every missing column on the contract row —
                // and then KEEP CHECKING what can still be checked on the partially-recognised
                // row (PR #22 review round 2): a duplicate among the surviving headers or a
                // formula under a still-unique app column is a Broken and must not hide behind
                // the rename/disappearance Confirms.
                let contract_row = obs.row_labels(c.header_row);
                diagnose_missing(c, contract_row, &mut brokens, &mut confirms);
                partial_row_checks(c, obs, contract_row, &mut brokens, &mut confirms);
                return finalize(brokens, confirms, vec![]);
            }
            many => {
                brokens.push(format!(
                    "header constellation found on {} rows (ambiguous)",
                    many.len()
                ));
                return finalize(brokens, confirms, vec![]);
            }
        }
    };
    let row = obs.row_labels(hdr_row);

    // ---- Phase C: duplicated contract headers ---------------------------------------------
    let mut duplicated: BTreeSet<&str> = BTreeSet::new();
    for (label, _) in c.columns {
        if unique_pos(row, label).is_none() {
            brokens.push(format!("header '{label}' appears more than once"));
            duplicated.insert(label);
        }
    }

    // ---- Phase D: column positions (skip duplicated ones — no unique position exists) -----
    let mut moved: Vec<String> = Vec::new();
    let mut app_moved = false;
    let mut positions: BTreeMap<&str, u32> = BTreeMap::new();
    for (idx, (label, own)) in c.columns.iter().enumerate() {
        if duplicated.contains(label) {
            continue;
        }
        let pos = unique_pos(row, label).expect("non-duplicated label is unique");
        positions.insert(label, pos);
        if pos != idx as u32 && hdr_row == c.header_row {
            // (When the header row moved, columns usually keep their indices; only flag moves
            // relative to the contract order.)
            moved.push((*label).to_string());
            if *own == Ownership::App {
                app_moved = true;
            }
        } else if pos != idx as u32 {
            moved.push((*label).to_string());
            if *own == Ownership::App {
                app_moved = true;
            }
        }
    }
    if !moved.is_empty() {
        confirms.push(if app_moved {
            format!("app-owned column moved (with: {})", moved.join(", "))
        } else {
            format!("columns moved: {}", moved.join(", "))
        });
    }

    // ---- Phase E: formulas in app-owned data areas (below the EFFECTIVE header row) --------
    for (label, own) in c.columns {
        if *own != Ownership::App {
            continue;
        }
        let Some(col) = positions.get(label) else {
            continue; // duplicated — already Broken
        };
        if obs.formulas.iter().any(|(r, cc)| *r > hdr_row && cc == col) {
            brokens.push(format!(
                "formula found in app-owned column '{label}' data area"
            ));
        }
    }

    // ---- Phase F: extra labelled columns ---------------------------------------------------
    let known: Vec<&str> = c.columns.iter().map(|(l, _)| *l).collect();
    let app_labels: Vec<&str> = c
        .columns
        .iter()
        .filter(|(_, o)| *o == Ownership::App)
        .map(|(l, _)| *l)
        .collect();
    let mut new_cols: Vec<String> = Vec::new();
    let mut recognised_cols: BTreeSet<u32> = positions.values().copied().collect();
    for (col, label) in row {
        if known.contains(&label.as_str()) {
            continue;
        }
        recognised_cols.insert(*col);
        if label.trim().is_empty() {
            confirms.push("new column with empty header".into());
        } else if app_labels.contains(&label.as_str()) || new_cols.contains(label) {
            confirms.push(format!(
                "new column '{label}' conflicts with a reserved/duplicate name"
            ));
        } else {
            new_cols.push(label.clone());
        }
    }

    // ---- Phase G: occupied columns without a usable header (review finding 4) -------------
    // A column holding real content whose header cell is absent, non-string (number/bool/
    // formula) or otherwise unusable was previously invisible and slipped through as Safe.
    for col in &obs.occupied_cols {
        if !recognised_cols.contains(col) {
            confirms.push(format!(
                "column {} has data but no usable header (empty or unsupported header type)",
                col_name(*col)
            ));
        }
    }

    finalize(brokens, confirms, new_cols)
}

/// 0-based column index → A1-style letters, for messages.
fn col_name(mut col: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s
}

/// Checks that stay possible on a PARTIALLY recognised contract row (PR #22 review round 2):
/// the full constellation is gone, but a surviving header can still be duplicated, a still-
/// unique app-owned column can still hide a formula, and occupied columns without any label
/// still deserve a Confirm. Without this, those Brokens hid behind diagnose_missing's Confirms.
fn partial_row_checks(
    c: &Contract,
    obs: &SheetObs,
    row: &[(u32, String)],
    brokens: &mut Vec<String>,
    confirms: &mut Vec<String>,
) {
    // 1. duplicates among surviving contract headers
    for (label, _) in c.columns {
        let count = row.iter().filter(|(_, l)| l == label).count();
        if count > 1 {
            brokens.push(format!("header '{label}' appears more than once"));
        }
    }
    // 2. formulas under app-owned columns whose position is still unique
    for (label, own) in c.columns {
        if *own != Ownership::App {
            continue;
        }
        if let Some(col) = unique_pos(row, label) {
            if obs
                .formulas
                .iter()
                .any(|(r, cc)| *r > c.header_row && *cc == col)
            {
                brokens.push(format!(
                    "formula found in app-owned column '{label}' data area"
                ));
            }
        }
    }
    // 3. occupied columns with no label at all on the contract row
    let labelled: BTreeSet<u32> = row.iter().map(|(col, _)| *col).collect();
    for col in &obs.occupied_cols {
        if !labelled.contains(col) {
            confirms.push(format!(
                "column {} has data but no usable header (empty or unsupported header type)",
                col_name(*col)
            ));
        }
    }
}

/// The header constellation exists nowhere. Diagnose EVERY missing column (review finding 1:
/// evaluating only the first one let an app-column deletion hide behind a user rename).
fn diagnose_missing(
    c: &Contract,
    contract_row: &[(u32, String)],
    brokens: &mut Vec<String>,
    confirms: &mut Vec<String>,
) {
    let present: Vec<&str> = c
        .columns
        .iter()
        .map(|(l, _)| *l)
        .filter(|l| contract_row.iter().any(|(_, x)| x == l))
        .collect();
    if present.is_empty() {
        brokens.push("no contract header found anywhere".into());
        return;
    }

    for (idx, (label, own)) in c.columns.iter().enumerate() {
        if present.contains(label) {
            continue;
        }
        // A different label sitting exactly where the missing one used to be → rename candidate.
        let replacement = contract_row
            .iter()
            .find(|(col, l)| *col == idx as u32 && !c.columns.iter().any(|(cl, _)| cl == l))
            .map(|(_, l)| l.clone());
        match (replacement, own) {
            // Renaming an app-owned column is a sync-stop offence (§4.9), as is deleting it.
            (_, Ownership::App) => {
                brokens.push(format!("app-owned column '{label}' missing or renamed"))
            }
            (Some(new_label), Ownership::User) => confirms.push(format!(
                "header renamed? '{label}' -> '{new_label}' (user must confirm)"
            )),
            (None, Ownership::User) => {
                if c.required.contains(label) {
                    brokens.push(format!("required column '{label}' missing"));
                } else {
                    confirms.push(format!("column '{label}' disappeared"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gen-mutations.ps1 base: BOM sheet, header row 1,
    /// No / 型番 / 数量 / EC単価(App) / 小計 / 注文番号, required 型番+数量.
    const C: Contract = Contract {
        sheet: "BOM",
        header_row: 1,
        columns: &[
            ("No", Ownership::User),
            ("型番", Ownership::User),
            ("数量", Ownership::User),
            ("EC単価", Ownership::App),
            ("小計", Ownership::User),
            ("注文番号", Ownership::User),
        ],
        required: &["型番", "数量"],
    };

    fn header(labels: &[(u32, &str)]) -> Vec<(u32, String)> {
        labels.iter().map(|(c, l)| (*c, l.to_string())).collect()
    }

    fn base_obs() -> SheetObs {
        let mut labels = BTreeMap::new();
        labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数量"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        SheetObs {
            labels,
            // 小計 column (index 4) holds formulas in the data area — a USER column, allowed.
            formulas: (2..=6).map(|r| (r, 4)).collect(),
            occupied_cols: (0..=5).collect(),
        }
    }

    fn book(obs: SheetObs) -> Vec<(String, SheetObs)> {
        vec![("BOM".to_string(), obs)]
    }

    // ---- safe ----

    #[test]
    fn pristine_header_is_safe() {
        assert_eq!(
            verify_structure(&C, &book(base_obs())),
            Verdict::Safe {
                new_user_columns: vec![]
            }
        );
    }

    #[test]
    fn clean_new_user_column_is_safe() {
        let mut o = base_obs();
        o.labels.get_mut(&1).unwrap().push((6, "備考".into()));
        o.occupied_cols.insert(6);
        assert_eq!(
            verify_structure(&C, &book(o)),
            Verdict::Safe {
                new_user_columns: vec!["備考".into()]
            }
        );
    }

    // ---- confirm ----

    #[test]
    fn moved_columns_confirm() {
        let mut o = base_obs();
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (4, "数量"),
                (3, "EC単価"),
                (2, "小計"),
                (5, "注文番号"),
            ]),
        );
        o.formulas = (2..=6).map(|r| (r, 2)).collect();
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("moved"))
        );
    }

    #[test]
    fn renamed_user_header_confirms() {
        let mut o = base_obs();
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("renamed?"))
        );
    }

    #[test]
    fn renamed_sheet_confirms() {
        let o = base_obs();
        let sheets = vec![("BOM2".to_string(), o)];
        assert!(
            matches!(verify_structure(&C, &sheets), Verdict::Confirm(m) if m.contains("renamed to 'BOM2'"))
        );
    }

    #[test]
    fn moved_header_row_confirms() {
        let mut o = SheetObs::default();
        o.labels.insert(1, header(&[(0, "メモ")]));
        o.labels.insert(
            2,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数量"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        o.occupied_cols = (0..=5).collect();
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("header row moved"))
        );
    }

    #[test]
    fn headerless_data_column_confirms() {
        // Review finding 4: content in a column whose header is empty/non-string must NOT be Safe.
        let mut o = base_obs();
        o.occupied_cols.insert(6); // data in G, but no label anywhere for G
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("no usable header"))
        );
    }

    // ---- broken ----

    #[test]
    fn deleted_required_column_is_broken() {
        let mut o = base_obs();
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (2, "数量"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        assert!(matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("型番")));
    }

    #[test]
    fn duplicate_header_is_broken() {
        let mut o = base_obs();
        o.labels.get_mut(&1).unwrap().push((6, "型番".into()));
        o.occupied_cols.insert(6);
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("more than once"))
        );
    }

    #[test]
    fn deleted_sheet_is_broken() {
        let mut labels = BTreeMap::new();
        labels.insert(1, header(&[(0, "その他")]));
        let sheets = vec![(
            "Other".to_string(),
            SheetObs {
                labels,
                formulas: vec![],
                occupied_cols: BTreeSet::new(),
            },
        )];
        assert!(
            matches!(verify_structure(&C, &sheets), Verdict::Broken(m) if m.contains("deleted"))
        );
    }

    #[test]
    fn renamed_app_column_is_broken() {
        let mut o = base_obs();
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数量"),
                (3, "単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("app-owned"))
        );
    }

    #[test]
    fn formula_in_app_column_is_broken() {
        let mut o = base_obs();
        o.formulas.push((2, 3));
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("formula"))
        );
    }

    #[test]
    fn user_formula_columns_stay_safe() {
        assert!(matches!(
            verify_structure(&C, &book(base_obs())),
            Verdict::Safe { .. }
        ));
    }

    #[test]
    fn ambiguous_header_rows_broken() {
        let mut o = base_obs();
        o.labels.remove(&1);
        let hdr = header(&[
            (0, "No"),
            (1, "型番"),
            (2, "数量"),
            (3, "EC単価"),
            (4, "小計"),
            (5, "注文番号"),
        ]);
        o.labels.insert(3, hdr.clone());
        o.labels.insert(5, hdr);
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("ambiguous"))
        );
    }

    // ---- compound mutations (review finding 1): Broken must never hide behind Confirm ----

    #[test]
    fn moved_header_row_with_app_formula_is_broken() {
        // Reproduced pre-fix: this returned Confirm("header row moved") and masked the formula.
        let mut o = SheetObs::default();
        o.labels.insert(1, header(&[(0, "メモ")]));
        o.labels.insert(
            2,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数量"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        o.formulas.push((3, 3)); // formula in EC単価's data area, below the MOVED header
        o.occupied_cols = (0..=5).collect();
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("formula")),
            "Broken (app formula) must outrank Confirm (moved header row)"
        );
    }

    #[test]
    fn moved_header_row_with_duplicate_is_broken() {
        let mut o = SheetObs::default();
        o.labels.insert(1, header(&[(0, "メモ")]));
        let mut hdr = header(&[
            (0, "No"),
            (1, "型番"),
            (2, "数量"),
            (3, "EC単価"),
            (4, "小計"),
            (5, "注文番号"),
        ]);
        hdr.push((6, "型番".into())); // duplicate on the moved row
        o.labels.insert(2, hdr);
        o.occupied_cols = (0..=6).collect();
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("more than once"))
        );
    }

    #[test]
    fn user_rename_with_app_formula_is_broken() {
        // Review round 2: 数量→数 (rename, Confirm) AND a formula in EC単価's data area
        // (Broken). EC単価 survives at a unique position, so the formula IS checkable — the
        // partial-header path previously returned right after diagnose_missing and missed it.
        let mut o = base_obs();
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数"),
                (3, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        o.formulas.push((3, 3)); // formula under the still-unique app column
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("formula")),
            "app-column formula must outrank the rename Confirm"
        );
    }

    #[test]
    fn user_rename_with_duplicate_is_broken() {
        // Review round 2: 数量→数 (rename, Confirm) AND 型番 duplicated (Broken).
        let mut o = base_obs();
        let mut hdr = header(&[
            (0, "No"),
            (1, "型番"),
            (2, "数"),
            (3, "EC単価"),
            (4, "小計"),
            (5, "注文番号"),
        ]);
        hdr.push((6, "型番".into()));
        o.labels.insert(1, hdr);
        o.occupied_cols.insert(6);
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("more than once")),
            "duplicate header must outrank the rename Confirm"
        );
    }

    #[test]
    fn user_rename_with_app_deletion_is_broken() {
        // Review finding 1: diagnose_missing evaluated only the FIRST missing column, so a
        // user rename (Confirm) could mask a later app-column deletion (Broken).
        let mut o = base_obs();
        // 数量 renamed in place AND EC単価 deleted entirely.
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("app-owned")),
            "app-column deletion must outrank the user rename Confirm"
        );
    }
}
