// Data model for the BOM editor. Mirrors the TS types in src/types/bom.ts and
// the spec in docs/plans/0003-bom-editor/plan.md §4. JSON uses camelCase.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

fn one_f() -> f64 {
    1.0
}
fn one_i() -> i64 {
    1
}

/// MISUMI/supplier link on a column (optional). Drives the cell from a supplier field.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ColumnLink {
    /// Dotted path on SupplierQuote, e.g. "quote.unitPrice" / "product.name" / "raw.*".
    pub field: String,
    /// "overwrite" | "fillEmpty" | "suggest"
    pub write: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ColumnDef {
    pub key: String,
    pub label: String,
    /// "core" | "custom" | "supplier"
    pub kind: String,
    pub editable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<ColumnLink>,
    /// Designated role for the fetch pipeline: "partNo" (型番列) | "source" (EC発注先列).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SupplierProduct {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_no: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SupplierPricing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_price_tax: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tax_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lead_time_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_qty: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtotal: Option<f64>,
}

/// Normalized supplier result (canonical "quote"). Provider-specific data in `raw`.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SupplierQuote {
    pub supplier_code: String,
    /// "idle" | "pending" | "ok" | "error"
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<SupplierProduct>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<SupplierPricing>,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct BomRow {
    /// `#[serde(default)]` so JSON import of rows lacking `id` parses (→ ""), then
    /// `bom_import` assigns a fresh id. UI-created rows always set a UUID.
    #[serde(default)]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts_no: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qty: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub material: Option<String>,
    #[serde(default)]
    pub custom: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplier: Option<SupplierQuote>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct BomMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<String>,
    #[serde(default = "one_f")]
    pub qty_multiplier: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BomDoc {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default = "one_i")]
    pub version: i64,
    #[serde(default)]
    pub meta: BomMeta,
    #[serde(default)]
    pub columns: Vec<ColumnDef>,
    #[serde(default)]
    pub rows: Vec<BomRow>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BomSummary {
    pub id: String,
    pub name: Option<String>,
    pub row_count: i64,
    pub updated_at: Option<String>,
}

/// One appended price/delivery observation for a (supplier, part number), read back
/// from `supplier_price_history` for the history view (Phase 3). Newest first.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PriceHistoryEntry {
    pub fetched_at: String,
    pub unit_price: Option<String>,
    pub currency: Option<String>,
    pub ship_date: Option<String>,
    /// Immediate-shippable stock at fetch time (recorded from schema V3 on; NULL before).
    pub stock: Option<i64>,
}
