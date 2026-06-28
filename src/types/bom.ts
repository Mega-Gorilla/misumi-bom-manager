// Mirrors src-tauri/src/model.rs (serde camelCase). See docs/plans/0003-bom-editor/plan.md §4.

export type ColumnKind = "core" | "custom" | "supplier";
export type WritePolicy = "overwrite" | "fillEmpty" | "suggest";

export interface ColumnLink {
  /** Dotted path on SupplierQuote, e.g. "quote.unitPrice" / "product.name". */
  field: string;
  write: WritePolicy;
}

export interface ColumnDef {
  key: string;
  label: string;
  kind: ColumnKind;
  editable: boolean;
  width?: number;
  link?: ColumnLink;
}

export interface SupplierProduct {
  name?: string;
  brand?: string;
  partNo?: string;
  category?: string;
}

export interface SupplierPricing {
  currency?: string;
  unitPrice?: string;
  unitPriceTax?: string;
  taxRate?: string;
  shipDate?: string;
  leadTimeDays?: number;
  stock?: number;
  moq?: number;
  packQty?: number;
  subtotal?: number;
}

export interface SupplierQuote {
  supplierCode: string;
  status: "idle" | "pending" | "ok" | "error";
  product?: SupplierProduct;
  quote?: SupplierPricing;
  errors: string[];
  warnings: string[];
  fetchedAt?: string;
  raw?: unknown;
}

export interface BomRow {
  id: string;
  no?: number;
  partsName?: string;
  partsNo?: string;
  order?: string;
  qty?: number;
  material?: string;
  custom: Record<string, string>;
  supplier?: SupplierQuote;
}

export interface BomMeta {
  name?: string;
  importedFrom?: string;
  qtyMultiplier: number;
  updatedAt?: string;
}

export interface BomDoc {
  id?: string;
  version: number;
  meta: BomMeta;
  columns: ColumnDef[];
  rows: BomRow[];
}

export interface BomSummary {
  id: string;
  name?: string | null;
  rowCount: number;
  updatedAt?: string | null;
}

/** Field keys that live on the BomRow top level (vs. row.custom[...]). */
export const CORE_KEYS = ["no", "partsName", "partsNo", "order", "qty", "material"] as const;
export const NUMERIC_CORE_KEYS = new Set<string>(["no", "qty"]);

export const CORE_COLUMNS: ColumnDef[] = [
  { key: "no", label: "No.", kind: "core", editable: true, width: 72 },
  { key: "partsNo", label: "Parts No", kind: "core", editable: true, width: 180 },
  { key: "partsName", label: "Parts Name", kind: "core", editable: true, width: 200 },
  { key: "order", label: "ORDER", kind: "core", editable: true, width: 120 },
  { key: "qty", label: "Qty", kind: "core", editable: true, width: 90 },
  { key: "material", label: "MATERIAL", kind: "core", editable: true, width: 160 },
];

export function newRowId(): string {
  return crypto.randomUUID();
}

export function newRow(no?: number): BomRow {
  return { id: newRowId(), no, custom: {} };
}

/** Next sequential No. = max existing No. + 1 (avoids duplicates on add/duplicate). */
export function nextNo(rows: BomRow[]): number {
  return rows.reduce((m, r) => Math.max(m, r.no ?? 0), 0) + 1;
}

export function newBom(name = "新規 BOM"): BomDoc {
  return {
    id: crypto.randomUUID(),
    version: 1,
    meta: { name, qtyMultiplier: 1 },
    columns: CORE_COLUMNS.map((c) => ({ ...c })),
    rows: [newRow(1)],
  };
}
