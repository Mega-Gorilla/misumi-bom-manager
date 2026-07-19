// Go/no-go probe for read-modify-write of an EXISTING .xlsx.
// See docs/plans/0018-excel-link-mode/plan.md §6.1 (top risk) and §7 step 1.
//
// The question: if a Rust crate opens a workbook Excel wrote, changes ONE cell and saves,
// does everything else survive? §3.1 shows the current writer cannot do this at all
// (it builds a new workbook), so link mode depends on finding something that can.
//
// `inspect` and `diff` are deliberately crate-agnostic: .xlsx is a zip of XML, so we measure
// what is actually IN the file rather than trusting a library's API surface. That way a second
// candidate backend can be dropped into `rmw` and judged by the same yardstick.
//
// Usage:
//   cargo run -- inspect <xlsx>
//   cargo run -- rmw <xlsx> [umya|zip]   (writes out/<stem>-after-<backend>.xlsx)
//   cargo run -- diff <before> <after>
//
// Two backends, judged by the same yardstick:
//   umya - umya-spreadsheet: deserialises every part and re-serialises. Anything it does not
//          model is dropped on save.
//   zip  - surgical: copy every zip entry byte-for-byte except the sheet XML we must touch.
//          Nothing else can be lost because nothing else is rewritten.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

mod stale_scan;
mod state;
mod structure;
mod sync;

type R<T> = Result<T, String>;

/// SHA-256 over the whole file (plan.md §4.6.1). The one content fingerprint used everywhere.
fn fingerprint(path: &Path) -> R<state::Fingerprint> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(h.finalize().into())
}

fn parse_u64(s: &str) -> R<u64> {
    s.parse().map_err(|_| format!("not a number: {s}"))
}

fn parse_fp(hex: &str) -> R<state::Fingerprint> {
    let bytes: Result<Vec<u8>, _> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap_or(""), 16))
        .collect();
    let v = bytes.map_err(|_| "bad hex fingerprint".to_string())?;
    v.try_into()
        .map_err(|_| "fingerprint must be 32 bytes".to_string())
}

/// Apply the §4.4.2 restore rule to a live file, driven by the persisted values. This is the same
/// pure logic `state::restore` unit-tests, exposed so lifecycle.ps1 can prove it against real
/// fingerprints (Stale stays sticky when unchanged; Trusted only after a recalc-requested change).
fn cmd_restore(current: &Path, last_write_hex: &str, recalc_requested: bool) -> R<()> {
    let current_fp = fingerprint(current)?;
    let last_app_write = if last_write_hex == "none" {
        None
    } else {
        Some(parse_fp(last_write_hex)?)
    };
    let p = state::Persisted {
        last_app_write,
        recalc_requested,
        value_readable: true,
    };
    let st = state::restore(&current_fp, &p);
    println!("{st:?} usable={}", st.is_usable());
    Ok(())
}

// ---- profile: what we can observe in the raw package -------------------------------------

/// Counts of the XML markers that stand for a user-visible Excel feature. Counting substrings
/// (rather than parsing) keeps this honest and dependency-free: if a marker is gone from the
/// bytes, the feature is gone from the file, whatever the library claims.
#[derive(Default, Clone, PartialEq)]
struct Marks {
    formulas: usize,
    array_formula_t: usize,
    shared_formula_t: usize,
    formula_ref_attr: usize,
    cached_values: usize,
    merge_cells: usize,
    cond_formatting: usize,
    cols: usize,
    page_setup: usize,
    print_options: usize,
    drawing_rel: usize,
    inline_str: usize,
    shared_str_cells: usize,
}

impl Marks {
    fn scan(xml: &str) -> Marks {
        Marks {
            formulas: count(xml, "<f"),
            array_formula_t: count(xml, "t=\"array\""),
            shared_formula_t: count(xml, "t=\"shared\""),
            formula_ref_attr: count(xml, " ref=\""),
            cached_values: count(xml, "<v>"),
            merge_cells: count(xml, "<mergeCell "),
            cond_formatting: count(xml, "<conditionalFormatting"),
            cols: count(xml, "<col "),
            page_setup: count(xml, "<pageSetup"),
            print_options: count(xml, "<printOptions"),
            drawing_rel: count(xml, "<drawing "),
            inline_str: count(xml, "t=\"inlineStr\""),
            shared_str_cells: count(xml, "t=\"s\""),
        }
    }

    fn rows(&self) -> Vec<(&'static str, usize)> {
        vec![
            ("formula <f>", self.formulas),
            ("  array   t=\"array\"", self.array_formula_t),
            ("  shared  t=\"shared\"", self.shared_formula_t),
            ("  ref= attr (array/shared range)", self.formula_ref_attr),
            ("cached value <v>", self.cached_values),
            ("mergeCell", self.merge_cells),
            ("conditionalFormatting", self.cond_formatting),
            ("col (width)", self.cols),
            ("pageSetup", self.page_setup),
            ("printOptions", self.print_options),
            ("drawing ref", self.drawing_rel),
            ("inlineStr cell", self.inline_str),
            ("sharedString cell t=\"s\"", self.shared_str_cells),
        ]
    }
}

struct Profile {
    entries: Vec<String>,
    /// Per worksheet part (xl/worksheets/sheetN.xml).
    sheets: BTreeMap<String, Marks>,
    /// Formula texts per worksheet part, so we can name exactly which formulas vanished.
    formulas: BTreeMap<String, Vec<String>>,
    workbook_xml: String,
}

impl Profile {
    fn read(path: &Path) -> R<Profile> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip =
            zip::ZipArchive::new(f).map_err(|e| format!("{}: not a zip: {e}", path.display()))?;
        let mut entries = Vec::new();
        let mut sheets = BTreeMap::new();
        let mut formulas = BTreeMap::new();
        let mut workbook_xml = String::new();

        for i in 0..zip.len() {
            let mut e = zip.by_index(i).map_err(|e| e.to_string())?;
            let name = e.name().to_string();
            entries.push(name.clone());
            if !name.ends_with(".xml") && !name.ends_with(".rels") {
                continue;
            }
            let mut s = String::new();
            // Binary-ish parts (images) are not utf-8; skip them rather than fail.
            if e.read_to_string(&mut s).is_err() {
                continue;
            }
            if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
                sheets.insert(name.clone(), Marks::scan(&s));
                formulas.insert(name.clone(), extract_formulas(&s));
            } else if name == "xl/workbook.xml" {
                workbook_xml = s;
            }
        }
        entries.sort();
        Ok(Profile {
            entries,
            sheets,
            formulas,
            workbook_xml,
        })
    }

    fn calc_pr(&self) -> String {
        match slice_between(&self.workbook_xml, "<calcPr", ">") {
            Some(s) => format!("<calcPr{s}>"),
            None => "(absent)".into(),
        }
    }

    fn defined_names(&self) -> usize {
        count(&self.workbook_xml, "<definedName ")
    }
}

fn count(hay: &str, needle: &str) -> usize {
    hay.matches(needle).count()
}

/// Read every zip entry's uncompressed bytes, keyed by name. Used by the exhaustive `diff`:
/// counting XML markers can miss a value swap or a number→sharedString change, so the real
/// check is "are the bytes of every non-target part identical".
fn read_all_bytes(path: &Path) -> R<BTreeMap<String, Vec<u8>>> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip =
        zip::ZipArchive::new(f).map_err(|e| format!("{}: not a zip: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for i in 0..zip.len() {
        let mut e = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = e.name().to_string();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        out.insert(name, buf);
    }
    Ok(out)
}

fn slice_between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = hay.find(start)? + start.len();
    let j = hay[i..].find(end)? + i;
    Some(&hay[i..j])
}

/// Same as `slice_between`, exposed for the submodules.
pub(crate) fn slice_between_pub<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    slice_between(hay, start, end)
}

/// Pull the text of every `<f ...>text</f>`. Self-closing `<f/>` (shared-formula followers)
/// carry no text and are skipped — they are counted by Marks::formulas instead.
fn extract_formulas(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<f") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        if rest[..gt].ends_with('/') {
            rest = &rest[gt + 1..];
            continue;
        }
        let after = &rest[gt + 1..];
        let Some(close) = after.find("</f>") else {
            break;
        };
        let text = after[..close].trim();
        if !text.is_empty() {
            out.push(text.to_string());
        }
        rest = &after[close + 4..];
    }
    out.sort();
    out
}

/// Collect array/spill ranges from a sheet: the `ref` of every `<f t="array" ref="...">` (and
/// shared-formula anchors). `unresolved` becomes true if a formula declares a type that implies a
/// range but we cannot read a `ref` — so the caller fails closed (§4.4.2).
fn spill_ranges(sheet_xml: &str) -> (Vec<String>, bool) {
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

/// Does the target cell itself carry a formula — normal, shared (anchor or `<f/>` follower) or
/// array alike? plan §4.4: the app must NEVER write into a formula cell; `patch_cell` would
/// replace the `<f>` with a plain value and silently destroy the user's formula (PR #21 review
/// finding 1: the spill-range check alone let a plain formula cell through). Unparseable cell
/// bodies fail closed (treated as formula-bearing).
fn cell_has_formula(sheet_xml: &str, cell_ref: &str) -> bool {
    let open = format!("<c r=\"{cell_ref}\"");
    let Some(start) = sheet_xml.find(&open) else {
        return false; // cell absent: nothing to destroy (insertion is out of scope anyway)
    };
    let rest = &sheet_xml[start..];
    let Some(gt) = rest.find('>') else {
        return true; // malformed open tag → fail closed
    };
    if rest[..gt].ends_with('/') {
        return false; // self-closing <c/>: empty cell, no formula
    }
    let Some(close) = rest.find("</c>") else {
        return true; // malformed body → fail closed
    };
    rest[gt..close].contains("<f")
}

/// check-write <xlsx> <targetCell> — may the app write that cell? Refused when the cell itself is
/// a formula (any kind), when it falls inside an array/spill range, or when a range cannot be
/// safely identified.
fn cmd_check_write(path: &Path, target: &str) -> R<()> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut sheet = String::new();
    zip.by_name("xl/worksheets/sheet1.xml")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut sheet)
        .map_err(|e| e.to_string())?;
    let has_formula = cell_has_formula(&sheet, target);
    let (ranges, unresolved) = spill_ranges(&sheet);
    let range_refs: Vec<&str> = ranges.iter().map(|s| s.as_str()).collect();
    let in_spill = state::write_blocked(&[target], &range_refs, unresolved);

    println!("== check-write {} @ {target} ==", path.display());
    println!("   cell has formula  : {has_formula}");
    println!("   array/spill ranges: {ranges:?}  unresolved={unresolved}");
    if has_formula {
        println!("   [BLOCK] {target} is itself a formula cell — writing would destroy the user's formula (§4.4)");
    } else if in_spill {
        println!(
            "   [BLOCK] writing {target} is refused (intersects a spill range or range unresolved)"
        );
    } else {
        println!("   [ ok ] {target} is a plain value cell, clear of every array/spill range");
    }
    Ok(())
}

// ---- commands ----------------------------------------------------------------------------

fn cmd_inspect(path: &Path) -> R<()> {
    let p = Profile::read(path)?;
    println!("== {} ==", path.display());
    println!("\n-- package parts ({}) --", p.entries.len());
    for e in &p.entries {
        println!("   {e}");
    }
    println!("\n-- workbook.xml --");
    println!("   calcPr       : {}", p.calc_pr());
    println!("   definedName  : {}", p.defined_names());
    for (name, m) in &p.sheets {
        println!("\n-- {name} --");
        for (label, n) in m.rows() {
            if n > 0 {
                println!("   {label:34} {n}");
            }
        }
        if let Some(fs) = p.formulas.get(name) {
            if !fs.is_empty() {
                println!("   formulas: {fs:?}");
            }
        }
    }
    Ok(())
}

/// The cell the app would write: an app-owned "EC単価" value. Deliberately far away from the
/// array formula in J2, to show whether a write can damage cells it never touched.
const TARGET_SHEET: &str = "BOM";
const TARGET_CELL: &str = "D2";
const TARGET_VALUE: i32 = 1234;

fn out_path(path: &Path, backend: &str) -> R<PathBuf> {
    let out_dir = PathBuf::from("out");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("book");
    Ok(out_dir.join(format!("{stem}-after-{backend}.xlsx")))
}

fn cmd_rmw_umya(path: &Path) -> R<()> {
    let out = out_path(path, "umya")?;

    println!("== read-modify-write via umya-spreadsheet ==");
    println!("   in : {}", path.display());
    println!("   out: {}", out.display());

    let mut book =
        umya_spreadsheet::reader::xlsx::read(path).map_err(|e| format!("read failed: {e:?}"))?;

    // (2) Can we identify formula cells? This underpins the whole calc-state model (§4.4).
    println!("\n-- formula identification (plan §7 step1 item 2) --");
    let mut seen = 0usize;
    let names: Vec<String> = book
        .sheet_collection()
        .iter()
        .map(|s| s.name().to_string())
        .collect();
    for name in names {
        let sheet = book
            .sheet_by_name(&name)
            .map_err(|e| format!("{name}: {e:?}"))?;
        for cell in sheet.cells_sorted() {
            if cell.is_formula() {
                seen += 1;
                let coord = cell.coordinate().to_string();
                let f = cell.formula();
                let (ftype, fref) = match cell.formula_obj() {
                    Some(o) => (format!("{:?}", o.formula_type()), o.reference().to_string()),
                    None => ("(no obj)".into(), String::new()),
                };
                println!("   {name}!{coord:6} f={f:34} type={ftype:8} ref={fref:?}");
            }
        }
    }
    println!("   -> {seen} formula cells identified");

    // (3) Touch exactly ONE cell (an app-owned column value), nothing else.
    println!("\n-- dirty-cell update (plan §7 step1 item 3) --");
    {
        let sheet = book
            .sheet_by_name_mut(TARGET_SHEET)
            .map_err(|e| format!("sheet {TARGET_SHEET}: {e:?}"))?;
        sheet.cell_mut(TARGET_CELL).set_value_number(TARGET_VALUE);
    }
    println!("   set {TARGET_SHEET}!{TARGET_CELL} = {TARGET_VALUE} (number)");

    // (4) fullCalcOnLoad — research says umya has no API for it; prove it against the artifact.
    println!("\n-- calcPr / fullCalcOnLoad (plan §7 step1 item 4) --");
    println!("   umya exposes no setter; `diff` will show what actually lands in workbook.xml.");

    umya_spreadsheet::writer::xlsx::write(&book, &out)
        .map_err(|e| format!("write failed: {e:?}"))?;
    println!(
        "\n   wrote {} bytes",
        std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

// ---- backend: surgical zip edit ----------------------------------------------------------

/// Rewrite one `<c r="..">` element in place, keeping its style attribute.
/// Returns None when the cell is absent (inserting a new <c>/<row> is out of scope for the probe).
fn patch_cell(xml: &str, cell_ref: &str, value: i32) -> Option<String> {
    let open = format!("<c r=\"{cell_ref}\"");
    let start = xml.find(&open)?;
    let after_open = start + open.len();
    let tag_end = xml[after_open..].find('>')? + after_open;
    let attrs = &xml[after_open..tag_end];
    let self_closing = attrs.ends_with('/');
    let attrs = attrs.trim_end_matches('/');

    // Keep the style index; drop t= because we are writing a number.
    let style = attrs
        .split_whitespace()
        .find(|a| a.starts_with("s=\""))
        .map(|s| format!(" {s}"))
        .unwrap_or_default();

    let end = if self_closing {
        tag_end + 1
    } else {
        xml[tag_end..].find("</c>")? + tag_end + 4
    };
    let replacement = format!("<c r=\"{cell_ref}\"{style}><v>{value}</v></c>");
    let mut out = String::with_capacity(xml.len());
    out.push_str(&xml[..start]);
    out.push_str(&replacement);
    out.push_str(&xml[end..]);
    Some(out)
}

/// Force Excel to recalculate on open. plan.md §4.4.1 case A depends on this being under our
/// control rather than a side effect of some library's hardcoded calcId.
fn patch_calc_pr(xml: &str) -> String {
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

/// Every (sheet name, worksheet part path), resolved the OOXML-correct way:
/// workbook.xml <sheet r:id="rIdN"> → xl/_rels/workbook.xml.rels → Target. Sheet ORDER in
/// workbook.xml does NOT determine sheetN.xml numbering (PR #22 review finding 2) — after a
/// sheet insert/reorder in Excel the two diverge, and an order-based lookup reads the wrong XML.
fn sheet_parts(zip: &mut zip::ZipArchive<File>) -> R<Vec<(String, String)>> {
    let mut wb = String::new();
    zip.by_name("xl/workbook.xml")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut wb)
        .map_err(|e| e.to_string())?;
    let mut rels = String::new();
    zip.by_name("xl/_rels/workbook.xml.rels")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut rels)
        .map_err(|e| e.to_string())?;

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
            out.push((name.to_string(), part));
        }
    }
    Ok(out)
}

/// Worksheet part for one sheet name (relationship-resolved).
fn sheet_part_for(zip: &mut zip::ZipArchive<File>, sheet_name: &str) -> R<String> {
    sheet_parts(zip)?
        .into_iter()
        .find(|(n, _)| n == sheet_name)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("sheet {sheet_name} not in workbook.xml"))
}

// ---- structure verdict (plan §4.9, step 3) -----------------------------------------------

/// The contract for the gen-mutations.ps1 base workbook. PoC-only: the real implementation
/// stores this per linked BOM in the DB (§4.7).
const POC_CONTRACT: structure::Contract = structure::Contract {
    sheet: "BOM",
    header_row: 1,
    columns: &[
        ("No", structure::Ownership::User),
        ("型番", structure::Ownership::User),
        ("数量", structure::Ownership::User),
        ("EC単価", structure::Ownership::App),
        ("小計", structure::Ownership::User),
        ("注文番号", structure::Ownership::User),
    ],
    required: &["型番", "数量"],
};

/// Build the observation for every worksheet: string-cell labels per row plus formula positions.
/// Reuses the stale_scan XML walkers so the structure checker sees exactly what the scanner sees.
fn observe_workbook(path: &Path) -> R<Vec<(String, structure::SheetObs)>> {
    let ss = stale_scan::shared_strings(path);
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let parts = sheet_parts(&mut zip)?; // relationship-resolved (review finding 2)

    let mut out = Vec::new();
    for (name, part) in parts {
        let mut xml = String::new();
        match zip.by_name(&part) {
            Ok(mut e) => e.read_to_string(&mut xml).map_err(|e| e.to_string())?,
            Err(_) => 0, // sheet part absent — leave the observation empty
        };
        let mut obs = structure::SheetObs::default();
        for (cell_ref, has_formula, has_content, ss_idx) in stale_scan::cells(&xml) {
            let Some((row0, col0)) = state::parse_cell(&cell_ref) else {
                continue;
            };
            let row = row0 + 1; // SheetObs rows are 1-based like the contract's header_row
            if has_formula {
                obs.formulas.push((row, col0));
            }
            if has_content {
                obs.occupied_cols.insert(col0);
            }
            if let Some(label) = ss_idx.and_then(|i| ss.get(i)) {
                obs.labels
                    .entry(row)
                    .or_default()
                    .push((col0, label.clone()));
            }
        }
        out.push((name, obs));
    }
    Ok(out)
}

fn verdict_of(path: &Path) -> R<structure::Verdict> {
    let sheets = observe_workbook(path)?;
    Ok(structure::verify_structure(&POC_CONTRACT, &sheets))
}

/// verify-structure <xlsx|dir>. For a directory, every *.xlsx is judged against the expectation
/// encoded in its file name prefix (safe- / warn- / broken-); any mismatch exits non-zero, so
/// the fixture sweep is a machine check, not a wall of text to eyeball.
fn cmd_verify_structure(path: &Path) -> R<()> {
    if path.is_file() {
        let v = verdict_of(path)?;
        println!("== verify-structure {} ==\n   {v:?}", path.display());
        return Ok(());
    }

    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "xlsx"))
        .collect();
    entries.sort();
    if entries.is_empty() {
        return Err(format!("no .xlsx under {}", path.display()));
    }

    println!("== verify-structure sweep: {} ==", path.display());
    let mut failures = 0usize;
    for p in &entries {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let expected = if stem.starts_with("safe-") || stem == "base" {
            "Safe"
        } else if stem.starts_with("warn-") {
            "Confirm"
        } else if stem.starts_with("broken-") {
            "Broken"
        } else {
            println!("   [skip] {stem} (no expectation prefix)");
            continue;
        };
        let v = verdict_of(p)?;
        let actual = match &v {
            structure::Verdict::Safe { .. } => "Safe",
            structure::Verdict::Confirm(_) => "Confirm",
            structure::Verdict::Broken(_) => "Broken",
        };
        let ok = actual == expected;
        if !ok {
            failures += 1;
        }
        println!(
            "   [{}] {stem:28} expected={expected:7} actual={actual:7}  {v:?}",
            if ok { " ok " } else { "FAIL" }
        );
    }
    println!("\n-- verdict --");
    if failures == 0 {
        println!("   [PASS] every mutation judged as its file name expects");
        Ok(())
    } else {
        Err(format!(
            "{failures} mutation(s) judged differently than expected"
        ))
    }
}

/// resolve <path>: canonicalize so junctions/symlinks collapse to the real location. The .lnk
/// hop and the volume's file-system name are handled by check-env.ps1, which feeds both into
/// state::link_allowed for the final decision.
fn cmd_resolve(path: &Path) -> R<()> {
    println!("== resolve {} ==", path.display());
    match std::fs::canonicalize(path) {
        Ok(real) => {
            let changed = real != path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
                || real.to_string_lossy() != path.to_string_lossy();
            println!("   real   : {}", real.display());
            println!("   changed: {changed}");
            Ok(())
        }
        Err(e) => {
            println!("   real   : (unresolvable: {e})");
            Err("path cannot be resolved — link must be refused (fail closed)".into())
        }
    }
}

/// `set_fco=false` is the CONTROL for the lifecycle experiment (PR #21 review finding 2): an
/// identical write minus fullCalcOnLoad, to prove that opening in Excel recalculates because of
/// OUR flag, not as a side effect of merely opening.
fn cmd_rmw_zip(path: &Path, set_fco: bool, cell: &str, out_override: Option<&Path>) -> R<()> {
    let out = match out_override {
        Some(p) => p.to_path_buf(),
        None => out_path(path, if set_fco { "zip" } else { "zip-nofco" })?,
    };
    println!("== read-modify-write via surgical zip edit (fullCalcOnLoad={set_fco}) ==");
    println!("   in : {}", path.display());
    println!("   out: {}", out.display());

    let f = File::open(path).map_err(|e| e.to_string())?;
    let mut zin = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let part = sheet_part_for(&mut zin, TARGET_SHEET)?;
    println!("\n   {TARGET_SHEET} -> {part}");

    let fout = File::create(&out).map_err(|e| e.to_string())?;
    let mut zout = zip::ZipWriter::new(fout);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    let mut copied = 0usize;
    let mut rewritten: Vec<String> = Vec::new();

    // calcChain.xml records the *order* in which formulas are computed (the dependency chain),
    // not their values. Changing a value cell does not change that order, so calcChain stays
    // valid and we keep it. Dropping it would leave dangling references in
    // [Content_Types].xml (Override) and xl/_rels/workbook.xml.rels (Relationship) unless those
    // are also edited — an inconsistent OPC package. Keeping it avoids that entirely, and
    // fullCalcOnLoad="1" (set below) still forces a full recalc on open.
    for i in 0..zin.len() {
        let mut e = zin.by_index(i).map_err(|e| e.to_string())?;
        let name = e.name().to_string();

        let must_patch = name == part || (name == "xl/workbook.xml" && set_fco);
        if must_patch {
            let mut s = String::new();
            e.read_to_string(&mut s).map_err(|e| e.to_string())?;
            let patched = if name == part {
                patch_cell(&s, cell, TARGET_VALUE)
                    .ok_or_else(|| format!("cell {cell} not found in {part}"))?
            } else {
                patch_calc_pr(&s)
            };
            zout.start_file(&name, opts).map_err(|e| e.to_string())?;
            std::io::Write::write_all(&mut zout, patched.as_bytes()).map_err(|e| e.to_string())?;
            rewritten.push(name);
        } else {
            // Byte-for-byte: never re-encode a part we do not need to touch.
            zout.raw_copy_file(e).map_err(|e| e.to_string())?;
            copied += 1;
        }
    }
    zout.finish().map_err(|e| e.to_string())?;

    println!("   rewritten: {rewritten:?}");
    println!("   copied verbatim: {copied} parts (incl. calcChain.xml — kept for OPC consistency)");
    println!("\n   set {TARGET_SHEET}!{cell} = {TARGET_VALUE}");
    if set_fco {
        println!("   set calcPr fullCalcOnLoad=\"1\"");
    } else {
        println!("   calcPr left untouched (CONTROL: no recalc request)");
    }
    println!(
        "\n   wrote {} bytes",
        std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

/// Exhaustive equality check. Counting XML markers (the old approach) has false negatives:
/// a value swap in another cell, or a number→sharedString change, keeps the counts the same
/// (reviewer, PR #20). So the real yardstick is: the ONLY changes allowed are the two we
/// intended. Everything else must be byte-identical to `before`.
///
/// Allowed changes are exactly two: the target worksheet part must equal before-with-D2-patched,
/// and xl/workbook.xml must equal before-with-fullCalcOnLoad-patched. Every other part must match
/// `before` byte-for-byte, and the entry set must be identical. This proves "only the intended 2
/// edits happened" rather than merely "nothing shrank".
/// Pure verdict: given the two packages' parts (name -> bytes) and the target worksheet part,
/// list every disallowed change. Empty list == PASS. Factored out so it can be unit-tested with
/// hand-built part maps, without needing Excel to build a fixture (reviewer, PR #20).
fn evaluate(
    a: &BTreeMap<String, Vec<u8>>,
    b: &BTreeMap<String, Vec<u8>>,
    target_part: &str,
    cell: &str,
    log: &mut Vec<String>,
) -> Vec<String> {
    let mut violations: Vec<String> = Vec::new();

    // 1. entry set identical
    let a_names: std::collections::BTreeSet<&String> = a.keys().collect();
    let b_names: std::collections::BTreeSet<&String> = b.keys().collect();
    log.push(format!("-- package parts: {} -> {} --", a.len(), b.len()));
    for e in a_names.difference(&b_names) {
        log.push(format!("   [LOST] {e}"));
        violations.push(format!("part lost: {e}"));
    }
    for e in b_names.difference(&a_names) {
        log.push(format!("   [ +  ] {e}"));
        violations.push(format!("part added: {e}"));
    }
    if a_names == b_names {
        log.push("   [ ok ] entry set identical".into());
    }

    // 2. every part byte-identical, except the two we deliberately patch — and those two must
    //    equal exactly before-plus-the-single-intended-edit.
    log.push("-- per-part bytes --".into());
    for (name, before_bytes) in a {
        let Some(after_bytes) = b.get(name) else {
            continue; // reported above
        };
        let allowed_target = name == target_part;
        let allowed_workbook = name == "xl/workbook.xml";

        if !allowed_target && !allowed_workbook {
            if before_bytes != after_bytes {
                log.push(format!(
                    "   [DIFF] {name}  (must be byte-identical, but changed)"
                ));
                violations.push(format!("unexpected change in {name}"));
            }
            continue;
        }

        let before_xml = String::from_utf8_lossy(before_bytes).into_owned();
        let after_xml = String::from_utf8_lossy(after_bytes);
        let expected = if allowed_target {
            match patch_cell(&before_xml, cell, TARGET_VALUE) {
                Some(x) => x,
                None => {
                    violations.push(format!("{cell} not found in {name}"));
                    continue;
                }
            }
        } else {
            patch_calc_pr(&before_xml)
        };
        if after_xml == expected {
            let what = if allowed_target {
                format!("{cell}={TARGET_VALUE}")
            } else {
                "fullCalcOnLoad=\"1\"".into()
            };
            log.push(format!("   [ ok ] {name}  == before + ({what})"));
        } else {
            log.push(format!(
                "   [DIFF] {name}  differs beyond the single intended edit"
            ));
            violations.push(format!("{name} changed beyond the intended edit"));
        }
    }

    // 3. belt-and-braces: the value must be exactly fullCalcOnLoad="1", not merely present.
    let wb_after = String::from_utf8_lossy(b.get("xl/workbook.xml").map(|v| &v[..]).unwrap_or(&[]));
    if !wb_after.contains("fullCalcOnLoad=\"1\"") {
        log.push("   [FAIL] fullCalcOnLoad=\"1\" not found in workbook.xml".into());
        violations.push("fullCalcOnLoad=\"1\" not set".into());
    }

    violations
}

fn cmd_diff(before: &Path, after: &Path, cell: &str) -> R<()> {
    let a = read_all_bytes(before)?;
    let b = read_all_bytes(after)?;
    let target_part = {
        let f = File::open(before).map_err(|e| e.to_string())?;
        let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
        sheet_part_for(&mut z, TARGET_SHEET)?
    };

    println!("== diff (exhaustive) ==");
    println!("   before: {}", before.display());
    println!("   after : {}", after.display());
    println!("   target: {target_part}  (only this + xl/workbook.xml may differ)");

    let mut log = Vec::new();
    let violations = evaluate(&a, &b, &target_part, cell, &mut log);
    for line in &log {
        println!("{line}");
    }

    println!("\n-- verdict --");
    if violations.is_empty() {
        println!(
            "   [PASS] only the 2 intended edits occurred ({cell}={TARGET_VALUE} + fullCalcOnLoad); \
             all other {} parts byte-identical",
            a.len() - 2
        );
        Ok(())
    } else {
        println!("   [FAIL] {} violation(s):", violations.len());
        for v in &violations {
            println!("      - {v}");
        }
        Err(format!("{} violation(s) — see above", violations.len()))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.as_slice() {
        [c, p] if c == "inspect" => cmd_inspect(Path::new(p)),
        [c, p] if c == "rmw" => cmd_rmw_umya(Path::new(p)),
        [c, p, b] if c == "rmw" && b == "umya" => cmd_rmw_umya(Path::new(p)),
        [c, p, b] if c == "rmw" && b == "zip" => cmd_rmw_zip(Path::new(p), true, TARGET_CELL, None),
        [c, p, b, cell] if c == "rmw" && b == "zip" => cmd_rmw_zip(Path::new(p), true, cell, None),
        // rmw <xlsx> zip <cell> <outPath>  — step 3c: place the temp next to the case target
        [c, p, b, cell, o] if c == "rmw" && b == "zip" => {
            cmd_rmw_zip(Path::new(p), true, cell, Some(Path::new(o)))
        }
        [c, p, b] if c == "rmw" && b == "zip-nofco" => {
            cmd_rmw_zip(Path::new(p), false, TARGET_CELL, None)
        }
        [c, p, b, cell] if c == "rmw" && b == "zip-nofco" => {
            cmd_rmw_zip(Path::new(p), false, cell, None)
        }
        [c, a, b] if c == "diff" => cmd_diff(Path::new(a), Path::new(b), TARGET_CELL),
        [c, a, b, cell] if c == "diff" => cmd_diff(Path::new(a), Path::new(b), cell),
        [c, p] if c == "verify-structure" => cmd_verify_structure(Path::new(p)),
        [c, p] if c == "resolve" => cmd_resolve(Path::new(p)),
        // decide <fs_name> <resolved:true|false>  → §4.10 link decision (authority lives here,
        // not in the PowerShell that gathered the facts)
        [c, fs, resolved] if c == "decide" => {
            let d = state::link_allowed(fs, resolved == "true");
            println!("{d:?}");
            match d {
                state::LinkDecision::Allow => Ok(()),
                state::LinkDecision::WarnNoWriteBack => {
                    Err("link refused or write-back disabled (fail closed)".into())
                }
            }
        }
        [c, p] if c == "stale-scan" => stale_scan::report(Path::new(p)),
        // Print the SHA-256 fingerprint (§4.6.1). Used by lifecycle.ps1 to prove the restore rule.
        [c, p] if c == "fingerprint" => fingerprint(Path::new(p)).map(|fp| {
            println!(
                "{}",
                fp.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
        }),
        // restore <file> <last-app-write-hex|none> <true|false>  → prints CalcState + usable
        [c, p, last, recalc] if c == "restore" => cmd_restore(Path::new(p), last, recalc == "true"),
        // check-write <xlsx> <cell>  → is writing that cell blocked by an array/spill range?
        [c, p, cell] if c == "check-write" => cmd_check_write(Path::new(p), cell),
        // ---- step 3c: real-sync PoC (Issue #23) ----
        [c, p] if c == "marker" => sync::cmd_marker(Path::new(p), "D2", "G2"),
        [c, p, ec, user] if c == "marker" => sync::cmd_marker(Path::new(p), ec, user),
        [c, p] if c == "watch-stable" => sync::cmd_watch_stable(Path::new(p), 300, 10, None),
        [c, p, t, s] if c == "watch-stable" => {
            (|| sync::cmd_watch_stable(Path::new(p), parse_u64(t)?, parse_u64(s)?, None))()
        }
        [c, p, t, s, base] if c == "watch-stable" => (|| {
            sync::cmd_watch_stable(
                Path::new(p),
                parse_u64(t)?,
                parse_u64(s)?,
                Some(parse_fp(base)?),
            )
        })(),
        [c, root, rid, tpl] if c == "sync-init" => {
            sync::cmd_sync_init(Path::new(root), rid, Path::new(tpl))
        }
        [c, d, parent, rid] if c == "sync-guard" => {
            sync::cmd_sync_guard(Path::new(d), Path::new(parent), rid, false)
        }
        [c, d, parent, rid, del] if c == "sync-guard" && del == "--delete" => {
            sync::cmd_sync_guard(Path::new(d), Path::new(parent), rid, true)
        }
        _ => {
            eprintln!(
                "usage:\n  inspect <xlsx>\n  rmw <xlsx> [umya|zip]\n  diff <before> <after>\n  \
                 stale-scan <xlsx>  |  verify-structure <xlsx|dir>  |  resolve <path>\n  fingerprint <xlsx>\n  restore <xlsx> <last-hex|none> <true|false>\n  \
                 check-write <xlsx> <cell>\n  \
                 marker <xlsx> [ecCell] [userCell]\n  watch-stable <file> [timeoutS] [stableS] [baselineHex]\n  \
                 sync-init <mirror-root> <run-id> <template>\n  \
                 sync-guard <dir> <expected-parent> <run-id> [--delete]"
            );
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A minimal but realistic package: the target sheet has D2 as a number, plus a formula cell
    // whose cached <v> we can tamper with; a second part stands in for "everything else".
    const TARGET: &str = "xl/worksheets/sheet1.xml";
    fn sheet_before() -> Vec<u8> {
        br#"<worksheet><sheetData><row r="2"><c r="C2"><v>2</v></c><c r="D2" s="3"><v>400</v></c><c r="E2" s="3"><f>C2*D2</f><v>800</v></c></row></sheetData></worksheet>"#.to_vec()
    }
    fn workbook_before() -> Vec<u8> {
        br#"<workbook><sheets><sheet name="BOM" sheetId="1"/></sheets><calcPr calcId="191029"/></workbook>"#.to_vec()
    }
    fn other_part() -> Vec<u8> {
        br#"<xml>styles etc</xml>"#.to_vec()
    }

    fn before_map() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (TARGET.to_string(), sheet_before()),
            ("xl/workbook.xml".to_string(), workbook_before()),
            ("xl/styles.xml".to_string(), other_part()),
        ])
    }

    /// The good case: exactly the two intended edits (D2 -> 1234, fullCalcOnLoad on workbook).
    fn after_ok() -> BTreeMap<String, Vec<u8>> {
        let sheet = patch_cell(
            &String::from_utf8(sheet_before()).unwrap(),
            TARGET_CELL,
            TARGET_VALUE,
        )
        .unwrap();
        let wb = patch_calc_pr(&String::from_utf8(workbook_before()).unwrap());
        BTreeMap::from([
            (TARGET.to_string(), sheet.into_bytes()),
            ("xl/workbook.xml".to_string(), wb.into_bytes()),
            ("xl/styles.xml".to_string(), other_part()),
        ])
    }

    fn eval(a: &BTreeMap<String, Vec<u8>>, b: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
        let mut log = Vec::new();
        evaluate(a, b, TARGET, TARGET_CELL, &mut log)
    }

    #[test]
    fn intended_two_edits_pass() {
        assert!(eval(&before_map(), &after_ok()).is_empty());
    }

    // The false negatives the old marker-count diff let through (reviewer, PR #20):

    #[test]
    fn tampering_another_cells_value_fails() {
        // E2 cached value 800 -> 999 while keeping D2 correct. Marker counts are unchanged.
        let mut after = after_ok();
        let sheet = String::from_utf8(after[TARGET].clone())
            .unwrap()
            .replace("<v>800</v>", "<v>999</v>");
        after.insert(TARGET.to_string(), sheet.into_bytes());
        assert!(
            !eval(&before_map(), &after).is_empty(),
            "a value swap elsewhere must FAIL"
        );
    }

    #[test]
    fn number_to_shared_string_fails() {
        // D2 turned from number into a shared-string ref: <v> count same, t="s" appears.
        let mut after = after_ok();
        let sheet = String::from_utf8(after[TARGET].clone()).unwrap().replace(
            "<c r=\"D2\" s=\"3\"><v>1234</v></c>",
            "<c r=\"D2\" s=\"3\" t=\"s\"><v>0</v></c>",
        );
        after.insert(TARGET.to_string(), sheet.into_bytes());
        assert!(
            !eval(&before_map(), &after).is_empty(),
            "number->sharedString must FAIL"
        );
    }

    #[test]
    fn full_calc_on_load_zero_fails() {
        // fullCalcOnLoad="0" — the old check only looked for the attribute name.
        let mut after = after_ok();
        let wb = String::from_utf8(after["xl/workbook.xml"].clone())
            .unwrap()
            .replace("fullCalcOnLoad=\"1\"", "fullCalcOnLoad=\"0\"");
        after.insert("xl/workbook.xml".to_string(), wb.into_bytes());
        assert!(
            !eval(&before_map(), &after).is_empty(),
            "fullCalcOnLoad=0 must FAIL"
        );
    }

    #[test]
    fn added_part_fails() {
        let mut after = after_ok();
        after.insert("xl/sneaky.xml".to_string(), b"<x/>".to_vec());
        assert!(
            !eval(&before_map(), &after).is_empty(),
            "an added part must FAIL"
        );
    }

    #[test]
    fn untouched_part_change_fails() {
        let mut after = after_ok();
        after.insert("xl/styles.xml".to_string(), b"<xml>TAMPERED</xml>".to_vec());
        assert!(
            !eval(&before_map(), &after).is_empty(),
            "a change to a copied part must FAIL"
        );
    }

    // ---- write guard: never write into a formula cell (PR #21 review finding 1) ----

    /// One sheet exercising every formula flavour: B2 normal, E2 shared anchor, E3 shared
    /// follower (self-closing <f/>), J2 array anchor; D2/K5 plain values; J6 has no <f> but sits
    /// inside the J5:J7 spill of an array formula.
    const GUARD_SHEET: &str = r#"<worksheet><sheetData>
        <row r="2"><c r="B2"><f>"CBT3-"&amp;A2</f><v>CBT3-1</v></c><c r="D2" s="3"><v>400</v></c><c r="E2"><f t="shared" ref="E2:E3" si="0">C2*D2</f><v>800</v></c><c r="J2"><f t="array" ref="J5:J7">_xlfn.UNIQUE(A2:A4)</f><v>x</v></c></row>
        <row r="3"><c r="E3"><f t="shared" si="0"/><v>1350</v></c></row>
        <row r="5"><c r="K5"><v>7</v></c></row>
    </sheetData></worksheet>"#;

    fn guard_blocked(cell: &str) -> bool {
        let has_formula = cell_has_formula(GUARD_SHEET, cell);
        let (ranges, unresolved) = spill_ranges(GUARD_SHEET);
        let refs: Vec<&str> = ranges.iter().map(|s| s.as_str()).collect();
        has_formula || state::write_blocked(&[cell], &refs, unresolved)
    }

    #[test]
    fn writing_normal_formula_cell_is_blocked() {
        assert!(guard_blocked("B2"), "normal formula cell must be refused");
    }

    #[test]
    fn writing_shared_anchor_is_blocked() {
        assert!(guard_blocked("E2"), "shared-formula anchor must be refused");
    }

    #[test]
    fn writing_shared_follower_is_blocked() {
        // Follower carries only a self-closing <f t="shared" si="0"/> — no formula text.
        assert!(
            guard_blocked("E3"),
            "shared-formula follower must be refused"
        );
    }

    #[test]
    fn writing_spill_result_cell_is_blocked() {
        // J6 has no <c> element at all, but it lies inside the array ref J5:J7.
        assert!(guard_blocked("J6"), "spill result cell must be refused");
    }

    #[test]
    fn writing_plain_value_cell_is_allowed() {
        assert!(
            !guard_blocked("D2"),
            "plain value cell clear of spills must be allowed"
        );
        assert!(
            !guard_blocked("K5"),
            "plain value cell clear of spills must be allowed"
        );
    }
}
