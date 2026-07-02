import { useMemo, useState } from "react";
import { X, Check } from "lucide-react";
import type { BomDoc, BomRow, Workbook } from "../types/bom";
import { CORE_COLUMNS, NUMERIC_CORE_KEYS, newBom, newRow } from "../types/bom";

interface Props {
  workbook: Workbook;
  fileName: string;
  path: string;
  onCancel: () => void;
  onConfirm: (doc: BomDoc) => void;
}

const SKIP = ""; // 取込しない
const NEW = "__new__"; // 新規列として追加
const PREVIEW_ROWS = 20;

// Header-text → core key guesses (normalized, lowercase, spaces/punct stripped).
const GUESS: { key: string; hints: string[] }[] = [
  { key: "no", hints: ["no", "no.", "#", "番号", "項番", "連番"] },
  { key: "partsNo", hints: ["partsno", "partnumber", "partno", "品番", "型番", "品目コード", "partsnumber"] },
  { key: "partsName", hints: ["partsname", "name", "品名", "名称", "部品名"] },
  { key: "order", hints: ["order", "発注", "発注先", "調達先", "仕入先"] },
  { key: "qty", hints: ["qty", "quantity", "数量", "員数", "個数"] },
  { key: "material", hints: ["material", "材質", "材料"] },
];

function normalize(s: string): string {
  return s.trim().toLowerCase().replace(/[\s()（）_.-]/g, "");
}

function guessKey(header: string): string {
  const n = normalize(header);
  if (!n) return SKIP;
  for (const g of GUESS) {
    if (g.hints.some((h) => normalize(h) === n)) return g.key;
  }
  return SKIP;
}

function slug(s: string): string {
  return (
    s
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "_")
      .replace(/^_|_$/g, "") || "col"
  );
}

function uniqueKey(used: Set<string>, base: string): string {
  let key = `c_${base}`;
  let i = 1;
  while (used.has(key)) key = `c_${base}_${i++}`;
  return key;
}

export function ImportWizard(p: Props) {
  const sheets = p.workbook.sheets;
  const [sheetIndex, setSheetIndex] = useState(0);
  const [hasHeader, setHasHeader] = useState(true);
  const [headerRow, setHeaderRow] = useState(0); // 0-based
  const [dataStartRow, setDataStartRow] = useState(1); // 0-based
  // Column → target: "" skip / core key / "__new__". Keyed by sheet so switching resets.
  const [mapping, setMapping] = useState<string[]>([]);
  const [mappedSheet, setMappedSheet] = useState(-1);

  const sheet = sheets[sheetIndex] ?? { name: "", rows: [], truncated: false };
  const colCount = useMemo(
    () => sheet.rows.reduce((m, r) => Math.max(m, r.length), 0),
    [sheet],
  );

  const headerLabels = useMemo(() => {
    const src = hasHeader ? sheet.rows[headerRow] ?? [] : [];
    return Array.from({ length: colCount }, (_, i) => (src[i] ?? "").trim());
  }, [sheet, headerRow, hasHeader, colCount]);

  // (Re)initialize the mapping with auto-guesses when the sheet or header changes.
  const effectiveMapping = useMemo(() => {
    if (mappedSheet === sheetIndex && mapping.length === colCount) return mapping;
    const guessed = Array.from({ length: colCount }, (_, i) =>
      hasHeader ? guessKey(headerLabels[i]) : SKIP,
    );
    return guessed;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sheetIndex, colCount, hasHeader, headerLabels]);

  // Keep state in sync when the derived guess set replaces the mapping.
  const currentMapping = mappedSheet === sheetIndex && mapping.length === colCount ? mapping : effectiveMapping;

  const setTarget = (ci: number, value: string) => {
    const next = [...currentMapping];
    next[ci] = value;
    setMapping(next);
    setMappedSheet(sheetIndex);
  };

  const changeSheet = (idx: number) => {
    setSheetIndex(idx);
    setHeaderRow(0);
    setDataStartRow(1);
    setMappedSheet(-1); // force re-guess for the new sheet
  };

  const changeHeaderRow = (oneBased: number) => {
    const hr = Math.max(0, oneBased - 1);
    setHeaderRow(hr);
    setDataStartRow(hr + 1); // data starts right after the header by default (editable)
    setMappedSheet(-1); // re-guess against the new header
  };

  const mappedCount = currentMapping.filter((m) => m !== SKIP).length;
  const dataRowCount = useMemo(() => {
    let n = 0;
    for (let r = dataStartRow; r < sheet.rows.length; r++) {
      const cells = sheet.rows[r];
      if (cells && cells.some((c) => (c ?? "").trim() !== "")) n++;
    }
    return n;
  }, [sheet, dataStartRow]);

  const coreByKey = useMemo(() => new Map(CORE_COLUMNS.map((c) => [c.key, c])), []);

  const buildDoc = (): BomDoc => {
    const doc = newBom(p.fileName || "取込BOM");
    const cols = [...doc.columns];
    const used = new Set(cols.map((c) => c.key));
    // Resolve each spreadsheet column to a target column key (creating custom columns
    // for "__new__"). Role columns default to Parts No / ORDER via CORE_COLUMNS.
    const targets: (string | null)[] = currentMapping.map((m, ci) => {
      if (m === SKIP) return null;
      if (m === NEW) {
        const label = (headerLabels[ci] || `列${ci + 1}`).trim();
        const key = uniqueKey(used, slug(label));
        used.add(key);
        cols.push({ key, label, kind: "custom", editable: true, width: 140 });
        return key;
      }
      return m; // existing core key
    });

    const rows: BomRow[] = [];
    let seq = 1;
    for (let r = dataStartRow; r < sheet.rows.length; r++) {
      const cells = sheet.rows[r];
      if (!cells || cells.every((c) => (c ?? "").trim() === "")) continue; // skip blank rows
      const row = newRow();
      const rec = row as unknown as Record<string, unknown>;
      let mappedNo: number | undefined;
      for (let ci = 0; ci < targets.length; ci++) {
        const key = targets[ci];
        if (!key) continue;
        const raw = (cells[ci] ?? "").trim();
        if (coreByKey.has(key)) {
          if (NUMERIC_CORE_KEYS.has(key)) {
            const num = Number(raw);
            const val = raw !== "" && Number.isFinite(num) ? num : undefined;
            rec[key] = val;
            if (key === "no") mappedNo = val;
          } else {
            rec[key] = raw;
          }
        } else {
          row.custom[key] = raw;
        }
      }
      row.no = mappedNo ?? seq;
      seq = (row.no ?? seq) + 1;
      rows.push(row);
    }
    doc.columns = cols;
    doc.rows = rows.length ? rows : [newRow(1)];
    doc.meta.importedFrom = p.path;
    return doc;
  };

  const previewRows = Math.min(sheet.rows.length, PREVIEW_ROWS);
  const canConfirm = mappedCount > 0 && dataRowCount > 0;

  return (
    <div className="col-mgr-backdrop" onClick={p.onCancel}>
      <div className="col-mgr import-wizard" onClick={(e) => e.stopPropagation()}>
        <div className="col-mgr-head">
          <strong>取込プレビュー — {p.fileName}</strong>
          <button className="icon-btn" onClick={p.onCancel} title="閉じる">
            <X size={16} />
          </button>
        </div>

        <div className="iw-controls">
          {sheets.length > 1 && (
            <label>
              シート
              <select value={sheetIndex} onChange={(e) => changeSheet(Number(e.currentTarget.value))}>
                {sheets.map((s, i) => (
                  <option key={i} value={i}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className="iw-check">
            <input
              type="checkbox"
              checked={hasHeader}
              onChange={(e) => {
                setHasHeader(e.currentTarget.checked);
                setMappedSheet(-1);
              }}
            />
            ヘッダ行あり
          </label>
          {hasHeader && (
            <label>
              ヘッダ行
              <input
                type="number"
                min={1}
                max={sheet.rows.length}
                value={headerRow + 1}
                onChange={(e) => changeHeaderRow(Number(e.currentTarget.value))}
              />
            </label>
          )}
          <label>
            取込開始行
            <input
              type="number"
              min={1}
              max={Math.max(1, sheet.rows.length)}
              value={dataStartRow + 1}
              onChange={(e) => setDataStartRow(Math.max(0, Number(e.currentTarget.value) - 1))}
            />
          </label>
          <span className="iw-summary">
            {mappedCount} 列を取込 / {dataRowCount} 行
            {sheet.truncated && "（先頭のみ表示・上限到達）"}
          </span>
        </div>

        <div className="iw-preview">
          <table className="col-table iw-table">
            <thead>
              <tr>
                <th className="iw-rownum">行</th>
                {Array.from({ length: colCount }, (_, ci) => (
                  <th key={ci}>
                    <select value={currentMapping[ci] ?? SKIP} onChange={(e) => setTarget(ci, e.currentTarget.value)}>
                      <option value={SKIP}>取込しない</option>
                      {CORE_COLUMNS.map((c) => (
                        <option key={c.key} value={c.key}>
                          {c.label}
                        </option>
                      ))}
                      <option value={NEW}>＋ 新規列として追加</option>
                    </select>
                    {hasHeader && <div className="iw-header-label">{headerLabels[ci] || `列${ci + 1}`}</div>}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {Array.from({ length: previewRows }, (_, r) => {
                const cells = sheet.rows[r] ?? [];
                const cls = hasHeader && r === headerRow ? "iw-hdr" : r < dataStartRow ? "iw-skip" : "";
                return (
                  <tr key={r} className={cls}>
                    <td className="iw-rownum">{r + 1}</td>
                    {Array.from({ length: colCount }, (_, ci) => (
                      <td key={ci}>{cells[ci] ?? ""}</td>
                    ))}
                  </tr>
                );
              })}
            </tbody>
          </table>
          {sheet.rows.length > previewRows && (
            <div className="iw-more">… 他 {sheet.rows.length - previewRows} 行（取込は全行対象）</div>
          )}
        </div>

        <div className="iw-actions">
          <button onClick={p.onCancel}>キャンセル</button>
          <button
            className="primary"
            disabled={!canConfirm}
            title={canConfirm ? "取込を実行" : "取込する列と行を指定してください"}
            onClick={() => p.onConfirm(buildDoc())}
          >
            <Check size={15} /> この内容で取込
          </button>
        </div>
      </div>
    </div>
  );
}
