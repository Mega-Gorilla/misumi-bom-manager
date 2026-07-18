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

type R<T> = Result<T, String>;

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

fn slice_between<'a>(hay: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let i = hay.find(start)? + start.len();
    let j = hay[i..].find(end)? + i;
    Some(&hay[i..j])
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

/// Map "BOM" to its sheetN.xml part, via workbook.xml order (sheet order == sheetN order for
/// files Excel writes; a real implementation must follow the r:id relationships instead).
fn sheet_part_for(zip: &mut zip::ZipArchive<File>, sheet_name: &str) -> R<String> {
    let mut wb = String::new();
    zip.by_name("xl/workbook.xml")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut wb)
        .map_err(|e| e.to_string())?;
    let idx = wb
        .match_indices("<sheet ")
        .position(|(i, _)| {
            let tag_end = wb[i..].find("/>").map(|j| i + j).unwrap_or(wb.len());
            wb[i..tag_end].contains(&format!("name=\"{sheet_name}\""))
        })
        .ok_or_else(|| format!("sheet {sheet_name} not in workbook.xml"))?;
    Ok(format!("xl/worksheets/sheet{}.xml", idx + 1))
}

fn cmd_rmw_zip(path: &Path) -> R<()> {
    let out = out_path(path, "zip")?;
    println!("== read-modify-write via surgical zip edit ==");
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

        if name == part || name == "xl/workbook.xml" {
            let mut s = String::new();
            e.read_to_string(&mut s).map_err(|e| e.to_string())?;
            let patched = if name == part {
                patch_cell(&s, TARGET_CELL, TARGET_VALUE)
                    .ok_or_else(|| format!("cell {TARGET_CELL} not found in {part}"))?
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
    println!("\n   set {TARGET_SHEET}!{TARGET_CELL} = {TARGET_VALUE}");
    println!("   set calcPr fullCalcOnLoad=\"1\"");
    println!(
        "\n   wrote {} bytes",
        std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0)
    );
    Ok(())
}

fn cmd_diff(before: &Path, after: &Path) -> R<()> {
    let a = Profile::read(before)?;
    let b = Profile::read(after)?;

    println!("== diff ==");
    println!("   before: {}", before.display());
    println!("   after : {}", after.display());

    // Violations are anything the surgical edit must NOT do. The only tolerated change is calcPr
    // (we deliberately set fullCalcOnLoad). Everything else — a lost part, a lost worksheet, a
    // lost formula, a numeric cell turned into text, or fullCalcOnLoad missing — is a failure.
    // Collected here so the command can exit non-zero (reviewer #3): a green diff becomes a
    // machine-checkable go/no-go, not just a wall of text.
    let mut violations: Vec<String> = Vec::new();

    // 1. package parts
    let lost: Vec<_> = a
        .entries
        .iter()
        .filter(|e| !b.entries.contains(e))
        .collect();
    let added: Vec<_> = b
        .entries
        .iter()
        .filter(|e| !a.entries.contains(e))
        .collect();
    println!(
        "\n-- package parts: {} -> {} --",
        a.entries.len(),
        b.entries.len()
    );
    if lost.is_empty() {
        println!("   [ ok ] no part lost");
    } else {
        for e in &lost {
            println!("   [LOST] {e}");
            violations.push(format!("part lost: {e}"));
        }
    }
    for e in &added {
        println!("   [ +  ] {e}");
    }

    // 2. workbook-level
    println!("\n-- workbook.xml --");
    verdict("calcPr", &a.calc_pr(), &b.calc_pr()); // allowed to differ (fullCalcOnLoad)
    let has_fco = b.calc_pr().contains("fullCalcOnLoad");
    println!(
        "   {} fullCalcOnLoad present in output: {}",
        if has_fco { "[ ok ]" } else { "[FAIL]" },
        has_fco
    );
    if !has_fco {
        violations.push("fullCalcOnLoad not set in output".into());
    }
    num("definedName", a.defined_names(), b.defined_names());
    if b.defined_names() < a.defined_names() {
        violations.push("definedName count dropped".into());
    }

    // 3. per sheet
    for (name, ma) in &a.sheets {
        let Some(mb) = b.sheets.get(name) else {
            println!("\n-- {name} --\n   [LOST] worksheet part missing in output");
            violations.push(format!("worksheet lost: {name}"));
            continue;
        };
        println!("\n-- {name} --");
        if ma == mb {
            println!("   [ ok ] all markers unchanged");
        }
        for ((label, na), (_, nb)) in ma.rows().into_iter().zip(mb.rows()) {
            if na != nb {
                num(label, na, nb);
                if nb < na {
                    violations.push(format!("{name}: {} dropped ({na} -> {nb})", label.trim()));
                }
            }
        }
        let fa = a.formulas.get(name).cloned().unwrap_or_default();
        let fb = b.formulas.get(name).cloned().unwrap_or_default();
        for f in fa.iter().filter(|f| !fb.contains(f)) {
            println!("   [LOST] formula: {f}");
            violations.push(format!("{name}: formula lost: {f}"));
        }
        for f in fb.iter().filter(|f| !fa.contains(f)) {
            println!("   [ +  ] formula: {f}");
        }
    }

    println!("\n-- verdict --");
    if violations.is_empty() {
        println!("   [PASS] no disallowed change (only calcPr may differ)");
        Ok(())
    } else {
        println!("   [FAIL] {} violation(s):", violations.len());
        for v in &violations {
            println!("      - {v}");
        }
        Err(format!("{} violation(s) — see above", violations.len()))
    }
}

fn num(label: &str, a: usize, b: usize) {
    let tag = if b < a {
        "[LOST]"
    } else if b > a {
        "[ +  ]"
    } else {
        "[ ok ]"
    };
    println!("   {tag} {label:34} {a} -> {b}");
}

fn verdict(label: &str, a: &str, b: &str) {
    let tag = if a == b { "[ ok ]" } else { "[DIFF]" };
    println!("   {tag} {label}");
    println!("          before: {a}");
    println!("          after : {b}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.as_slice() {
        [c, p] if c == "inspect" => cmd_inspect(Path::new(p)),
        [c, p] if c == "rmw" => cmd_rmw_umya(Path::new(p)),
        [c, p, b] if c == "rmw" && b == "umya" => cmd_rmw_umya(Path::new(p)),
        [c, p, b] if c == "rmw" && b == "zip" => cmd_rmw_zip(Path::new(p)),
        [c, a, b] if c == "diff" => cmd_diff(Path::new(a), Path::new(b)),
        _ => {
            eprintln!("usage:\n  inspect <xlsx>\n  rmw <xlsx> [umya|zip]\n  diff <before> <after>");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
