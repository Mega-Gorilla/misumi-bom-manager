//! Measure how heavy the "all formulas → stale" policy is on a given workbook (plan.md §5, step 2).
//!
//! When the app writes an EC column, plan §4.4.1 marks EVERY formula cell stale (no dependency
//! graph → fail safe). The open question for the MVP boundary: how much does that actually cost?
//! It costs a lot only if BUSINESS columns (型番/数量/小計/合計/MOQ/注文番号) are themselves
//! formulas, because then an EC fetch makes those unusable until Excel recalculates.
//!
//! This scans a real (or synthetic) BOM sheet and counts formulas by whether they sit in a
//! business column, so the boundary decision rests on numbers, not guesses. Parsing is done off
//! the raw sheet XML (dependency-light, same spirit as the rest of the harness).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

type R<T> = Result<T, String>;

/// Header substrings that mark a column whose VALUE the business logic consumes. If a formula
/// lands in one of these, the all-stale policy stops that column after every EC fetch.
const BUSINESS_HEADERS: &[(&str, &str)] = &[
    ("型番", "partNo"),
    ("品番", "partNo"),
    ("数量", "qty"),
    ("小計", "subtotal"),
    ("合計", "total"),
    ("moq", "moq"),
    ("最小", "moq"),
    ("注文番号", "orderNo"),
    ("発注", "orderNo"),
];

pub struct ScanResult {
    pub sheet: String,
    pub header_row: u32,
    pub total_cells: usize,
    pub formula_cells: usize,
    /// (column label, business role, count) for formulas that sit in a business column.
    pub business_formulas: Vec<(String, String, usize)>,
    pub calc_mode: String,
}

/// Read the first worksheet's XML and the workbook.xml (for calcMode). Returns (sheet_xml, wb_xml).
fn read_parts(path: &Path) -> R<(String, String)> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip =
        zip::ZipArchive::new(f).map_err(|e| format!("{}: not a zip: {e}", path.display()))?;
    let mut sheet = String::new();
    let mut wb = String::new();
    // sheet1.xml is the first sheet for files Excel writes; the harness fixtures put BOM there.
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = e.name().to_string();
        if name == "xl/worksheets/sheet1.xml" {
            e.read_to_string(&mut sheet).map_err(|e| e.to_string())?;
        } else if name == "xl/workbook.xml" {
            e.read_to_string(&mut wb).map_err(|e| e.to_string())?;
        }
    }
    if sheet.is_empty() {
        return Err("xl/worksheets/sheet1.xml not found".into());
    }
    Ok((sheet, wb))
}

/// Column letters from an A1 ref ("D2" → "D", "AA10" → "AA").
fn col_letters(cell_ref: &str) -> String {
    cell_ref
        .chars()
        .take_while(|c| c.is_ascii_uppercase())
        .collect()
}

/// Row number from an A1 ref ("D2" → 2).
fn row_num(cell_ref: &str) -> Option<u32> {
    cell_ref
        .chars()
        .skip_while(|c| c.is_ascii_uppercase())
        .collect::<String>()
        .parse()
        .ok()
}

/// Every `<c r="REF" ...>...</c>` as (ref, has_formula, shared_string_index?). Inline enough for
/// the PoC; the shared-string index lets us resolve header text.
fn cells(sheet_xml: &str) -> Vec<(String, bool, Option<usize>)> {
    let mut out = Vec::new();
    let mut rest = sheet_xml;
    while let Some(i) = rest.find("<c ") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        let open = &rest[..gt];
        let self_closing = open.ends_with('/');
        let r = super::slice_between_pub(open, "r=\"", "\"")
            .unwrap_or("")
            .to_string();
        let is_str = open.contains("t=\"s\"");
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
        let ss_idx = if is_str {
            super::slice_between_pub(body, "<v>", "</v>").and_then(|v| v.parse().ok())
        } else {
            None
        };
        if !r.is_empty() {
            out.push((r, has_formula, ss_idx));
        }
        rest = &rest[body_end..];
    }
    out
}

/// Resolve shared strings (index → text) from xl/sharedStrings.xml.
fn shared_strings(path: &Path) -> Vec<String> {
    let Ok(f) = File::open(path) else {
        return Vec::new();
    };
    let Ok(mut zip) = zip::ZipArchive::new(f) else {
        return Vec::new();
    };
    let mut xml = String::new();
    if let Ok(mut e) = zip.by_name("xl/sharedStrings.xml") {
        let _ = e.read_to_string(&mut xml);
    }
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(i) = rest.find("<si>") {
        rest = &rest[i + 4..];
        let Some(end) = rest.find("</si>") else { break };
        let si = &rest[..end];
        // Concatenate all <t>..</t> runs inside the <si>.
        let mut text = String::new();
        let mut t = si;
        while let Some(ti) = t.find("<t") {
            t = &t[ti..];
            let Some(tgt) = t.find('>') else { break };
            let after = &t[tgt + 1..];
            let Some(tend) = after.find("</t>") else {
                break;
            };
            text.push_str(&after[..tend]);
            t = &after[tend + 4..];
        }
        out.push(text);
        rest = &rest[end + 5..];
    }
    out
}

/// Pure classification: header detection, business-column mapping, formula counting. Split from
/// I/O so it can be unit-tested on hand-built XML (PR #21 review finding 4).
fn classify(sheet_xml: &str, ss: &[String]) -> (u32, usize, usize, Vec<(String, String, usize)>) {
    let cells = cells(sheet_xml);

    // Header row = the lowest row number present. BOM fixtures use row 1.
    let header_row = cells
        .iter()
        .filter_map(|(r, _, _)| row_num(r))
        .min()
        .unwrap_or(1);

    // Map column letters → business role, by matching the header cell text.
    let mut col_role: BTreeMap<String, (String, String)> = BTreeMap::new(); // col -> (label, role)
    for (r, _, ss_idx) in &cells {
        if row_num(r) != Some(header_row) {
            continue;
        }
        let label = ss_idx.and_then(|i| ss.get(i)).cloned().unwrap_or_default();
        let low = label.to_lowercase();
        if let Some((_, role)) = BUSINESS_HEADERS
            .iter()
            .find(|(needle, _)| low.contains(&needle.to_lowercase()))
        {
            col_role.insert(col_letters(r), (label, role.to_string()));
        }
    }

    // Count formulas. Under the all-stale policy EVERY formula goes stale, header row included,
    // so formula_cells counts them all (finding 4: a =TODAY() in the header row was missed).
    // Only the BUSINESS classification skips the header row, because header cells are labels,
    // not data the business logic consumes.
    let mut formula_cells = 0usize;
    let mut biz: BTreeMap<String, (String, String, usize)> = BTreeMap::new(); // col -> (label,role,count)
    for (r, has_formula, _) in &cells {
        if !has_formula {
            continue;
        }
        formula_cells += 1;
        if row_num(r) == Some(header_row) {
            continue; // header formulas count as stale, but are not business data
        }
        let col = col_letters(r);
        if let Some((label, role)) = col_role.get(&col) {
            let entry = biz.entry(col).or_insert((label.clone(), role.clone(), 0));
            entry.2 += 1;
        }
    }

    (
        header_row,
        cells.len(),
        formula_cells,
        biz.into_values().collect(),
    )
}

pub fn scan(path: &Path) -> R<ScanResult> {
    let (sheet_xml, wb_xml) = read_parts(path)?;
    let ss = shared_strings(path);
    let (header_row, total_cells, formula_cells, business_formulas) = classify(&sheet_xml, &ss);

    let calc_mode = super::slice_between_pub(&wb_xml, "calcMode=\"", "\"")
        .unwrap_or("auto (default)")
        .to_string();

    Ok(ScanResult {
        sheet: "sheet1 (BOM)".into(),
        header_row,
        total_cells,
        formula_cells,
        business_formulas,
        calc_mode,
    })
}

pub fn report(path: &Path) -> R<()> {
    let r = scan(path)?;
    println!("== stale-scan: {} ==", path.display());
    println!("   sheet       : {}", r.sheet);
    println!("   header row  : {}", r.header_row);
    println!("   cells       : {}", r.total_cells);
    println!("   formulas    : {}", r.formula_cells);
    println!("   calcMode    : {}", r.calc_mode);
    if r.calc_mode.starts_with("manual") {
        println!("   [WARN] calcMode=manual — cached values may already be stale on link (§4.4.2)");
    }
    println!("\n-- business-column formulas (unusable after every EC fetch, all-stale policy) --");
    if r.business_formulas.is_empty() {
        println!(
            "   [ ok ] none — business columns are hand-entered; all-stale costs nothing here"
        );
    } else {
        let total: usize = r.business_formulas.iter().map(|(_, _, n)| n).sum();
        for (label, role, n) in &r.business_formulas {
            println!("   [HEAVY] {label} ({role}) x{n}");
        }
        println!(
            "   → {total} business formula cell(s) go unusable on each EC fetch until Excel recalcs"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row 1 headers (shared strings 0..=2), row 2 data. C1 header carries a =TODAY() formula.
    /// B2 (型番 column) is a formula; D2 (non-business) is a formula; C2 is a plain value.
    const SHEET: &str = r#"<worksheet><sheetData>
        <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><f>TODAY()</f><v>2</v></c></row>
        <row r="2"><c r="A2"><v>1</v></c><c r="B2"><f>"CBT3-"&amp;A2</f><v>CBT3-1</v></c><c r="C2"><v>5</v></c><c r="D2"><f>A2*2</f><v>2</v></c></row>
    </sheetData></worksheet>"#;

    fn ss() -> Vec<String> {
        vec!["No".into(), "型番".into(), "数量".into()]
    }

    #[test]
    fn header_formula_counts_as_stale_but_not_business() {
        // PR #21 finding 4: a header-row formula (=TODAY() in C1) must be counted in
        // formula_cells (it DOES go stale under the all-stale policy) …
        let (header_row, total, formulas, biz) = classify(SHEET, &ss());
        assert_eq!(header_row, 1);
        assert_eq!(total, 7);
        assert_eq!(formulas, 3, "C1 + B2 + D2 — header formula must be counted");
        // … but must NOT appear in the business classification (labels are not data), while the
        // 型番 formula in B2 must.
        assert_eq!(biz.len(), 1);
        assert_eq!(biz[0].0, "型番");
        assert_eq!(biz[0].1, "partNo");
        assert_eq!(biz[0].2, 1);
    }

    #[test]
    fn no_formulas_no_business() {
        let plain = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c></row>
            <row r="2"><c r="A2"><v>1</v></c></row>
        </sheetData></worksheet>"#;
        let (_, total, formulas, biz) = classify(plain, &ss());
        assert_eq!(total, 2);
        assert_eq!(formulas, 0);
        assert!(biz.is_empty());
    }
}
