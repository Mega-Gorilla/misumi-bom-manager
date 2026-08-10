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
    /// True when a bom_link row exists (Excel link mode). Display-only: set by
    /// load_bom, IGNORED on save (the bom_link row is the single source of truth
    /// and save_bom refuses linked BOMs anyway).
    #[serde(default)]
    pub linked: bool,
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
    /// Excel link mode: the list view badges linked BOMs and the open flow
    /// routes them through excel_link_open instead of bom_load.
    pub is_linked: bool,
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
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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
    /// Contract metadata per mapped column (role / EC projection). Deliberately
    /// SEPARATE from ColumnDef.link: the one-shot import write policies are
    /// normalized away for linked BOMs (§1.3) and must not be conflated with the
    /// contract's projection semantics. The UI (PR-6) maps EC display through this.
    pub columns_meta: Vec<LinkColumnMeta>,
    pub formula_cells: Vec<FormulaCell>,
    pub truncated: bool,
    pub warnings: Vec<String>,
    /// Live pending row (§4.2.1) — Some = a "反映待ち" is outstanding for this BOM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingInfo>,
}

/// One mapped contract column as the frontend needs it (bom_link_column projection).
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LinkColumnMeta {
    pub app_key: String,
    pub excel_col: i64,
    pub ownership: LinkOwnership,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_field: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<LinkProjection>,
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

/// Outcome of the quote command (implementation.md §2.3). `generation` is Some only
/// when the BOM is linked AND the adopted snapshot actually changed this run.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct QuoteOutcome {
    pub results: Vec<SupplierQuote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<i64>,
    pub failed: Vec<QuoteFailure>,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuoteFailure {
    pub parts_no: String,
    pub message: String,
}

/// Outcome of excel_link_apply = §4.2.2 steps 1-9 (implementation.md §2.3).
#[derive(Serialize, Clone, PartialEq, Debug)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApplyOutcome {
    /// Written and verified: backup fingerprint matched F0. `warnings` carries
    /// non-fatal after-effects (backup transfer/retention issues) — the write
    /// itself succeeded.
    Applied {
        generation: i64,
        fingerprint: String,
        warnings: Vec<String>,
    },
    /// Could not obtain the write handle (Excel has the file open) —
    /// bom_link_pending is recorded (§4.2.1; a later excel_link_apply retries).
    /// `warnings` carries recovery/transfer notices from the pre-write phase.
    Pending {
        reason: String,
        warnings: Vec<String>,
    },
    /// Nothing was written (fail closed) — the reason names the guard. `warnings`
    /// carries recovery/transfer notices from the pre-write phase (a refused
    /// apply may still have reconciled an interrupted write — PR-5).
    Refused {
        reason: RefuseReason,
        warnings: Vec<String>,
    },
    /// Replace happened but backup≠F0 (§4.2.2 step 8): sync stopped, the displaced
    /// external version is preserved in the backup.
    Conflict {
        backup_id: i64,
        backup_path: String,
        warnings: Vec<String>,
    },
}

/// Why a write was refused without touching the file (§4.2.2 guards).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum RefuseReason {
    Env,
    Structure,
    FingerprintChanged,
    /// An unresolved conflict backup exists for this BOM (§1.3: conflict stops
    /// sync until the USER resolves it) — no further write may run before that.
    UnresolvedConflict,
    Spill,
    FormulaCell,
    Truncated,
    NothingToWrite,
}

/// Lightweight link status (implementation.md §2.3: sync_status / calc_state /
/// pending / unresolved conflicts). DB reads plus one file-EXISTENCE check only —
/// excel_link_status must stay cheap enough to poll.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkStatus {
    pub sync_status: SyncStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_error: Option<String>,
    pub calc_state: CalcState,
    pub ec_generation: i64,
    pub applied_generation: i64,
    /// Some = a 反映待ち is outstanding (§4.2.1; the row itself is the state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<PendingInfo>,
    /// Unresolved conflict backups of THIS BOM (§4.2.2 step 8 evidence).
    pub conflicts: Vec<ConflictInfo>,
    /// Backups still sitting beside the workbook (volume_temp) — a transfer that
    /// failed once; retried on every open/apply (PR-4 review handover).
    pub untransferred: i64,
    /// A write journal survived reconciliation: apply/unlink/delete are refused
    /// until a link open recovers it (V6).
    pub recovery_pending: bool,
    /// The `~$` owner file exists beside the workbook — Excel LIKELY has it open.
    /// A hint only (§4.2.1): the authority stays the write-open attempt.
    pub excel_lock_hint: bool,
}

/// bom_link_pending row as the frontend sees it (§4.2.1 latest-wins).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PendingInfo {
    pub requested_generation: i64,
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<String>,
    pub attempt_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
}

/// One unresolved conflict backup (ledger projection for the status/resolve UI).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ConflictInfo {
    pub backup_id: i64,
    pub backup_path: String,
    pub created_at: String,
    /// false = still volume_temp beside the workbook (transfer pending).
    pub transferred: bool,
}

/// Input of excel_link_confirm: the accepted subset of the candidates a Confirm
/// verdict offered, plus the structure fingerprint of the read those candidates
/// were generated from (echo back — the freshness guard, implementation.md §2.3).
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkConfirmRequest {
    pub structure_fp: String,
    pub accepted: Vec<LinkResolutionCandidate>,
}

/// What excel_link_resolve_conflict does with the backup. An enum from day one so
/// PR-6+ can add actions (e.g. restoring the external version) without changing
/// the command shape; PR-5 records the user's resolution only.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum ConflictAction {
    /// The user has seen/kept what they need: mark the conflict resolved and lift
    /// the §1.3 sync stop.
    Resolved,
}

/// Input of excel_link_remap (PR-6): replace the whole contract — sheet/header
/// coordinates plus every column mapping — while KEEPING state, generation and
/// the adopted EC snapshots (unlike unlink+create, which resets them). The new
/// mapping must verify Safe against the current workbook; there is no freshness
/// guard because the command re-verifies the live file at execution time.
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LinkRemapRequest {
    pub sheet_name: String,
    pub header_row: i64,
    pub data_start_row: i64,
    pub columns: Vec<LinkColumnConfig>,
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

#[cfg(test)]
mod wire_tests {
    use super::*;

    fn keys(v: &serde_json::Value) -> Vec<String> {
        v.as_object().unwrap().keys().cloned().collect()
    }

    /// The TS types (src/types/bom.ts) mirror these wire shapes. The tagged enums
    /// carry `rename_all_fields = "camelCase"` (PR-6) so EVERYTHING on the wire is
    /// camelCase — this pins that contract against accidental serde changes.
    #[test]
    fn tagged_enums_serialize_fully_camel_case() {
        let v = serde_json::to_value(StructureVerdict::Safe {
            new_columns: vec!["x".into()],
        })
        .unwrap();
        assert_eq!(v["kind"], "safe");
        assert!(v.get("newColumns").is_some(), "{v}");

        let v = serde_json::to_value(StructureVerdict::Confirm {
            reasons: vec![],
            candidates: vec![LinkResolutionCandidate::AdoptColumnMove {
                app_key: "qty".into(),
                new_excel_col: 3,
            }],
            structure_fp: "fp".into(),
        })
        .unwrap();
        assert!(v.get("structureFp").is_some(), "{v}");
        let cand = &v["candidates"][0];
        assert_eq!(cand["kind"], "adoptColumnMove");
        assert!(
            cand.get("appKey").is_some() && cand.get("newExcelCol").is_some(),
            "{cand}"
        );

        let v = serde_json::to_value(ApplyOutcome::Conflict {
            backup_id: 1,
            backup_path: "p".into(),
            warnings: vec![],
        })
        .unwrap();
        assert_eq!(v["kind"], "conflict");
        assert!(
            v.get("backupId").is_some() && v.get("backupPath").is_some(),
            "{v}"
        );

        let v = serde_json::to_value(ApplyOutcome::Refused {
            reason: RefuseReason::FingerprintChanged,
            warnings: vec!["w".into()],
        })
        .unwrap();
        assert_eq!(v["reason"], "fingerprint_changed"); // enum VALUES stay snake_case (DB parity)
        assert_eq!(v["warnings"][0], "w");

        // Round-trip: the frontend echoes candidates back into confirm.
        let cand: LinkResolutionCandidate = serde_json::from_value(serde_json::json!({
            "kind": "keepSkipped", "excelCol": 5, "newLabel": null
        }))
        .unwrap();
        assert_eq!(
            cand,
            LinkResolutionCandidate::KeepSkipped {
                excel_col: 5,
                new_label: None
            }
        );
    }

    #[test]
    fn status_and_view_shapes_are_camel_case() {
        let st = LinkStatus {
            sync_status: SyncStatus::Linked,
            sync_error: None,
            calc_state: CalcState::Stale,
            ec_generation: 2,
            applied_generation: 1,
            pending: Some(PendingInfo {
                requested_generation: 2,
                requested_at: "t".into(),
                last_attempt_at: None,
                attempt_count: 1,
                blocked_reason: Some("file_open".into()),
            }),
            conflicts: vec![],
            untransferred: 0,
            recovery_pending: false,
            excel_lock_hint: true,
        };
        let v = serde_json::to_value(&st).unwrap();
        for k in [
            "syncStatus",
            "calcState",
            "ecGeneration",
            "appliedGeneration",
            "untransferred",
            "recoveryPending",
            "excelLockHint",
        ] {
            assert!(v.get(k).is_some(), "missing {k}: {:?}", keys(&v));
        }
        assert_eq!(v["syncStatus"], "linked");
        assert_eq!(v["calcState"], "stale");
        assert_eq!(v["pending"]["requestedGeneration"], 2);
        assert_eq!(v["pending"]["blockedReason"], "file_open");
    }
}
