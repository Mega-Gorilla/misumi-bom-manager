//! Structure-change verdict for the link mode (plan.md §4.9), step 3.
//!
//! The link-mode principle is "never guess an unknown structure into a sync". After the user
//! edits the workbook in Excel, the app re-reads it and must decide, mechanically:
//!
//!   Safe    - take the changes automatically (row edits, a clean new user column)
//!   Confirm - stop syncing, show candidates (columns moved, header renamed, sheet renamed,
//!             header row moved, app-owned column moved)
//!   Broken  - do not ingest AND do not write (required column gone, duplicate headers,
//!             target sheet gone, app-owned column deleted/renamed, a formula typed into an
//!             app-owned column's data area)
//!
//! Pure logic — no I/O, no Excel — mirroring `state.rs`: unit-testable anywhere and liftable
//! into src-tauri as the structure-contract checker (§4.7). In the PoC the contract is a Rust
//! value; in the real implementation it lives in the DB (migration V5).

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    User,
    App,
}

/// The PoC cut of the §4.7 structure contract: what the app remembered about the workbook the
/// last time the link was confirmed.
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

/// §4.9 as code. `sheets` is every worksheet in the workbook, in workbook order.
pub fn verify_structure(c: &Contract, sheets: &[(String, SheetObs)]) -> Verdict {
    // ---- Phase A: locate the target sheet -------------------------------------------------
    let target = sheets.iter().find(|(name, _)| name == c.sheet);
    let (obs, via_rename) = match target {
        Some((_, obs)) => (obs, None),
        None => {
            // Contract sheet is gone. Look for its header constellation elsewhere — exactly one
            // candidate means "sheet renamed" (Confirm); zero or many means Broken.
            let candidates: Vec<&(String, SheetObs)> = sheets
                .iter()
                .filter(|(_, o)| o.labels.values().any(|row| row_has_all_labels(c, row)))
                .collect();
            match candidates.as_slice() {
                [one] => (&one.1, Some(one.0.clone())),
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

    // ---- Phase B: find the header row -----------------------------------------------------
    let contract_row = obs.row_labels(c.header_row);
    let header_row_here = row_has_all_labels(c, contract_row);

    // Full constellation on a DIFFERENT row → header row moved (Confirm) if unique.
    if !header_row_here {
        let rows_with_header: Vec<u32> = obs
            .labels
            .iter()
            .filter(|(_, row)| row_has_all_labels(c, row))
            .map(|(r, _)| *r)
            .collect();
        match rows_with_header.as_slice() {
            [row] => {
                return Verdict::Confirm(format!(
                    "header row moved: row {} -> row {} (user must confirm)",
                    c.header_row, row
                ))
            }
            [] => {
                // Not simply moved. Try to tell "renamed" apart from "gone" on the contract row.
                return diagnose_missing(c, contract_row);
            }
            _ => {
                return Verdict::Broken(format!(
                    "header constellation found on {} rows (ambiguous)",
                    rows_with_header.len()
                ))
            }
        }
    }

    // ---- Phase C: duplicates are Broken regardless of anything else ------------------------
    for (label, _) in c.columns {
        if unique_pos(contract_row, label).is_none() {
            return Verdict::Broken(format!("header '{label}' appears more than once"));
        }
    }

    // ---- Phase D: are the columns where the contract says? --------------------------------
    let mut moved: Vec<String> = Vec::new();
    let mut app_moved = false;
    let mut positions: BTreeMap<&str, u32> = BTreeMap::new();
    for (idx, (label, own)) in c.columns.iter().enumerate() {
        let pos = unique_pos(contract_row, label).expect("checked above");
        positions.insert(label, pos);
        if pos != idx as u32 {
            moved.push((*label).to_string());
            if *own == Ownership::App {
                app_moved = true;
            }
        }
    }

    // ---- Phase E: a formula typed into an app-owned column's data area is Broken ----------
    // (§4.9: the app would overwrite it, or the user is computing what the app owns.)
    for (label, own) in c.columns {
        if *own != Ownership::App {
            continue;
        }
        let col = positions[label];
        if obs
            .formulas
            .iter()
            .any(|(r, cc)| *r > c.header_row && *cc == col)
        {
            return Verdict::Broken(format!(
                "formula found in app-owned column '{label}' data area"
            ));
        }
    }

    if !moved.is_empty() {
        return Verdict::Confirm(if app_moved {
            format!("app-owned column moved (with: {})", moved.join(", "))
        } else {
            format!("columns moved: {}", moved.join(", "))
        });
    }

    // ---- Phase F: extra columns ------------------------------------------------------------
    let known: Vec<&str> = c.columns.iter().map(|(l, _)| *l).collect();
    let app_labels: Vec<&str> = c
        .columns
        .iter()
        .filter(|(_, o)| *o == Ownership::App)
        .map(|(l, _)| *l)
        .collect();
    let mut new_cols: Vec<String> = Vec::new();
    for (_, label) in contract_row {
        if known.contains(&label.as_str()) {
            continue;
        }
        if label.trim().is_empty() {
            return Verdict::Confirm("new column with empty header".into());
        }
        if app_labels.contains(&label.as_str()) || new_cols.contains(label) {
            return Verdict::Confirm(format!(
                "new column '{label}' conflicts with a reserved/duplicate name"
            ));
        }
        new_cols.push(label.clone());
    }

    // Row adds/deletes/reorders never reach this function: the view is rebuilt from current
    // rows (§4.5/§4.9), so a clean header IS the safe verdict.
    Verdict::Safe {
        new_user_columns: new_cols,
    }
    .with_rename_note(via_rename)
}

impl Verdict {
    /// A sheet located via rename is still Confirm even if its header is pristine.
    fn with_rename_note(self, via_rename: Option<String>) -> Verdict {
        match via_rename {
            None => self,
            Some(name) => match self {
                Verdict::Broken(b) => Verdict::Broken(b),
                _ => Verdict::Confirm(format!("target sheet renamed to '{name}'")),
            },
        }
    }
}

/// The contract row does not carry the full constellation and no other row does either.
/// Distinguish "renamed in place" (Confirm / Broken-if-app) from "column gone" (Broken).
fn diagnose_missing(c: &Contract, contract_row: &[(u32, String)]) -> Verdict {
    let present: Vec<&str> = c
        .columns
        .iter()
        .map(|(l, _)| *l)
        .filter(|l| contract_row.iter().any(|(_, x)| x == l))
        .collect();
    let missing: Vec<(&str, Ownership, u32)> = c
        .columns
        .iter()
        .enumerate()
        .filter(|(_, (l, _))| !present.contains(l))
        .map(|(i, (l, o))| (*l, *o, i as u32))
        .collect();

    if present.is_empty() {
        return Verdict::Broken("no contract header found anywhere".into());
    }

    // Diagnose the FIRST missing column: one broken/ambiguous column is enough to stop the
    // sync, and a multi-column diagnosis would only be as good as its weakest guess.
    let Some((label, own, idx)) = missing.first() else {
        return Verdict::Broken("structure not recognizable".into());
    };
    // A different label sitting exactly where the missing one used to be → rename candidate.
    let replacement = contract_row
        .iter()
        .find(|(col, l)| col == idx && !c.columns.iter().any(|(cl, _)| cl == l))
        .map(|(_, l)| l.clone());
    match (replacement, own) {
        // Renaming an app-owned column is a sync-stop offence (§4.9), as is deleting it.
        (_, Ownership::App) => {
            Verdict::Broken(format!("app-owned column '{label}' missing or renamed"))
        }
        (Some(new_label), Ownership::User) => Verdict::Confirm(format!(
            "header renamed? '{label}' -> '{new_label}' (user must confirm)"
        )),
        (None, Ownership::User) => {
            if c.required.contains(label) {
                Verdict::Broken(format!("required column '{label}' missing"))
            } else {
                Verdict::Confirm(format!("column '{label}' disappeared"))
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
            // 小計 column (index 4) holds formulas in the data area — that is a USER column,
            // allowed. Rows 2..=6.
            formulas: (2..=6).map(|r| (r, 4)).collect(),
        }
    }

    fn book(obs: SheetObs) -> Vec<(String, SheetObs)> {
        vec![("BOM".to_string(), obs)]
    }

    // ---- safe ----

    #[test]
    fn pristine_header_is_safe() {
        // Row adds/deletes/reorders never alter the header, so this is also the verdict for
        // safe-add-row / safe-del-row / safe-reorder-rows.
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
        // Swap 数量 (2) and 小計 (4).
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
        o.formulas = (2..=6).map(|r| (r, 2)).collect(); // 小計 formulas moved with it
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("moved"))
        );
    }

    #[test]
    fn moved_app_column_confirms_with_app_reason() {
        let mut o = base_obs();
        // EC単価 moves to the end.
        o.labels.insert(
            1,
            header(&[
                (0, "No"),
                (1, "型番"),
                (2, "数量"),
                (6, "EC単価"),
                (4, "小計"),
                (5, "注文番号"),
            ]),
        );
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("app-owned"))
        );
    }

    #[test]
    fn renamed_user_header_confirms() {
        let mut o = base_obs();
        // 数量 -> 数 in place (still column index 2).
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
        o.labels.insert(1, header(&[(0, "メモ")])); // something else on row 1
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
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Confirm(m) if m.contains("header row moved"))
        );
    }

    // ---- broken ----

    #[test]
    fn deleted_required_column_is_broken() {
        let mut o = base_obs();
        // 型番 column removed entirely (no replacement label at its position).
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
            },
        )];
        assert!(
            matches!(verify_structure(&C, &sheets), Verdict::Broken(m) if m.contains("deleted"))
        );
    }

    #[test]
    fn renamed_app_column_is_broken() {
        let mut o = base_obs();
        // EC単価 -> 単価 in place.
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
        o.formulas.push((2, 3)); // a formula in EC単価's data area
        assert!(
            matches!(verify_structure(&C, &book(o)), Verdict::Broken(m) if m.contains("formula"))
        );
    }

    #[test]
    fn user_formula_columns_stay_safe() {
        // 小計 (user) is all formulas in base_obs — must NOT trip the app-column guard.
        assert!(matches!(
            verify_structure(&C, &book(base_obs())),
            Verdict::Safe { .. }
        ));
    }

    #[test]
    fn ambiguous_header_rows_broken() {
        let mut o = base_obs();
        // The full constellation appears on rows 3 as well — but row 1 still matches, so this
        // is fine; ambiguity only matters when the contract row does NOT match. Build that case:
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
}
