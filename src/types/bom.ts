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

/** ORDER (supplier/source) dropdown choices. "MISUMI" drives the PR-C bulk lookup. */
export const ORDER_OPTIONS = ["", "MISUMI", "3D-PRINTED", "OTHER"];

export const CORE_COLUMNS: ColumnDef[] = [
  { key: "no", label: "No.", kind: "core", editable: true, width: 72 },
  { key: "partsNo", label: "Parts No", kind: "core", editable: true, width: 180 },
  { key: "partsName", label: "Parts Name", kind: "core", editable: true, width: 200 },
  { key: "order", label: "ORDER", kind: "core", editable: true, width: 120 },
  { key: "qty", label: "Qty", kind: "core", editable: true, width: 90 },
  { key: "material", label: "MATERIAL", kind: "core", editable: true, width: 160 },
];

/** MISUMI/supplier fields that can be added as read-only linked columns.
 *  `field` is a dotted path on SupplierQuote, or a computed key (status/messages).
 *  `linkable` = concrete data field that may also drive an existing editable column
 *  (write policy / PR-D). Computed/meta fields (status/messages/fetchedAt/subtotal) are not. */
export interface SupplierFieldDef {
  field: string;
  label: string;
  width?: number;
  linkable?: boolean;
}

/** Write-policy choices for a linked editable column (ColumnLink.write). */
export const WRITE_POLICIES: { value: WritePolicy; label: string }[] = [
  { value: "fillEmpty", label: "空欄補完" },
  { value: "overwrite", label: "上書き" },
  { value: "suggest", label: "提案" },
];

// "EC " prefix distinguishes fetched supplier columns from the user's own BOM columns
// (品名/出荷日 等と紛らわしいため)。"在庫" は即時出荷可能数(immediateShippableQty)
// なので意味を明確化して "即納在庫数"。
export const SUPPLIER_FIELDS: SupplierFieldDef[] = [
  { field: "product.name", label: "EC 品名", width: 200, linkable: true },
  { field: "quote.unitPrice", label: "EC 単価(税別)", width: 120, linkable: true },
  { field: "quote.unitPriceTax", label: "EC 単価(税込)", width: 120, linkable: true },
  { field: "quote.shipDate", label: "EC 出荷日", width: 120, linkable: true },
  { field: "quote.stock", label: "EC 即納在庫数", width: 120, linkable: true },
  { field: "quote.moq", label: "EC 最小数量", width: 110, linkable: true },
  { field: "quote.subtotal", label: "EC 小計", width: 110 },
  { field: "status", label: "EC 状態", width: 90 },
  { field: "messages", label: "EC メッセージ", width: 240 },
  { field: "fetchedAt", label: "EC 取得日時", width: 160 },
];

/** Fields seeded as supplier columns on a new BOM. `status` is intentionally NOT
 *  seeded (redundant with the message column + red error highlight); it stays in
 *  SUPPLIER_FIELDS so it can still be added via the column manager on demand. */
const DEFAULT_SUPPLIER_FIELDS = [
  "quote.unitPrice",
  "quote.shipDate",
  "quote.stock",
  "messages",
];

function supplierKey(field: string): string {
  return "s_" + field.replace(/[^a-zA-Z0-9]+/g, "_").toLowerCase();
}

export function newSupplierColumn(field: string, label: string, width?: number): ColumnDef {
  return {
    key: supplierKey(field),
    label,
    kind: "supplier",
    editable: false,
    width: width ?? 120,
    link: { field, write: "fillEmpty" },
  };
}

function defaultSupplierColumns(): ColumnDef[] {
  return DEFAULT_SUPPLIER_FIELDS.map((f) => {
    const def = SUPPLIER_FIELDS.find((s) => s.field === f)!;
    return newSupplierColumn(def.field, def.label, def.width);
  });
}

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
    columns: [...CORE_COLUMNS.map((c) => ({ ...c })), ...defaultSupplierColumns()],
    rows: [newRow(1)],
  };
}
