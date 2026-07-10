// Build AG Grid column defs from BomDoc.columns.
//  - core   : top-level BomRow field (no/partsName/partsNo/order/qty/material)
//  - custom : row.custom[key] via value getter/setter
//  - supplier (read-only): resolves link.field dotted path on row.supplier (PR-C populates it)

import type {
  ColDef,
  ValueGetterParams,
  ValueParserParams,
  ValueSetterParams,
} from "ag-grid-community";
import type { BomDoc, BomRow, ColumnDef } from "../types/bom";
import { NUMERIC_CORE_KEYS, ORDER_OPTIONS, partNoColumn, sourceColumn } from "../types/bom";

export function buildColumnDefs(doc: BomDoc): ColDef<BomRow>[] {
  // The source column (designated EC発注先 role) drives the ORDER gate; bake it into the
  // column defs so getters/cellClassRules use the right column even if it's re-designated.
  const sourceCol = sourceColumn(doc);
  return doc.columns.map((c) => toColDef(c, sourceCol));
}

/** Apply linked-column write policies to all rows using the current supplier results.
 *  Called after a bulk fetch and when a link is configured. Only editable columns with
 *  a link (excluding partsNo/order) on ORDER-matching rows are affected:
 *    overwrite -> always write the fetched value; fillEmpty -> only blank cells;
 *    suggest   -> no write (shown via diff highlight / tooltip). */
export function applyLinkedColumns(doc: BomDoc): BomDoc {
  const sourceCol = sourceColumn(doc);
  // The role columns (型番列 / EC発注先列) are fetch keys, never fill targets.
  const partKey = partNoColumn(doc)?.key;
  const srcKey = sourceCol?.key;
  const links = doc.columns.filter(
    (c) =>
      c.kind !== "supplier" &&
      c.editable &&
      c.link &&
      !c.role &&
      c.key !== partKey &&
      c.key !== srcKey,
  );
  if (links.length === 0) return doc;
  const rows = doc.rows.map((r) => {
    if (!supplierActive(r, sourceCol)) return r;
    let nr = r;
    for (const c of links) {
      const fetched = getSupplierFieldValue(nr, c.link!.field, sourceCol);
      if (fetched == null || fetched === "") continue;
      if (c.link!.write === "overwrite") {
        nr = setCellValue(nr, c, fetched);
      } else if (c.link!.write === "fillEmpty") {
        const cur = getCellValue(nr, c);
        if (cur == null || String(cur).trim() === "") nr = setCellValue(nr, c, fetched);
      }
      // suggest: no write (display only)
    }
    return nr;
  });
  return { ...doc, rows };
}

/** Reserved row.custom key marking that a linked column's difference vs the fetched EC
 *  value has been reconciled ("現在の値を採用"). It stores the fetched value it was resolved
 *  against, so a later fetch that changes that value re-flags the cell. Hidden (no column
 *  uses this key, so it never displays or exports) and persisted for free via row.custom. */
export const ackKey = (colKey: string): string => `__mbmAck:${colKey}`;

/** True if the cell was reconciled against the current fetched value (highlight suppressed). */
function isResolved(row: BomRow, colKey: string, fetched: unknown): boolean {
  const ack = row.custom?.[ackKey(colKey)];
  return ack != null && ack === String(fetched ?? "").trim();
}

/** ColDef extras for an editable column linked to a supplier field: live diff highlight
 *  (cell value vs fetched) + a "MISUMI: <value>" tooltip. Stateless. */
function linkExtras(c: ColumnDef, sourceCol?: ColumnDef): Partial<ColDef<BomRow>> {
  // A role column (型番列 / EC発注先列) is a fetch key, never an EC fill target — ignore any
  // stale link it may still carry so no diff highlight / tooltip is shown on it.
  if (!c.link || c.kind === "supplier" || c.role) return {};
  const field = c.link.field;
  const policy = c.link.write;
  const extras: Partial<ColDef<BomRow>> = {
    cellClassRules: {
      "cell-link-diff": (p) => {
        if (!p.data) return false;
        const fetched = getSupplierFieldValue(p.data, field, sourceCol);
        if (fetched == null || fetched === "") return false;
        const cur = getCellValue(p.data, c);
        if (cur == null || String(cur).trim() === "") return false;
        if (isResolved(p.data, c.key, fetched)) return false; // reconciled
        return String(cur).trim() !== String(fetched).trim();
      },
      "cell-link-suggest": (p) => {
        if (policy !== "suggest" || !p.data) return false;
        const fetched = getSupplierFieldValue(p.data, field, sourceCol);
        if (fetched == null || fetched === "") return false;
        if (isResolved(p.data, c.key, fetched)) return false; // reconciled
        const cur = getCellValue(p.data, c);
        return cur == null || String(cur).trim() === "";
      },
    },
    tooltipValueGetter: (p) => {
      const fetched = p.data ? getSupplierFieldValue(p.data, field, sourceCol) : "";
      return fetched != null && fetched !== "" ? `MISUMI: ${fetched}` : "";
    },
  };
  return extras;
}

/** Read a cell's value for a column (used by the fill handle). Supplier columns are
 *  read-only and return undefined. */
export function getCellValue(row: BomRow, col: ColumnDef): unknown {
  if (col.kind === "supplier") return undefined;
  if (col.kind === "custom") return row.custom?.[col.key] ?? "";
  return (row as unknown as Record<string, unknown>)[col.key];
}

/** Return a new row with `col` set to `value` (immutable). Mirrors the column
 *  valueSetter semantics (numeric parse for No./Qty, custom -> row.custom). Role-agnostic:
 *  invalidating the fetched EC result when the 型番列 changes is the caller's job (it knows
 *  the current partNo-role column via partNoColumn(doc)), since this pure setter has no doc. */
export function setCellValue(row: BomRow, col: ColumnDef, value: unknown): BomRow {
  if (col.kind === "supplier" || !col.editable) return row;
  if (col.kind === "custom") {
    return { ...row, custom: { ...row.custom, [col.key]: value == null ? "" : String(value) } };
  }
  if (NUMERIC_CORE_KEYS.has(col.key)) {
    const n = Number(value);
    return { ...row, [col.key]: Number.isFinite(n) ? n : undefined };
  }
  return { ...row, [col.key]: value == null ? "" : String(value) };
}

/** Display string for a cell, matching what the grid shows — used for export.
 *  Editable columns render their own value; supplier columns render the (ORDER-gated,
 *  qty-live) fetched value exactly as the grid does. */
export function cellDisplayValue(doc: BomDoc, row: BomRow, col: ColumnDef, sourceCol?: ColumnDef): string {
  if (col.kind === "supplier") {
    const v = supplierValue(row, col.link?.field, doc.meta.qtyMultiplier ?? 1, sourceCol ?? sourceColumn(doc));
    return v == null ? "" : String(v);
  }
  const v = getCellValue(row, col);
  return v == null ? "" : String(v);
}

/** Build a flat export grid: header labels + one display string per column per row.
 *  Columns are emitted in their current order, including fetched EC (supplier) values. */
export function buildExportGrid(doc: BomDoc): { headers: string[]; rows: string[][] } {
  const sourceCol = sourceColumn(doc);
  const headers = doc.columns.map((c) => c.label);
  const rows = doc.rows.map((r) => doc.columns.map((c) => cellDisplayValue(doc, r, c, sourceCol)));
  return { headers, rows };
}

/** BOM-wide aggregate over the fetched EC data (Phase 3 合計). Computed live from the
 *  current rows so it tracks Qty/ORDER edits without a re-fetch. */
export interface BomTotals {
  /** Currency of the summed amount (first seen among active rows; "JPY" default). */
  currency: string;
  /** Σ subtotal (unitPrice × Qty × qtyMultiplier) over active rows with a numeric price. */
  totalAmount: number;
  /** Active rows that contributed a numeric amount. */
  pricedRows: number;
  /** Rows whose ORDER still matches the supplier they were fetched from. */
  activeRows: number;
  /** Active rows whose fetch ended in error. */
  errorRows: number;
  /** Latest ship date among active rows ("" if none). MISUMI dates are zero-padded
   *  YYYY-MM-DD, so a string max equals the chronological max. */
  latestShipDate: string;
}

/** Aggregate the fetched EC data across the BOM. Reuses supplierValue/supplierActive so the
 *  numbers match the grid's per-row supplier columns exactly. */
export function computeTotals(doc: BomDoc): BomTotals {
  const sourceCol = sourceColumn(doc);
  const mult = doc.meta.qtyMultiplier ?? 1;
  let currency = "";
  let totalAmount = 0;
  let pricedRows = 0;
  let activeRows = 0;
  let errorRows = 0;
  let latestShipDate = "";
  for (const r of doc.rows) {
    if (!supplierActive(r, sourceCol)) continue;
    activeRows++;
    if (r.supplier?.status === "error") errorRows++;
    if (!currency) currency = r.supplier?.quote?.currency ?? "";
    // supplierValue returns "" when there's no unit price; Number("") is 0 (finite), so gate
    // on the raw value being non-empty before counting it as a priced row.
    const subRaw = supplierValue(r, "quote.subtotal", mult, sourceCol);
    const sub = Number(subRaw);
    if (String(subRaw) !== "" && Number.isFinite(sub)) {
      totalAmount += sub;
      pricedRows++;
    }
    const ship = String(supplierValue(r, "quote.shipDate", mult, sourceCol) ?? "").trim();
    if (ship && ship > latestShipDate) latestShipDate = ship;
  }
  return {
    currency: currency || "JPY",
    totalAmount,
    pricedRows,
    activeRows,
    errorRows,
    latestShipDate,
  };
}

/** If a linked editable column has a fetched EC value that differs from the cell's current
 *  value (an empty cell counts as differing), return both so the UI can offer to adopt it.
 *  Returns null when there is nothing to reconcile: no link, a role/fetch-key column, no
 *  fetched value (or ORDER not matching), or the cell already equals the fetched value. */
export function pendingSuggestion(
  doc: BomDoc,
  row: BomRow,
  col: ColumnDef,
): { current: string; fetched: string } | null {
  if (!col.link || col.kind === "supplier" || col.role || !col.editable) return null;
  const sourceCol = sourceColumn(doc);
  if (col.key === partNoColumn(doc)?.key || col.key === sourceCol?.key) return null;
  const fetchedRaw = getSupplierFieldValue(row, col.link.field, sourceCol);
  const fetched = fetchedRaw == null ? "" : String(fetchedRaw);
  if (fetched.trim() === "") return null;
  if (isResolved(row, col.key, fetchedRaw)) return null; // already reconciled
  const curRaw = getCellValue(row, col);
  const current = curRaw == null ? "" : String(curRaw);
  if (current.trim() === fetched.trim()) return null;
  return { current, fetched };
}

function toColDef(c: ColumnDef, sourceCol?: ColumnDef): ColDef<BomRow> {
  const base: ColDef<BomRow> = {
    colId: c.key,
    headerName: c.label,
    width: c.width,
    editable: c.kind !== "supplier" && c.editable,
  };

  if (c.kind === "custom") {
    return {
      ...base,
      valueGetter: (p: ValueGetterParams<BomRow>) => p.data?.custom?.[c.key] ?? "",
      valueSetter: (p: ValueSetterParams<BomRow>) => {
        if (!p.data.custom) p.data.custom = {};
        p.data.custom[c.key] = p.newValue == null ? "" : String(p.newValue);
        return true;
      },
      ...linkExtras(c, sourceCol),
    };
  }

  if (c.kind === "supplier") {
    return {
      ...base,
      editable: false,
      valueGetter: (p: ValueGetterParams<BomRow>) =>
        supplierValue(p.data, c.link?.field, p.context?.qtyMultiplier ?? 1, sourceCol),
      cellClassRules: {
        "cell-error": (p) =>
          supplierActive(p.data, sourceCol) && p.data?.supplier?.status === "error",
      },
    };
  }

  // core
  const numeric = NUMERIC_CORE_KEYS.has(c.key);
  const core: ColDef<BomRow> = {
    ...base,
    field: c.key as keyof BomRow & string,
    rowDrag: c.key === "no", // drag handle on the No. column for row reordering
    pinned: c.key === "no" ? "left" : undefined, // keep No. visible while scrolling
    valueParser: numeric
      ? (p: ValueParserParams<BomRow>) => {
          const n = Number(p.newValue);
          return Number.isFinite(n) ? n : undefined;
        }
      : undefined,
  };
  if (c.key === "order") {
    // dropdown editor for the supplier/source (canonical values aid PR-C matching)
    core.cellEditor = "agSelectCellEditor";
    core.cellEditorParams = { values: ORDER_OPTIONS };
  }
  return { ...core, ...linkExtras(c, sourceCol) };
}

/** The row's supplier result applies only while its ORDER still matches the supplier
 *  it was fetched from. Changing ORDER away from MISUMI (or to blank) hides the data
 *  immediately, without needing a re-fetch; switching back re-shows the cached result. */
export function supplierActive(row: BomRow | undefined, sourceCol?: ColumnDef): boolean {
  const s = row?.supplier;
  if (!s) return false;
  const src = sourceCol && row ? getCellValue(row, sourceCol) : row?.order;
  return String(src ?? "").trim().toUpperCase() === (s.supplierCode ?? "").toUpperCase();
}

/** Raw fetched value for a supplier data field (dotted path on the quote), gated by the
 *  source (ORDER) column. Used to drive/compare linked editable columns. */
export function getSupplierFieldValue(
  row: BomRow | undefined,
  field: string,
  sourceCol?: ColumnDef,
): unknown {
  if (!supplierActive(row, sourceCol)) return "";
  return resolvePath(row!.supplier, field);
}

/** Resolve a supplier column value: computed keys (status/messages/fetchedAt/subtotal)
 *  or a dotted path on the row's SupplierQuote. Gated by ORDER. Qty-derived values
 *  (subtotal, MOQ note) are computed LIVE from the row's current Qty/multiplier so they
 *  never go stale after the user edits Qty. */
export function supplierValue(
  row: BomRow | undefined,
  field?: string,
  qtyMultiplier = 1,
  sourceCol?: ColumnDef,
): unknown {
  if (!field || !supplierActive(row, sourceCol)) return "";
  const s = row!.supplier!;
  const qty = row!.qty ?? 1;
  if (field === "status") return s.status ?? "";
  if (field === "fetchedAt") return (s.fetchedAt ?? "").slice(0, 16); // "YYYY-MM-DD HH:MM" (秒を除去)
  if (field === "messages") {
    const msgs = [...(s.errors ?? []), ...(s.warnings ?? [])];
    const moq = s.quote?.moq;
    if (moq != null && qty < moq) msgs.push(`最小発注数 ${moq}（現在 ${qty}）`);
    return msgs.join(" / ");
  }
  if (field === "quote.subtotal") {
    const unit = Number(s.quote?.unitPrice);
    return Number.isFinite(unit) ? unit * qty * (qtyMultiplier || 1) : "";
  }
  return resolvePath(s, field);
}

function resolvePath(obj: unknown, path?: string): unknown {
  if (obj == null || !path) return "";
  return (
    path
      .split(".")
      .reduce<unknown>((o, k) => (o == null ? o : (o as Record<string, unknown>)[k]), obj) ?? ""
  );
}
