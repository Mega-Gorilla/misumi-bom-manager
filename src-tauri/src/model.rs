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
    /// Designated role for the fetch pipeline: "partNo" (型番列) | "source" (EC発注先列) |
    /// "orderNo1"|"orderNo2"|"orderNo3" (お客様注文番号列; joined into customerItemSubReference).
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
    /// Separator used to join お客様注文番号1/2/3 columns into the single
    /// customerItemSubReference at cart-add time. None → frontend default (space).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_no_separator: Option<String>,
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

// ---- Excel link mode enums (schema V5) ----------------------------------------------
//
// Wire values (serde) and DB CHECK values share the SAME snake_case strings on purpose:
// a dual mapping (camelCase on the wire, snake_case in SQL) is a standing source of
// conversion bugs, and the TS side has no literals for these yet (they arrive in PR-6).
// `as_str`/`from_db` below are the single DB conversion point and MUST stay in sync
// with the CHECK constraints in db.rs V5.

/// Structural/sync health of a linked BOM (`bom_link_state.sync_status`).
/// Transitions: plan.md §4.9 (3判定) + §4.2.2 (conflict); see implementation.md §1.3.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    Linked,
    NeedsReview,
    Broken,
    Conflict,
}

impl SyncStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Linked => "linked",
            Self::NeedsReview => "needs_review",
            Self::Broken => "broken",
            Self::Conflict => "conflict",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "linked" => Some(Self::Linked),
            "needs_review" => Some(Self::NeedsReview),
            "broken" => Some(Self::Broken),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// Formula-cache trust state of the linked workbook (`bom_link_state.calc_state`,
/// plan.md §4.4). Workbook-level: MVP marks ALL formula cells stale at once (§4.4.2).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CalcState {
    Unverified,
    Trusted,
    Stale,
    Missing,
}

impl CalcState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Trusted => "trusted",
            Self::Stale => "stale",
            Self::Missing => "missing",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "unverified" => Some(Self::Unverified),
            "trusted" => Some(Self::Trusted),
            "stale" => Some(Self::Stale),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// Link-environment verdict (`bom_link.env_verdict`, plan.md §4.10): writeback is
/// allowed only for a resolved local NTFS real path; everything else degrades to
/// read-only linking (fail closed — also the §6.2 fallback switch).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EnvVerdict {
    Allow,
    NoWriteback,
}

impl EnvVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::NoWriteback => "no_writeback",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "no_writeback" => Some(Self::NoWriteback),
            _ => None,
        }
    }
}

/// Column ownership in the structure contract (`bom_link_column.ownership`,
/// plan.md §4.3): app-owned (written back), user-owned (never written), or skipped.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LinkOwnership {
    App,
    User,
    Skipped,
}

impl LinkOwnership {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::App => "app",
            Self::User => "user",
            Self::Skipped => "skipped",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "app" => Some(Self::App),
            "user" => Some(Self::User),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

/// How an EC field projects onto a contract column (`bom_link_column.projection`):
/// written back into Excel (app columns) or shown in-app only (user columns, §4.3 —
/// the old fillEmpty/overwrite import semantics do not survive continuous sync).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum LinkProjection {
    Writeback,
    Suggest,
}

impl LinkProjection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Writeback => "writeback",
            Self::Suggest => "suggest",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "writeback" => Some(Self::Writeback),
            "suggest" => Some(Self::Suggest),
            _ => None,
        }
    }
}

/// Where a replace-backup file currently lives (`bom_link_backup.location`,
/// plan.md §4.2.2 6-step transfer): still beside the workbook (same volume) or
/// safely moved into app data (outside any cloud-synced folder — §3.12 case-08).
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BackupLocation {
    VolumeTemp,
    AppData,
}

impl BackupLocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::VolumeTemp => "volume_temp",
            Self::AppData => "app_data",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "volume_temp" => Some(Self::VolumeTemp),
            "app_data" => Some(Self::AppData),
            _ => None,
        }
    }
}

// ---- Excel link mode IPC views (PR-3: probe/create/open/unlink) ---------------------

/// Structure verdict of a read (§4.9 3判定), returned as a SUCCESS value — Err is
/// reserved for I/O and DB failures (implementation.md §2.3).
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StructureVerdict {
    /// Auto-adopted; `new_columns` lists newly imported user column labels.
    Safe { new_columns: Vec<String> },
    /// Sync stopped; the user must confirm a candidate or remap (PR-5).
    /// `structure_fp` is the freshness guard the confirm call must echo back.
    Confirm {
        reasons: Vec<String>,
        candidates: Vec<LinkResolutionCandidate>,
        structure_fp: String,
    },
    /// Nothing imported, writing forbidden.
    Broken { reasons: Vec<String> },
}

/// One applicable contract update, self-contained: the confirm flow (PR-5) maps an
/// accepted candidate onto SQL without further judgement (contract.rs is the single
/// place that decides what is offered).
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum LinkResolutionCandidate {
    AdoptSheetRename {
        new_sheet: String,
    },
    AdoptHeaderRowMove {
        new_header_row: i64,
        new_data_start_row: i64,
    },
    AdoptColumnMove {
        app_key: String,
        new_excel_col: i64,
    },
    AdoptRename {
        app_key: String,
        new_label: String,
    },
    DropOptionalColumn {
        app_key: String,
    },
    /// Frontend fills in a fresh app_key when the user picks this.
    ImportSkippedAsUser {
        excel_col: i64,
        label: String,
    },
    KeepSkipped {
        excel_col: i64,
        new_label: Option<String>,
    },
    SkipColumn {
        excel_col: i64,
    },
}

/// A formula-bearing cell (0-based absolute coordinates) for the fx display (§9-8).
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FormulaCell {
    pub row: i64,
    pub col: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formula: Option<String>,
}

/// The composed state of a linked BOM returned by open/create (§2.3).
/// On Confirm/Broken, `doc` is the PREVIOUS snapshot (bom_column/bom_row cache).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkedBomView {
    pub doc: BomDoc,
    pub verdict: StructureVerdict,
    pub calc_state: CalcState,
    pub sync_status: SyncStatus,
    pub env: crate::excel_link::env::EnvCheck,
    pub formula_cells: Vec<FormulaCell>,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

/// Wizard probe (read-only look at a workbook before linking).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkProbe {
    pub env: crate::excel_link::env::EnvCheck,
    pub sheets: Vec<SheetProbe>,
    pub warnings: Vec<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SheetProbe {
    pub name: String,
    /// First rows × columns as display strings (wizard preview).
    pub preview: Vec<Vec<String>>,
    /// 1-based heuristic suggestion (row with the most non-empty string cells).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_header_row: Option<i64>,
    pub truncated: bool,
}

/// Input of excel_link_create (the wizard's outcome).
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkCreateConfig {
    /// None = create a fresh BOM for this link.
    #[serde(default)]
    pub bom_id: Option<String>,
    /// BOM display name when creating fresh.
    #[serde(default)]
    pub name: Option<String>,
    pub workbook_path: String,
    pub sheet_name: String,
    /// 1-based.
    pub header_row: i64,
    /// 1-based; must be > header_row.
    pub data_start_row: i64,
    pub columns: Vec<LinkColumnConfig>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkColumnConfig {
    /// 0-based Excel column index.
    pub excel_col: i64,
    #[serde(default)]
    pub header_label: Option<String>,
    /// None = skipped column.
    #[serde(default)]
    pub app_key: Option<String>,
    pub ownership: LinkOwnership,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub source_field: Option<String>,
    #[serde(default)]
    pub projection: Option<LinkProjection>,
}
