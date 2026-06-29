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
      c.kind !== "supplier" && c.editable && c.link && c.key !== partKey && c.key !== srcKey,
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

/** ColDef extras for an editable column linked to a supplier field: live diff highlight
 *  (cell value vs fetched) + a "MISUMI: <value>" tooltip. Stateless. */
function linkExtras(c: ColumnDef, sourceCol?: ColumnDef): Partial<ColDef<BomRow>> {
  if (!c.link || c.kind === "supplier") return {};
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
        return String(cur).trim() !== String(fetched).trim();
      },
      "cell-link-suggest": (p) => {
        if (policy !== "suggest" || !p.data) return false;
        const fetched = getSupplierFieldValue(p.data, field, sourceCol);
        if (fetched == null || fetched === "") return false;
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
 *  valueSetter semantics (numeric parse for No./Qty, custom -> row.custom). */
export function setCellValue(row: BomRow, col: ColumnDef, value: unknown): BomRow {
  if (col.kind === "supplier" || !col.editable) return row;
  if (col.kind === "custom") {
    return { ...row, custom: { ...row.custom, [col.key]: value == null ? "" : String(value) } };
  }
  // Changing the part number invalidates any fetched EC result for this row.
  const r = col.key === "partsNo" && row.supplier ? { ...row, supplier: undefined } : row;
  if (NUMERIC_CORE_KEYS.has(col.key)) {
    const n = Number(value);
    return { ...r, [col.key]: Number.isFinite(n) ? n : undefined };
  }
  return { ...r, [col.key]: value == null ? "" : String(value) };
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
function supplierValue(
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
