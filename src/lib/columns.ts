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
import { NUMERIC_CORE_KEYS, ORDER_OPTIONS } from "../types/bom";

export function buildColumnDefs(doc: BomDoc): ColDef<BomRow>[] {
  return doc.columns.map(toColDef);
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
  if (NUMERIC_CORE_KEYS.has(col.key)) {
    const n = Number(value);
    return { ...row, [col.key]: Number.isFinite(n) ? n : undefined };
  }
  return { ...row, [col.key]: value == null ? "" : String(value) };
}

function toColDef(c: ColumnDef): ColDef<BomRow> {
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
    };
  }

  if (c.kind === "supplier") {
    return {
      ...base,
      editable: false,
      valueGetter: (p: ValueGetterParams<BomRow>) => supplierValue(p.data, c.link?.field),
      cellClassRules: {
        "cell-error": (p) => p.data?.supplier?.status === "error",
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
  return core;
}

/** Resolve a supplier column value: computed keys (status/messages/fetchedAt) or a
 *  dotted path on the row's SupplierQuote (e.g. "quote.unitPrice"). */
function supplierValue(row: BomRow | undefined, field?: string): unknown {
  const s = row?.supplier;
  if (!s || !field) return "";
  if (field === "status") return s.status ?? "";
  if (field === "messages") return [...(s.errors ?? []), ...(s.warnings ?? [])].join(" / ");
  if (field === "fetchedAt") return s.fetchedAt ?? "";
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
