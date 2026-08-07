// Low-level .xlsx (zip + XML substring) helpers for Excel link mode, ported from
// tools/excel-link-poc (stale_scan.rs walkers + main.rs sheet_parts/spill_ranges/
// cell_has_formula/patch_calc_pr — step 1-3 verified). No XML parser on purpose:
// the write path must preserve every untouched byte (plan.md §3.9), so the read
// helpers stay on the same substring-scanning primitives that were validated there.
//
// Ownership of concerns (differs from the PoC): header LABELS are read via calamine
// in reader.rs (which natively handles inlineStr, XML entities and ruby runs — the
// PoC walker's known gaps). These walkers provide what calamine cannot: formula
// POSITIONS including shared-formula followers (`<f t="shared" si="n"/>`), occupied
// cells with style-only cells excluded, spill ranges, and the calcPr patch target.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// First substring between `start` and `end` markers.
pub fn slice_between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let s = hay.find(start)? + start.len();
    let e = hay[s..].find(end)? + s;
    Some(&hay[s..e])
}

/// Minimal XML entity unescape for attribute values we compare against user-visible
/// names (sheet names). Only the five predefined entities — numeric references are
/// not produced by Excel for these attributes.
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Open the workbook zip once.
pub fn open_zip(path: &Path) -> Result<zip::ZipArchive<File>, String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    zip::ZipArchive::new(f).map_err(|e| e.to_string())
}

pub fn read_part(zip: &mut zip::ZipArchive<File>, name: &str) -> Result<String, String> {
    let mut xml = String::new();
    zip.by_name(name)
        .map_err(|e| format!("{name}: {e}"))?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    Ok(xml)
}

/// Every (sheet name, worksheet part path), resolved the OOXML-correct way:
/// workbook.xml <sheet r:id="rIdN"> → xl/_rels/workbook.xml.rels → Target. Sheet
/// ORDER in workbook.xml does NOT determine sheetN.xml numbering (PoC review
/// finding: after a sheet insert/reorder the two diverge, and an order-based lookup
/// reads the wrong XML).
pub fn sheet_parts(zip: &mut zip::ZipArchive<File>) -> Result<Vec<(String, String)>, String> {
    let wb = read_part(zip, "xl/workbook.xml")?;
    let rels = read_part(zip, "xl/_rels/workbook.xml.rels")?;

    // rId -> Target map from the rels part.
    let mut targets: BTreeMap<String, String> = BTreeMap::new();
    for (i, _) in rels.match_indices("<Relationship ") {
        let end = rels[i..].find("/>").map(|j| i + j).unwrap_or(rels.len());
        let tag = &rels[i..end];
        if let (Some(id), Some(target)) = (
            slice_between(tag, "Id=\"", "\""),
            slice_between(tag, "Target=\"", "\""),
        ) {
            targets.insert(id.to_string(), target.trim_start_matches('/').to_string());
        }
    }

    let mut out = Vec::new();
    for (i, _) in wb.match_indices("<sheet ") {
        let end = wb[i..].find("/>").map(|j| i + j).unwrap_or(wb.len());
        let tag = &wb[i..end];
        let name = slice_between(tag, "name=\"", "\"");
        // The attribute is `r:id="rIdN"` — match on `r:id="` to not confuse it with sheetId=.
        let rid = slice_between(tag, "r:id=\"", "\"");
        if let (Some(name), Some(rid)) = (name, rid) {
            let target = targets
                .get(rid)
                .ok_or_else(|| format!("relationship {rid} for sheet '{name}' not found"))?;
            let part = if target.starts_with("xl/") {
                target.clone()
            } else {
                format!("xl/{target}")
            };
            out.push((unescape(name), part));
        }
    }
    Ok(out)
}

/// Worksheet part for one sheet name (relationship-resolved).
pub fn sheet_part_for(zip: &mut zip::ZipArchive<File>, sheet_name: &str) -> Result<String, String> {
    sheet_parts(zip)?
        .into_iter()
        .find(|(n, _)| n == sheet_name)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("sheet {sheet_name} not in workbook.xml"))
}

/// One `<c>` observation from the walker.
#[derive(Debug, Clone, PartialEq)]
pub struct CellScan {
    pub cell_ref: String,
    pub has_formula: bool,
    /// A cached value is present (`<v>` or `<is>`). A formula cell WITHOUT a value
    /// is the §4.4 "missing" case (value not readable).
    pub has_value: bool,
}

impl CellScan {
    /// Real content (value or formula) — style-only cells (e.g. the blank remainder
    /// of a merge) do NOT count, so "occupied column" detection is not fooled.
    pub fn has_content(&self) -> bool {
        self.has_formula || self.has_value
    }
}

/// Every `<c r="REF" ...>...</c>` in a sheet part. `has_formula` catches
/// shared-formula FOLLOWERS too (`<f t="shared" si="n"/>` with no text), which
/// calamine's worksheet_formula may not surface — that is why formula positions
/// come from here, not calamine. (The PoC variant also extracted shared-string
/// label indices; labels are read via calamine in reader.rs, so that role is
/// dropped here.)
pub fn cells(sheet_xml: &str) -> Vec<CellScan> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(i) = rest.find("<c ") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        let open = &rest[..gt];
        let self_closing = open.ends_with('/');
        let r = slice_between(open, "r=\"", "\"").unwrap_or("").to_string();
        let body_end = if self_closing {
            gt + 1
        } else {
            match rest[gt..].find("</c>") {
                Some(j) => gt + j + 4,
                None => break,
            }
        };
        let body = &rest[gt + 1..body_end.saturating_sub(4).max(gt + 1)];
        let has_formula = !self_closing && body.contains("<f");
        let has_value = !self_closing && (body.contains("<v>") || body.contains("<is>"));
        if !r.is_empty() {
            out.push(CellScan {
                cell_ref: r,
                has_formula,
                has_value,
            });
        }
        rest = &rest[body_end..];
    }
    out
}

/// Collect array/spill ranges from a sheet: the `ref` of every `<f t="array" ref="...">`
/// (and dataTable formulas). `unresolved` becomes true if a formula declares a
/// range-implying type but we cannot read a `ref` — so the caller fails closed
/// (§4.4.2). Consumed by the PR-4 write guard together with state::write_blocked.
pub fn spill_ranges(sheet_xml: &str) -> (Vec<String>, bool) {
    let mut ranges = Vec::new();
    let mut unresolved = false;
    let mut rest = sheet_xml;
    while let Some(i) = rest.find("<f") {
        let open_start = i;
        let Some(gt) = rest[open_start..].find('>') else {
            break;
        };
        let open = &rest[open_start..open_start + gt];
        let is_array = open.contains("t=\"array\"") || open.contains("t=\"dataTable\"");
        if is_array {
            match slice_between(open, "ref=\"", "\"") {
                Some(r) => ranges.push(r.to_string()),
                None => unresolved = true, // array formula with no readable ref → cannot prove safe
            }
        }
        rest = &rest[open_start + gt + 1..];
    }
    (ranges, unresolved)
}

/// Does the target cell itself carry a formula — normal, shared (anchor or `<f/>`
/// follower) or array alike? plan §4.4: the app must NEVER write into a formula
/// cell; patch_cell would replace the `<f>` with a plain value and silently destroy
/// the user's formula. The scan is ATTRIBUTE-ORDER INSENSITIVE (`<c s="1" r="D2">`
/// is equivalent OOXML — a positional `<c r=` match would miss it and let the PR-4
/// write guard overwrite a formula cell). Unparseable open tags / bodies fail
/// closed (treated as formula-bearing).
pub fn cell_has_formula(sheet_xml: &str, cell_ref: &str) -> bool {
    let mut rest = sheet_xml;
    while let Some(i) = rest.find("<c ") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else {
            // A cell open tag that never closes: if it could be our target we must
            // not assume it is safe to overwrite.
            return rest.contains(cell_ref);
        };
        let open = &rest[..gt];
        if slice_between(open, "r=\"", "\"") != Some(cell_ref) {
            rest = &rest[gt + 1..];
            continue;
        }
        if open.ends_with('/') {
            return false; // self-closing <c/>: empty cell, no formula
        }
        let Some(close) = rest.find("</c>") else {
            return true; // malformed body → fail closed
        };
        return rest[gt..close].contains("<f");
    }
    false // cell absent: nothing to destroy (insertion is out of scope anyway)
}

/// Force Excel to recalculate on open (plan.md §4.4.1). Existing calcPr attributes
/// (calcId, calcMode, ...) are preserved; only fullCalcOnLoad is (re)set.
/// Consumed by the PR-4 write path.
pub fn patch_calc_pr(xml: &str) -> String {
    match slice_between(xml, "<calcPr", ">") {
        Some(attrs) => {
            let cleaned = attrs.trim_end_matches('/').trim();
            let mut kept: Vec<&str> = cleaned
                .split_whitespace()
                .filter(|a| !a.starts_with("fullCalcOnLoad="))
                .collect();
            kept.push("fullCalcOnLoad=\"1\"");
            let old = format!("<calcPr{attrs}>");
            xml.replace(&old, &format!("<calcPr {}/>", kept.join(" ")))
        }
        // No <calcPr> at all: add one just before </workbook>.
        None => xml.replace(
            "</workbook>",
            "<calcPr calcId=\"191029\" fullCalcOnLoad=\"1\"/></workbook>",
        ),
    }
}

/// The workbook's calcMode attribute if present ("manual" / "auto" / "autoNoTable").
/// plan.md §4.4.2: calcMode="manual" means cached values may already be stale at
/// link time — detectable, so the link flow warns (heuristic, not proof).
pub fn calc_mode(workbook_xml: &str) -> Option<String> {
    slice_between(workbook_xml, "calcMode=\"", "\"").map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = r#"<worksheet><sheetData>
      <row r="1">
        <c r="A1" t="s"><v>0</v></c>
        <c r="B1" s="1"/>
        <c r="C1"><f>A1*2</f><v>8</v></c>
      </row>
      <row r="2">
        <c r="A2"><v>4</v></c>
        <c r="B2" t="inlineStr"><is><t>inline</t></is></c>
        <c r="C2"><f t="shared" si="0"/><v>16</v></c>
        <c r="D2"><f t="array" ref="D2:D4">UNIQUE(A:A)</f><v>1</v></c>
      </row>
    </sheetData></worksheet>"#;

    #[test]
    fn cells_walker_reports_formula_and_content() {
        let got = cells(SHEET);
        let find = |r: &str| got.iter().find(|c| c.cell_ref == r).unwrap().clone();
        assert!(!find("A1").has_formula && find("A1").has_value); // shared-string value
        assert!(!find("B1").has_content()); // style-only: not content
        assert!(find("C1").has_formula && find("C1").has_value); // normal formula
        assert!(!find("B2").has_formula && find("B2").has_value); // inlineStr counts
        assert!(find("C2").has_formula); // shared-formula FOLLOWER
        assert!(find("D2").has_formula); // array anchor
                                         // Formula cell WITHOUT a cached value = §4.4 missing case.
        let scan = cells(r#"<c r="E1"><f>A1</f></c>"#);
        assert!(scan[0].has_formula && !scan[0].has_value && scan[0].has_content());
    }

    #[test]
    fn spill_ranges_and_fail_closed() {
        let (ranges, unresolved) = spill_ranges(SHEET);
        assert_eq!(ranges, vec!["D2:D4"]);
        assert!(!unresolved);
        // Array formula without a readable ref → unresolved (caller fails closed).
        let (r, u) = spill_ranges(r#"<c r="A1"><f t="array">X</f></c>"#);
        assert!(r.is_empty());
        assert!(u);
    }

    #[test]
    fn cell_formula_detection_fails_closed() {
        assert!(cell_has_formula(SHEET, "C1"));
        assert!(cell_has_formula(SHEET, "C2")); // shared follower
        assert!(!cell_has_formula(SHEET, "A2")); // plain value
        assert!(!cell_has_formula(SHEET, "B1")); // self-closing
        assert!(!cell_has_formula(SHEET, "Z9")); // absent
                                                 // Malformed body → fail closed.
        assert!(cell_has_formula(r#"<c r="A1"><v>1"#, "A1"));
    }

    #[test]
    fn cell_formula_detection_is_attribute_order_insensitive() {
        // Equivalent OOXML with the style attribute FIRST — a positional
        // `<c r="D2"` search would miss the formula and let a write through.
        let xml = r#"<c s="1" r="D2"><f>A1*2</f><v>2</v></c>"#;
        assert!(cell_has_formula(xml, "D2"));
        // Same shape, plain value cell → no formula.
        assert!(!cell_has_formula(r#"<c s="1" r="D2"><v>2</v></c>"#, "D2"));
        // Self-closing with leading attributes → empty cell.
        assert!(!cell_has_formula(r#"<c s="1" r="D2"/>"#, "D2"));
        // D2 must not match D20 (exact attribute value comparison).
        assert!(!cell_has_formula(r#"<c s="1" r="D20"><f>X</f></c>"#, "D2"));
        // Broken open tag that may be the target → fail closed.
        assert!(cell_has_formula(r#"<c s="1" r="D2""#, "D2"));
    }

    #[test]
    fn calc_pr_patch_preserves_other_attributes() {
        let wb = r#"<workbook><calcPr calcId="1" calcMode="manual"/></workbook>"#;
        let out = patch_calc_pr(wb);
        assert!(out.contains("calcId=\"1\""));
        assert!(out.contains("calcMode=\"manual\""));
        assert!(out.contains("fullCalcOnLoad=\"1\""));
        // No calcPr at all → inserted before </workbook>.
        let out2 = patch_calc_pr("<workbook></workbook>");
        assert!(out2.contains("fullCalcOnLoad=\"1\""));
        assert_eq!(calc_mode(wb), Some("manual".into()));
        assert_eq!(calc_mode("<workbook/>"), None);
    }

    #[test]
    fn sheet_name_entities_are_unescaped() {
        assert_eq!(unescape("A &amp; B &lt;2&gt;"), "A & B <2>");
    }
}
