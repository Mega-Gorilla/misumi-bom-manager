// Spreadsheet (Excel .xlsx/.xls/.ods and CSV) read & write for BOM import/export.
//
// Rust only converts between a file and a flat string grid; BomDoc construction and
// column mapping live on the frontend (the interactive import wizard). This keeps the
// backend thin and lets the UI drive "which row / which column maps to what".

use calamine::{open_workbook_auto, Data, Reader};
use serde::Serialize;
use std::path::Path;

/// Safety cap on rows read from a single sheet (BOMs are small; guards against a
/// pathological file blowing up the IPC payload). Surfaced to the user via a warning.
const MAX_ROWS: usize = 5000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SheetGrid {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    /// True if the sheet was truncated at MAX_ROWS.
    pub truncated: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Workbook {
    pub sheets: Vec<SheetGrid>,
}

fn ext_lower(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn file_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Sheet1")
        .to_string()
}

/// Convert a calamine cell to a display string. Whole floats render without a
/// trailing ".0" (e.g. Qty 8.0 -> "8"); everything else uses calamine's Display.
fn data_to_string(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => fmt_num(*f),
        // Bool / DateTime / DateTimeIso / DurationIso / Error (+ future non_exhaustive)
        other => other.to_string(),
    }
}

fn fmt_num(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{}", f)
    }
}

/// Read a workbook (xlsx/xls/ods) or a CSV file into a flat string grid.
pub fn read_workbook(path: &str) -> Result<Workbook, String> {
    if ext_lower(path) == "csv" {
        read_csv(path)
    } else {
        read_excel(path)
    }
}

fn read_excel(path: &str) -> Result<Workbook, String> {
    let mut wb = open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names = wb.sheet_names().to_owned();
    let mut sheets = Vec::new();
    for name in names {
        let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
        let total = range.rows().count();
        let rows: Vec<Vec<String>> = range
            .rows()
            .take(MAX_ROWS)
            .map(|row| row.iter().map(data_to_string).collect())
            .collect();
        sheets.push(SheetGrid {
            name,
            rows,
            truncated: total > MAX_ROWS,
        });
    }
    if sheets.is_empty() {
        return Err("ワークブックにシートがありません".into());
    }
    Ok(Workbook { sheets })
}

fn read_csv(path: &str) -> Result<Workbook, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = decode_bytes(&bytes);
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut truncated = false;
    for rec in rdr.records() {
        let rec = rec.map_err(|e| e.to_string())?;
        rows.push(rec.iter().map(|s| s.to_string()).collect());
        if rows.len() >= MAX_ROWS {
            truncated = true;
            break;
        }
    }
    Ok(Workbook {
        sheets: vec![SheetGrid {
            name: file_stem(path),
            rows,
            truncated,
        }],
    })
}

/// Decode CSV bytes: strip a UTF-8 BOM, else try strict UTF-8, else fall back to
/// Shift-JIS (common for CSVs saved by Japanese Excel).
fn decode_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode(bytes).0.into_owned(),
    }
}

/// Write a flat grid (header row + data rows) to xlsx or CSV (by path extension).
pub fn write_grid(path: &str, headers: &[String], rows: &[Vec<String>]) -> Result<(), String> {
    if ext_lower(path) == "csv" {
        write_csv(path, headers, rows)
    } else {
        write_xlsx(path, headers, rows)
    }
}

fn write_csv(path: &str, headers: &[String], rows: &[Vec<String>]) -> Result<(), String> {
    let mut wtr = csv::WriterBuilder::new().from_writer(Vec::new());
    wtr.write_record(headers).map_err(|e| e.to_string())?;
    for row in rows {
        wtr.write_record(row).map_err(|e| e.to_string())?;
    }
    let data = wtr.into_inner().map_err(|e| e.to_string())?;
    // Prepend a UTF-8 BOM so Excel opens Japanese text without mojibake.
    let mut out = Vec::with_capacity(data.len() + 3);
    out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    out.extend_from_slice(&data);
    std::fs::write(path, out).map_err(|e| e.to_string())
}

fn write_xlsx(path: &str, headers: &[String], rows: &[Vec<String>]) -> Result<(), String> {
    use rust_xlsxwriter::{Format, Workbook as XlsxWorkbook};
    let mut wb = XlsxWorkbook::new();
    let sheet = wb.add_worksheet();
    let bold = Format::new().set_bold();
    for (c, h) in headers.iter().enumerate() {
        sheet
            .write_string_with_format(0, c as u16, h, &bold)
            .map_err(|e| e.to_string())?;
    }
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            sheet
                .write_string((r + 1) as u32, c as u16, cell)
                .map_err(|e| e.to_string())?;
        }
    }
    wb.save(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xlsx_write_then_read_roundtrip() {
        let path = std::env::temp_dir().join("mbm_test_roundtrip.xlsx");
        let p = path.to_str().unwrap();
        let headers = vec!["Parts No".to_string(), "Qty".to_string()];
        let rows = vec![
            vec!["CBT3-8".to_string(), "2".to_string()],
            vec!["SFB6-20".to_string(), "10".to_string()],
        ];
        write_grid(p, &headers, &rows).unwrap();
        let wb = read_workbook(p).unwrap();
        assert_eq!(wb.sheets.len(), 1);
        let s = &wb.sheets[0];
        assert_eq!(s.rows[0], headers);
        assert_eq!(s.rows[1], vec!["CBT3-8".to_string(), "2".to_string()]);
        assert_eq!(s.rows[2][0], "SFB6-20");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn csv_roundtrip_preserves_japanese() {
        let path = std::env::temp_dir().join("mbm_test_roundtrip.csv");
        let p = path.to_str().unwrap();
        let headers = vec!["型番".to_string(), "数量".to_string()];
        let rows = vec![vec!["部品A".to_string(), "3".to_string()]];
        write_grid(p, &headers, &rows).unwrap();
        let wb = read_workbook(p).unwrap();
        assert_eq!(wb.sheets[0].rows[0], headers);
        assert_eq!(wb.sheets[0].rows[1], rows[0]);
        let _ = std::fs::remove_file(p);
    }
}
