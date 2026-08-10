// Structure contract + 3-way structure verdict for Excel link mode (plan.md §4.9,
// implementation.md §2.1 contract.rs). Key-based redesign of the PoC checker
// (tools/excel-link-poc/src/structure.rs): column identity is the stable app_key +
// last-confirmed position (excel_col) + last-confirmed display label — the label is
// NEVER a permanent identifier (§4.7). What survives from the PoC is the decision
// skeleton: collect ALL anomalies, then Broken > Confirm > Safe (fail closed).
//
// Unlike the PoC (which had a separate partial_row_checks path that took two review
// rounds to keep honest), this is a SINGLE pipeline: every phase always runs on the
// best-effort effective header row, so a Broken can never hide behind a Confirm by
// construction.

use crate::excel_link::store::{LinkColumn, LinkHeader};
use crate::model::{LinkOwnership, LinkResolutionCandidate, StructureVerdict};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// ---- contract view ------------------------------------------------------------------

/// Validated contract the checker operates on, built from the V5 rows.
pub struct Contract {
    pub sheet_name: String,
    pub header_row: u32,     // 1-based
    pub data_start_row: u32, // 1-based, > header_row (validated)
    pub mapped: Vec<MappedColumn>,
    pub skipped: Vec<SkippedColumn>,
}

pub struct MappedColumn {
    pub app_key: String,
    pub excel_col: u32, // last confirmed 0-based position (arbitrary, non-contiguous)
    pub header_label: String, // last confirmed display name (non-empty, validated)
    pub app_owned: bool,
    pub required: bool,
    /// Fetch-pipeline role ('partNo'/'source'/'orderNo1..3') — contract-owned so the
    /// composed view keeps driving the existing partNo/source column lookups.
    pub role: Option<String>,
    /// EC projection metadata (§1.3: replaces the one-shot link_field/link_write).
    pub source_field: Option<String>,
    pub projection: Option<crate::model::LinkProjection>,
}

pub struct SkippedColumn {
    pub excel_col: u32,
    pub header_label: Option<String>, // label seen at link time (may be None/empty)
}

/// Contract-side inconsistency: not a structure verdict but a broken invariant of
/// the stored contract itself. The caller maps it onto sync_status='broken'
/// (fail closed) — the checker only accepts contracts it can reason about.
#[derive(Debug, PartialEq)]
pub enum ContractError {
    MappedColumnWithoutLabel { excel_col: i64 },
    DuplicateMappedLabel { label: String },
    DuplicatePosition { excel_col: i64 },
    InvalidRows,
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MappedColumnWithoutLabel { excel_col } => write!(
                f,
                "E_CONTRACT_INVALID: マッピング済み列 (excel_col={excel_col}) にヘッダ表示名がありません"
            ),
            Self::DuplicateMappedLabel { label } => write!(
                f,
                "E_CONTRACT_INVALID: ヘッダ表示名 {label:?} が複数のマッピング済み列に使われています"
            ),
            Self::DuplicatePosition { excel_col } => {
                write!(f, "E_CONTRACT_INVALID: 列位置 {excel_col} が重複しています")
            }
            Self::InvalidRows => write!(
                f,
                "E_CONTRACT_INVALID: ヘッダ行とデータ開始行の関係が不正です (header_row < data_start_row が必要)"
            ),
        }
    }
}

impl Contract {
    /// The column whose values are the part numbers the EC fetch keys on: the
    /// role-designated column first, the legacy core key as fallback — the SAME
    /// resolution rule as `roleColumn()` in src/types/bom.ts (partNoColumn). The
    /// read side (compose) and the write side (plan_writeback) must both use this,
    /// or a BOM whose partNo role moved to a custom column fetches by one column
    /// and writes back by another (PR-4 review #1).
    pub fn part_no_column(&self) -> Option<&MappedColumn> {
        self.role_column("partNo", "partsNo")
    }

    /// The column whose value selects the EC source (matched against the payload's
    /// supplier code) — same fallback rule as `sourceColumn()` in types/bom.ts.
    pub fn source_column(&self) -> Option<&MappedColumn> {
        self.role_column("source", "order")
    }

    fn role_column(&self, role: &str, fallback_key: &str) -> Option<&MappedColumn> {
        self.mapped
            .iter()
            .find(|m| m.role.as_deref() == Some(role))
            .or_else(|| self.mapped.iter().find(|m| m.app_key == fallback_key))
    }

    /// Build + validate from the stored link header and column rows.
    pub fn try_from_store(
        header: &LinkHeader,
        columns: &[LinkColumn],
    ) -> Result<Contract, ContractError> {
        if header.header_row < 1 || header.data_start_row <= header.header_row {
            return Err(ContractError::InvalidRows);
        }
        let mut mapped = Vec::new();
        let mut skipped = Vec::new();
        let mut seen_pos = BTreeSet::new();
        let mut seen_label = BTreeSet::new();
        for c in columns {
            if !seen_pos.insert(c.excel_col) {
                return Err(ContractError::DuplicatePosition {
                    excel_col: c.excel_col,
                });
            }
            match c.ownership {
                LinkOwnership::Skipped => skipped.push(SkippedColumn {
                    excel_col: c.excel_col as u32,
                    header_label: c.header_label.clone(),
                }),
                own => {
                    let label = c
                        .header_label
                        .as_deref()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .ok_or(ContractError::MappedColumnWithoutLabel {
                            excel_col: c.excel_col,
                        })?
                        .to_string();
                    if !seen_label.insert(label.clone()) {
                        return Err(ContractError::DuplicateMappedLabel { label });
                    }
                    mapped.push(MappedColumn {
                        app_key: c.app_key.clone().ok_or(
                            // unreachable per DDL CHECK, defensive
                            ContractError::MappedColumnWithoutLabel {
                                excel_col: c.excel_col,
                            },
                        )?,
                        excel_col: c.excel_col as u32,
                        header_label: label,
                        app_owned: own == LinkOwnership::App,
                        required: c.required,
                        role: c.role.clone(),
                        source_field: c.source_field.clone(),
                        projection: c.projection,
                    });
                }
            }
        }
        Ok(Contract {
            sheet_name: header.sheet_name.clone(),
            header_row: header.header_row as u32,
            data_start_row: header.data_start_row as u32,
            mapped,
            skipped,
        })
    }

    /// First data row given the EFFECTIVE header row (§4.9 discussion: the gap
    /// between header and data start — unit rows, notes — is not app-writable, so
    /// formulas there are not Broken). Shared with the PR-4 write path.
    pub fn effective_data_start(&self, effective_header_row: u32) -> u32 {
        effective_header_row + (self.data_start_row - self.header_row)
    }
}

// ---- observation --------------------------------------------------------------------

/// What the reader observed about one worksheet. `labels` come from calamine
/// (inlineStr / entities / ruby resolved); formula POSITIONS come from the xlsx.rs
/// walker (shared-formula followers included); `occupied_data_cols` is restricted to
/// rows >= the contract data_start_row (title rows above the header must not fake
/// "headerless data column" confirms).
#[derive(Debug, Default, Clone)]
pub struct SheetObs {
    /// row (1-based) -> [(col 0-based, label)] for every string cell.
    pub labels: BTreeMap<u32, Vec<(u32, String)>>,
    /// (row 1-based, col 0-based) of every formula cell.
    pub formulas: Vec<(u32, u32)>,
    /// Columns holding real content on any DATA row.
    pub occupied_data_cols: BTreeSet<u32>,
}

impl SheetObs {
    fn row_labels(&self, row: u32) -> &[(u32, String)] {
        self.labels.get(&row).map(|v| v.as_slice()).unwrap_or(&[])
    }
}

// ---- anomalies ----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Safe,
    Confirm,
    Broken,
}

/// Every deviation the checker can observe. Severity/code/message/candidates live on
/// the variant — a Broken can not be filed as a Confirm by construction.
#[derive(Debug, Clone, PartialEq)]
pub enum Anomaly {
    // ---- Broken ----
    SheetMissing {
        sheet: String,
    },
    SheetAmbiguous {
        sheet: String,
        count: usize,
    },
    HeaderRowAmbiguous {
        rows: Vec<u32>,
    },
    NoContractHeader,
    DuplicateHeader {
        label: String,
        cols: Vec<u32>,
    },
    AppColumnMissing {
        app_key: String,
        label: String,
        replacement: Option<String>,
    },
    RequiredColumnMissing {
        app_key: String,
        label: String,
    },
    FormulaInAppColumn {
        app_key: String,
        label: String,
        col: u32,
        first_row: u32,
    },
    // ---- Confirm ----
    SheetRenamed {
        from: String,
        to: String,
    },
    HeaderRowMoved {
        from: u32,
        to: u32,
    },
    ColumnMoved {
        app_key: String,
        label: String,
        from: u32,
        to: u32,
        app_owned: bool,
    },
    RenameCandidate {
        app_key: String,
        old_label: String,
        new_label: String,
        col: u32,
    },
    OptionalColumnMissing {
        app_key: String,
        label: String,
    },
    SkippedColumnRelabeled {
        excel_col: u32,
        old_label: Option<String>,
        new_label: String,
    },
    NewColumnMatchesSkipped {
        label: String,
        col: u32,
        skipped_col: u32,
    },
    EmptyHeader {
        col: u32,
    },
    ReservedNameConflict {
        label: String,
        col: u32,
    },
    DuplicateNewColumn {
        label: String,
        col: u32,
    },
    HeaderlessData {
        col: u32,
    },
}

impl Anomaly {
    pub fn severity(&self) -> Severity {
        use Anomaly::*;
        match self {
            SheetMissing { .. }
            | SheetAmbiguous { .. }
            | HeaderRowAmbiguous { .. }
            | NoContractHeader
            | DuplicateHeader { .. }
            | AppColumnMissing { .. }
            | RequiredColumnMissing { .. }
            | FormulaInAppColumn { .. } => Severity::Broken,
            _ => Severity::Confirm,
        }
    }

    pub fn code(&self) -> &'static str {
        use Anomaly::*;
        match self {
            SheetMissing { .. } => "E_SHEET_MISSING",
            SheetAmbiguous { .. } => "E_SHEET_AMBIGUOUS",
            HeaderRowAmbiguous { .. } => "E_HEADER_ROW_AMBIGUOUS",
            NoContractHeader => "E_NO_CONTRACT_HEADER",
            DuplicateHeader { .. } => "E_DUP_HEADER",
            AppColumnMissing { .. } => "E_APP_COL_MISSING",
            RequiredColumnMissing { .. } => "E_REQUIRED_COL_MISSING",
            FormulaInAppColumn { .. } => "E_FORMULA_IN_APP_COL",
            SheetRenamed { .. } => "C_SHEET_RENAMED",
            HeaderRowMoved { .. } => "C_HEADER_ROW_MOVED",
            ColumnMoved { .. } => "C_COLUMN_MOVED",
            RenameCandidate { .. } => "C_RENAME",
            OptionalColumnMissing { .. } => "C_OPTIONAL_COL_MISSING",
            SkippedColumnRelabeled { .. } => "C_SKIPPED_RELABELED",
            NewColumnMatchesSkipped { .. } => "C_NEW_MATCHES_SKIPPED",
            EmptyHeader { .. } => "C_EMPTY_HEADER",
            ReservedNameConflict { .. } => "C_RESERVED_NAME",
            DuplicateNewColumn { .. } => "C_DUP_NEW_COLUMN",
            HeaderlessData { .. } => "C_HEADERLESS_DATA",
        }
    }

    /// Candidates a Confirm anomaly offers (empty for Broken/Safe-side variants).
    /// Each candidate is self-contained (implementation.md: PR-5 confirm maps the
    /// accepted candidate onto SQL without further judgement).
    pub fn candidates(&self, c: &Contract) -> Vec<LinkResolutionCandidate> {
        use Anomaly::*;
        match self {
            SheetRenamed { to, .. } => vec![LinkResolutionCandidate::AdoptSheetRename {
                new_sheet: to.clone(),
            }],
            HeaderRowMoved { to, .. } => vec![LinkResolutionCandidate::AdoptHeaderRowMove {
                new_header_row: *to as i64,
                new_data_start_row: c.effective_data_start(*to) as i64,
            }],
            ColumnMoved { app_key, to, .. } => vec![LinkResolutionCandidate::AdoptColumnMove {
                app_key: app_key.clone(),
                new_excel_col: *to as i64,
            }],
            RenameCandidate {
                app_key, new_label, ..
            } => vec![LinkResolutionCandidate::AdoptRename {
                app_key: app_key.clone(),
                new_label: new_label.clone(),
            }],
            OptionalColumnMissing { app_key, .. } => {
                vec![LinkResolutionCandidate::DropOptionalColumn {
                    app_key: app_key.clone(),
                }]
            }
            SkippedColumnRelabeled {
                excel_col,
                new_label,
                ..
            } => vec![
                LinkResolutionCandidate::KeepSkipped {
                    excel_col: *excel_col as i64,
                    new_label: Some(new_label.clone()),
                },
                LinkResolutionCandidate::ImportSkippedAsUser {
                    excel_col: *excel_col as i64,
                    label: new_label.clone(),
                },
            ],
            NewColumnMatchesSkipped { label, col, .. } => vec![
                LinkResolutionCandidate::ImportSkippedAsUser {
                    excel_col: *col as i64,
                    label: label.clone(),
                },
                LinkResolutionCandidate::SkipColumn {
                    excel_col: *col as i64,
                },
            ],
            EmptyHeader { col }
            | ReservedNameConflict { col, .. }
            | DuplicateNewColumn { col, .. }
            | HeaderlessData { col } => vec![LinkResolutionCandidate::SkipColumn {
                excel_col: *col as i64,
            }],
            _ => vec![],
        }
    }
}

impl std::fmt::Display for Anomaly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use Anomaly::*;
        write!(f, "{}: ", self.code())?;
        match self {
            SheetMissing { sheet } => write!(f, "対象シート {sheet:?} が見つかりません"),
            SheetAmbiguous { sheet, count } => write!(
                f,
                "対象シート {sheet:?} が無く、ヘッダ構成が {count} 枚のシートに見つかりました (一意に特定できません)"
            ),
            HeaderRowAmbiguous { rows } => {
                write!(f, "ヘッダ構成が複数行に見つかりました (行 {rows:?})")
            }
            NoContractHeader => write!(f, "契約のヘッダ構成がどこにも見つかりません"),
            DuplicateHeader { label, cols } => {
                write!(f, "ヘッダ {label:?} が複数の列にあります (列 {cols:?})")
            }
            AppColumnMissing {
                label, replacement, ..
            } => match replacement {
                Some(r) => write!(
                    f,
                    "アプリ所有列 {label:?} が見つかりません (元の位置には {r:?} があります — 改名された可能性)"
                ),
                None => write!(f, "アプリ所有列 {label:?} が削除または改名されています"),
            },
            RequiredColumnMissing { label, .. } => {
                write!(f, "必須列 {label:?} が見つかりません")
            }
            FormulaInAppColumn {
                label, first_row, ..
            } => write!(
                f,
                "アプリ所有列 {label:?} のデータ領域に数式があります (行 {first_row})"
            ),
            SheetRenamed { from, to } => {
                write!(f, "対象シートが {from:?} から {to:?} に改名された可能性")
            }
            HeaderRowMoved { from, to } => {
                write!(f, "ヘッダ行が {from} 行目から {to} 行目へ移動した可能性")
            }
            ColumnMoved {
                label, from, to, app_owned, ..
            } => write!(
                f,
                "{}列 {label:?} が列 {} から列 {} へ移動しています",
                if *app_owned { "アプリ所有" } else { "" },
                col_name(*from),
                col_name(*to)
            ),
            RenameCandidate {
                old_label, new_label, ..
            } => write!(
                f,
                "ヘッダが {old_label:?} から {new_label:?} に改名された可能性 (要確認)"
            ),
            OptionalColumnMissing { label, .. } => write!(f, "列 {label:?} が見当たりません"),
            SkippedColumnRelabeled {
                excel_col,
                new_label,
                ..
            } => write!(
                f,
                "読み飛ばし列 (列 {}) のヘッダが {new_label:?} に変わっています",
                col_name(*excel_col)
            ),
            NewColumnMatchesSkipped { label, col, .. } => write!(
                f,
                "新しい列 {label:?} (列 {}) は読み飛ばし列と同じ名前です",
                col_name(*col)
            ),
            EmptyHeader { col } => {
                write!(f, "列 {} に空のヘッダの新しい列があります", col_name(*col))
            }
            ReservedNameConflict { label, col } => write!(
                f,
                "新しい列 {label:?} (列 {}) はアプリ所有列の予約名と衝突します",
                col_name(*col)
            ),
            DuplicateNewColumn { label, col } => write!(
                f,
                "新しい列 {label:?} (列 {}) が重複しています",
                col_name(*col)
            ),
            HeaderlessData { col } => write!(
                f,
                "列 {} にデータがありますが利用可能なヘッダがありません",
                col_name(*col)
            ),
        }
    }
}

/// 0-based column index → A1-style letters, for messages (PoC col_name).
pub fn col_name(mut col: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s
}

// ---- verification -------------------------------------------------------------------

/// A cleanly importable new user column (position kept for the contract update).
#[derive(Debug, Clone, PartialEq)]
pub struct NewColumn {
    pub excel_col: u32,
    pub label: String,
}

pub struct VerifyOutcome {
    pub anomalies: Vec<Anomaly>,
    /// Cleanly importable new user columns (only acted on when severity is Safe).
    pub new_columns: Vec<NewColumn>,
    /// Effective sheet/header row the checks ran against (None when unresolvable).
    pub effective_sheet: Option<String>,
    pub effective_header_row: Option<u32>,
    /// structure-v1 fingerprint of (contract × observation projection) — §4.7.
    pub structure_fp: String,
}

impl VerifyOutcome {
    pub fn severity(&self) -> Severity {
        self.anomalies
            .iter()
            .map(Anomaly::severity)
            .max()
            .unwrap_or(Severity::Safe)
    }

    pub fn to_verdict(&self, c: &Contract) -> StructureVerdict {
        match self.severity() {
            Severity::Safe => StructureVerdict::Safe {
                new_columns: self.new_columns.iter().map(|n| n.label.clone()).collect(),
            },
            Severity::Confirm => StructureVerdict::Confirm {
                reasons: self.anomalies.iter().map(|a| a.to_string()).collect(),
                candidates: self
                    .anomalies
                    .iter()
                    .flat_map(|a| a.candidates(c))
                    .collect(),
                structure_fp: self.structure_fp.clone(),
            },
            Severity::Broken => StructureVerdict::Broken {
                reasons: self.anomalies.iter().map(|a| a.to_string()).collect(),
            },
        }
    }
}

/// Distinct mapped labels present on a row.
fn score(c: &Contract, row: &[(u32, String)]) -> usize {
    c.mapped
        .iter()
        .filter(|m| row.iter().any(|(_, l)| *l == m.header_label))
        .count()
}

/// Tolerant threshold: majority of mapped labels, at least 2 (a single generic
/// label like "No" matching a data row must not pass as the header).
fn tolerant_threshold(m: usize) -> usize {
    (m / 2 + 1).max(2)
}

pub fn verify_structure(c: &Contract, sheets: &[(String, SheetObs)]) -> VerifyOutcome {
    let mut anomalies: Vec<Anomaly> = Vec::new();
    let m = c.mapped.len();

    // ---- Phase A: sheet resolution (exact name > exact constellation > tolerant) ----
    let resolved = match sheets.iter().find(|(name, _)| *name == c.sheet_name) {
        Some((name, obs)) => Some((name.clone(), obs)),
        None => {
            let with_score = |min: usize| -> Vec<&(String, SheetObs)> {
                sheets
                    .iter()
                    .filter(|(_, o)| o.labels.values().any(|row| score(c, row) >= min))
                    .collect()
            };
            let mut candidates = with_score(m);
            if candidates.is_empty() && m > 0 {
                candidates = with_score(tolerant_threshold(m));
            }
            match candidates.as_slice() {
                [one] => {
                    anomalies.push(Anomaly::SheetRenamed {
                        from: c.sheet_name.clone(),
                        to: one.0.clone(),
                    });
                    Some((one.0.clone(), &one.1))
                }
                [] => {
                    anomalies.push(Anomaly::SheetMissing {
                        sheet: c.sheet_name.clone(),
                    });
                    None
                }
                many => {
                    anomalies.push(Anomaly::SheetAmbiguous {
                        sheet: c.sheet_name.clone(),
                        count: many.len(),
                    });
                    None
                }
            }
        }
    };
    let Some((sheet_name, obs)) = resolved else {
        return finish(c, anomalies, vec![], None, None, &SheetObs::default());
    };

    // ---- Phase B: effective header row ----
    let contract_score = score(c, obs.row_labels(c.header_row));
    let hdr = if contract_score == m && m > 0 {
        Some(c.header_row)
    } else {
        let exact: Vec<u32> = obs
            .labels
            .iter()
            .filter(|(_, row)| score(c, row) == m && m > 0)
            .map(|(r, _)| *r)
            .collect();
        match exact.as_slice() {
            [row] => {
                anomalies.push(Anomaly::HeaderRowMoved {
                    from: c.header_row,
                    to: *row,
                });
                Some(*row)
            }
            [] => {
                if contract_score >= 1 {
                    // Diagnose in place at the contract row (single renames land here).
                    Some(c.header_row)
                } else {
                    let tolerant: Vec<u32> = obs
                        .labels
                        .iter()
                        .filter(|(_, row)| score(c, row) >= tolerant_threshold(m))
                        .map(|(r, _)| *r)
                        .collect();
                    match tolerant.as_slice() {
                        [row] => {
                            anomalies.push(Anomaly::HeaderRowMoved {
                                from: c.header_row,
                                to: *row,
                            });
                            Some(*row)
                        }
                        [] => {
                            anomalies.push(Anomaly::NoContractHeader);
                            None
                        }
                        many => {
                            anomalies.push(Anomaly::HeaderRowAmbiguous {
                                rows: many.to_vec(),
                            });
                            None
                        }
                    }
                }
            }
            many => {
                anomalies.push(Anomaly::HeaderRowAmbiguous {
                    rows: many.to_vec(),
                });
                None
            }
        }
    };
    let Some(hdr) = hdr else {
        return finish(c, anomalies, vec![], Some(sheet_name), None, obs);
    };
    let row = obs.row_labels(hdr);

    // ---- column resolution (PoC Phase C + D + diagnose_missing unified) ----
    let mapped_labels: BTreeSet<&str> = c.mapped.iter().map(|m| m.header_label.as_str()).collect();
    let mut resolved_pos: BTreeMap<&str, u32> = BTreeMap::new(); // app_key -> position
    let mut consumed_replacements: BTreeSet<&str> = BTreeSet::new();
    for mc in &c.mapped {
        let hits: Vec<u32> = row
            .iter()
            .filter(|(_, l)| *l == mc.header_label)
            .map(|(col, _)| *col)
            .collect();
        match hits.as_slice() {
            [pos] => {
                resolved_pos.insert(mc.app_key.as_str(), *pos);
                if *pos != mc.excel_col {
                    anomalies.push(Anomaly::ColumnMoved {
                        app_key: mc.app_key.clone(),
                        label: mc.header_label.clone(),
                        from: mc.excel_col,
                        to: *pos,
                        app_owned: mc.app_owned,
                    });
                }
            }
            [] => {
                // A foreign label sitting exactly at the last confirmed position → rename candidate.
                let replacement = row
                    .iter()
                    .find(|(col, l)| *col == mc.excel_col && !mapped_labels.contains(l.as_str()))
                    .map(|(_, l)| l.as_str());
                if mc.app_owned {
                    anomalies.push(Anomaly::AppColumnMissing {
                        app_key: mc.app_key.clone(),
                        label: mc.header_label.clone(),
                        replacement: replacement.map(str::to_string),
                    });
                } else if let Some(new_label) = replacement {
                    consumed_replacements.insert(new_label);
                    anomalies.push(Anomaly::RenameCandidate {
                        app_key: mc.app_key.clone(),
                        old_label: mc.header_label.clone(),
                        new_label: new_label.to_string(),
                        col: mc.excel_col,
                    });
                } else if mc.required {
                    anomalies.push(Anomaly::RequiredColumnMissing {
                        app_key: mc.app_key.clone(),
                        label: mc.header_label.clone(),
                    });
                } else {
                    anomalies.push(Anomaly::OptionalColumnMissing {
                        app_key: mc.app_key.clone(),
                        label: mc.header_label.clone(),
                    });
                }
            }
            many => {
                anomalies.push(Anomaly::DuplicateHeader {
                    label: mc.header_label.clone(),
                    cols: many.to_vec(),
                });
            }
        }
    }

    // ---- Phase E: formulas in app-owned data areas (at the RESOLVED position) ----
    let eds = c.effective_data_start(hdr);
    let mut app_formula_cols: BTreeSet<u32> = BTreeSet::new();
    for mc in c.mapped.iter().filter(|m| m.app_owned) {
        let Some(pos) = resolved_pos.get(mc.app_key.as_str()) else {
            continue; // missing/duplicated — already Broken above
        };
        if let Some(first) = obs
            .formulas
            .iter()
            .filter(|(r, cc)| *r >= eds && cc == pos)
            .map(|(r, _)| *r)
            .min()
        {
            app_formula_cols.insert(*pos);
            anomalies.push(Anomaly::FormulaInAppColumn {
                app_key: mc.app_key.clone(),
                label: mc.header_label.clone(),
                col: *pos,
                first_row: first,
            });
        }
    }

    // ---- Phase F: unrecognised labels on the effective header row ----
    let resolved_positions: BTreeSet<u32> = resolved_pos.values().copied().collect();
    let skipped_by_col: BTreeMap<u32, &Option<String>> = c
        .skipped
        .iter()
        .map(|s| (s.excel_col, &s.header_label))
        .collect();
    let skipped_labels: BTreeSet<&str> = c
        .skipped
        .iter()
        .filter_map(|s| s.header_label.as_deref())
        .filter(|l| !l.trim().is_empty())
        .collect();
    let reserved: BTreeSet<&str> = c
        .mapped
        .iter()
        .filter(|m| m.app_owned)
        .map(|m| m.header_label.as_str())
        .collect();
    let mut new_columns: Vec<NewColumn> = Vec::new();
    let mut flagged_cols: BTreeSet<u32> = BTreeSet::new();
    for (col, label) in row {
        if resolved_positions.contains(col) {
            continue; // a mapped column's (possibly moved) home
        }
        if let Some(recorded) = skipped_by_col.get(col) {
            let same = recorded.as_deref().map(str::trim).unwrap_or("") == label.trim();
            if !same && !label.trim().is_empty() {
                anomalies.push(Anomaly::SkippedColumnRelabeled {
                    excel_col: *col,
                    old_label: (*recorded).clone(),
                    new_label: label.clone(),
                });
            }
            continue; // skipped position is always "known"
        }
        if consumed_replacements.contains(label.as_str()) || mapped_labels.contains(label.as_str())
        {
            continue; // consumed as a rename candidate / duplicate handled above
        }
        flagged_cols.insert(*col);
        if label.trim().is_empty() {
            anomalies.push(Anomaly::EmptyHeader { col: *col });
        } else if reserved.contains(label.as_str()) {
            anomalies.push(Anomaly::ReservedNameConflict {
                label: label.clone(),
                col: *col,
            });
        } else if skipped_labels.contains(label.as_str()) {
            anomalies.push(Anomaly::NewColumnMatchesSkipped {
                label: label.clone(),
                col: *col,
                skipped_col: c
                    .skipped
                    .iter()
                    .find(|s| s.header_label.as_deref() == Some(label.as_str()))
                    .map(|s| s.excel_col)
                    .unwrap_or(*col),
            });
        } else if new_columns.iter().any(|n| n.label == *label) {
            anomalies.push(Anomaly::DuplicateNewColumn {
                label: label.clone(),
                col: *col,
            });
        } else {
            new_columns.push(NewColumn {
                excel_col: *col,
                label: label.clone(),
            });
        }
    }

    // ---- Phase G: data without any usable header ----
    let known: BTreeSet<u32> = resolved_positions
        .iter()
        .copied()
        .chain(skipped_by_col.keys().copied())
        .chain(new_columns.iter().map(|n| n.excel_col))
        .chain(flagged_cols.iter().copied())
        .collect();
    for col in &obs.occupied_data_cols {
        if !known.contains(col) {
            anomalies.push(Anomaly::HeaderlessData { col: *col });
        }
    }

    finish(c, anomalies, new_columns, Some(sheet_name), Some(hdr), obs)
}

fn finish(
    c: &Contract,
    anomalies: Vec<Anomaly>,
    new_columns: Vec<NewColumn>,
    effective_sheet: Option<String>,
    effective_header_row: Option<u32>,
    obs: &SheetObs,
) -> VerifyOutcome {
    let app_formula_cols: BTreeSet<u32> = anomalies
        .iter()
        .filter_map(|a| match a {
            Anomaly::FormulaInAppColumn { col, .. } => Some(*col),
            _ => None,
        })
        .collect();
    let structure_fp = structure_fp(
        c,
        effective_sheet.as_deref(),
        effective_header_row,
        effective_header_row
            .map(|r| obs.row_labels(r))
            .unwrap_or(&[]),
        &obs.occupied_data_cols,
        &app_formula_cols,
    );
    VerifyOutcome {
        anomalies,
        new_columns,
        effective_sheet,
        effective_header_row,
        structure_fp,
    }
}

// ---- structure fingerprint (structure-v1, §4.7) -------------------------------------

/// Equivalence-class key over everything the verdict DEPENDS on — and nothing else:
/// contract (sheet/rows/every column's position+ownership+key+label+required) plus
/// the observation's projection (resolved sheet, effective header row and its
/// labels, occupied data columns, app columns carrying data-area formulas).
/// Row count / cell values / user-column formulas are deliberately EXCLUDED: row
/// edits are Safe and must not change the fp. Used for the status drift display and
/// the confirm freshness guard — never to skip the verdict itself.
fn structure_fp(
    c: &Contract,
    resolved_sheet: Option<&str>,
    effective_header_row: Option<u32>,
    header_labels: &[(u32, String)],
    occupied_data_cols: &BTreeSet<u32>,
    app_formula_cols: &BTreeSet<u32>,
) -> String {
    let mut buf = String::from("structure-v1\0");
    let push = |buf: &mut String, tag: &str, s: &str| {
        buf.push_str(tag);
        buf.push_str(&s.len().to_string());
        buf.push(':');
        buf.push_str(s);
        buf.push('\0');
    };
    // Contract side.
    push(&mut buf, "sheet=", &c.sheet_name);
    push(&mut buf, "hdr=", &c.header_row.to_string());
    push(&mut buf, "data=", &c.data_start_row.to_string());
    let mut cols: Vec<String> = c
        .mapped
        .iter()
        .map(|m| {
            // role/source_field/projection change what the contract MEANS (which
            // column feeds the fetch pipeline, what gets written back), so they are
            // part of the equivalence class.
            format!(
                "m,{},{},{},{},{},{},{},{}",
                m.excel_col,
                if m.app_owned { "app" } else { "user" },
                m.app_key,
                m.header_label,
                m.required,
                m.role.as_deref().unwrap_or(""),
                m.source_field.as_deref().unwrap_or(""),
                m.projection.map(|p| p.as_str()).unwrap_or("")
            )
        })
        .chain(c.skipped.iter().map(|s| {
            format!(
                "s,{},{}",
                s.excel_col,
                s.header_label.as_deref().unwrap_or("")
            )
        }))
        .collect();
    cols.sort();
    for col in &cols {
        push(&mut buf, "col=", col);
    }
    // Observation projection.
    push(&mut buf, "osheet=", resolved_sheet.unwrap_or(""));
    push(
        &mut buf,
        "ohdr=",
        &effective_header_row
            .map(|r| r.to_string())
            .unwrap_or_default(),
    );
    let mut labels: Vec<String> = header_labels
        .iter()
        .map(|(col, l)| format!("{col},{l}"))
        .collect();
    labels.sort();
    for l in &labels {
        push(&mut buf, "olabel=", l);
    }
    let occ: Vec<String> = occupied_data_cols.iter().map(u32::to_string).collect();
    push(&mut buf, "oocc=", &occ.join(","));
    let aff: Vec<String> = app_formula_cols.iter().map(u32::to_string).collect();
    push(&mut buf, "oaf=", &aff.join(","));

    let mut h = Sha256::new();
    h.update(buf.as_bytes());
    let fp: [u8; 32] = h.finalize().into();
    crate::excel_link::fingerprint::to_hex(&fp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LinkOwnership;

    // Key-based fixture with a NON-CONTIGUOUS mapping and a skipped column — the
    // configuration the PoC's index-based contract could not express:
    //   A:no B:型番(req) C:数量(req) D:EC単価(app) E:(skipped "メモ") F:小計 G:注文番号
    fn contract() -> Contract {
        Contract::try_from_store(&dummy_header(), &columns()).unwrap()
    }

    fn dummy_header() -> LinkHeader {
        LinkHeader {
            bom_id: "b1".into(),
            workbook_path: "x".into(),
            sheet_name: "BOM".into(),
            header_row: 1,
            data_start_row: 2,
            env_verdict: crate::model::EnvVerdict::Allow,
            env_resolved_path: None,
            env_fs_name: None,
            env_checked_at: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn col(
        excel_col: i64,
        label: Option<&str>,
        key: Option<&str>,
        own: LinkOwnership,
        required: bool,
    ) -> LinkColumn {
        LinkColumn {
            excel_col,
            header_label: label.map(str::to_string),
            app_key: key.map(str::to_string),
            ownership: own,
            required,
            role: None,
            source_field: None,
            projection: None,
        }
    }

    fn columns() -> Vec<LinkColumn> {
        use LinkOwnership::*;
        vec![
            col(0, Some("No"), Some("no"), User, false),
            col(1, Some("型番"), Some("partsNo"), User, true),
            col(2, Some("数量"), Some("qty"), User, true),
            col(3, Some("EC単価"), Some("ecUnitPrice"), App, false),
            col(4, Some("メモ"), None, Skipped, false),
            col(5, Some("小計"), Some("subtotal"), User, false),
            col(6, Some("注文番号"), Some("orderNo"), User, false),
        ]
    }

    fn base_obs() -> SheetObs {
        let mut obs = SheetObs::default();
        obs.labels.insert(
            1,
            vec![
                (0, "No".into()),
                (1, "型番".into()),
                (2, "数量".into()),
                (3, "EC単価".into()),
                (4, "メモ".into()),
                (5, "小計".into()),
                (6, "注文番号".into()),
            ],
        );
        // User-owned subtotal formulas on data rows — allowed (PoC test 13).
        obs.formulas = (2..=6).map(|r| (r, 5)).collect();
        obs.occupied_data_cols = (0..=6).collect();
        obs
    }

    fn sheets(obs: SheetObs) -> Vec<(String, SheetObs)> {
        vec![("BOM".into(), obs)]
    }

    fn run(obs: SheetObs) -> VerifyOutcome {
        verify_structure(&contract(), &sheets(obs))
    }

    fn codes(o: &VerifyOutcome) -> Vec<&'static str> {
        o.anomalies.iter().map(Anomaly::code).collect()
    }

    // ---- Safe ----

    #[test]
    fn pristine_non_contiguous_contract_is_safe() {
        // The PoC's index==position comparison would flag this layout as "moved";
        // key-based resolution accepts arbitrary positions (main goal of the port).
        let o = run(base_obs());
        assert_eq!(o.severity(), Severity::Safe, "{:?}", o.anomalies);
        assert!(o.new_columns.is_empty());
        // Skipped column E carries a label and data and stays invisible (Phase F/G).
    }

    #[test]
    fn clean_new_user_column_is_safe_with_position() {
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().push((7, "備考".into()));
        obs.occupied_data_cols.insert(7);
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Safe, "{:?}", o.anomalies);
        assert_eq!(
            o.new_columns,
            vec![NewColumn {
                excel_col: 7,
                label: "備考".into()
            }]
        );
    }

    #[test]
    fn user_formula_columns_stay_safe() {
        // base_obs already has subtotal formulas on every data row (PoC test 13).
        assert_eq!(run(base_obs()).severity(), Severity::Safe);
    }

    #[test]
    fn gap_row_formula_in_app_column_is_safe() {
        // data_start_row=3 leaves row 2 as a note row: an app-column formula there
        // is NOT app-writable area → not Broken (data_start_row basis, §4.9).
        let header = LinkHeader {
            data_start_row: 3,
            ..dummy_header()
        };
        let c = Contract::try_from_store(&header, &columns()).unwrap();
        let mut obs = base_obs();
        obs.formulas.push((2, 3)); // row 2 (gap), app column D
        let o = verify_structure(&c, &sheets(obs));
        assert_eq!(o.severity(), Severity::Safe, "{:?}", o.anomalies);
    }

    // ---- Confirm ----

    #[test]
    fn moved_column_confirms_even_when_unique() {
        // Rule 8: unique resolution must still not auto-adopt.
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((7, "数量".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_COLUMN_MOVED"));
        // Candidate carries the new position for the confirm flow.
        match o.to_verdict(&contract()) {
            StructureVerdict::Confirm { candidates, .. } => assert!(candidates.contains(
                &LinkResolutionCandidate::AdoptColumnMove {
                    app_key: "qty".into(),
                    new_excel_col: 7
                }
            )),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn renamed_user_header_confirms_not_safe() {
        // Rule 8: a rename is NEVER auto-adopted even though the key identifies the
        // column uniquely via its last confirmed position.
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((2, "数".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_RENAME"));
    }

    #[test]
    fn renamed_sheet_confirms_and_checks_continue() {
        let o = verify_structure(&contract(), &[("BOM2".into(), base_obs())]);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_SHEET_RENAMED"));
    }

    #[test]
    fn moved_header_row_confirms_with_data_start_candidate() {
        let mut obs = base_obs();
        let hdr = obs.labels.remove(&1).unwrap();
        obs.labels.insert(1, vec![(0, "メモ: 部品表".into())]);
        obs.labels.insert(2, hdr);
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm, "{:?}", o.anomalies);
        assert!(codes(&o).contains(&"C_HEADER_ROW_MOVED"));
        match o.to_verdict(&contract()) {
            StructureVerdict::Confirm { candidates, .. } => assert!(candidates.contains(
                &LinkResolutionCandidate::AdoptHeaderRowMove {
                    new_header_row: 2,
                    new_data_start_row: 3
                }
            )),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn headerless_data_column_confirms() {
        let mut obs = base_obs();
        obs.occupied_data_cols.insert(8); // data, no label anywhere
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_HEADERLESS_DATA"));
    }

    #[test]
    fn skipped_column_relabel_confirms_with_two_choices() {
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 4);
        row.push((4, "原価".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_SKIPPED_RELABELED"));
        match o.to_verdict(&contract()) {
            StructureVerdict::Confirm { candidates, .. } => {
                assert!(candidates.iter().any(|c| matches!(
                    c,
                    LinkResolutionCandidate::ImportSkippedAsUser { excel_col: 4, .. }
                )));
                assert!(candidates.iter().any(|c| matches!(
                    c,
                    LinkResolutionCandidate::KeepSkipped { excel_col: 4, .. }
                )));
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn skipped_label_at_new_position_confirms() {
        // The skipped column's recorded label shows up elsewhere: might be the
        // skipped column moved — never auto-import over the user's exclusion.
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().push((8, "メモ".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_NEW_MATCHES_SKIPPED"));
    }

    #[test]
    fn new_column_conflicts_confirm() {
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().push((8, "".into()));
        obs.labels.get_mut(&1).unwrap().push((9, "備考".into()));
        obs.labels.get_mut(&1).unwrap().push((10, "備考".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_EMPTY_HEADER"));
        assert!(codes(&o).contains(&"C_DUP_NEW_COLUMN"));
        assert_eq!(o.new_columns.len(), 1); // first 備考 is clean until confirmed
    }

    #[test]
    fn reserved_name_conflict_confirms() {
        // A NEW column reusing an app-owned label at a different position while the
        // app column itself is intact at its own position → duplicate → Broken.
        // The pure reserved-name path needs the app label ABSENT from its home
        // first, which is Broken anyway — so exercise via a skipped-position-free
        // new label equal to a reserved name with the app column present: dup wins.
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().push((8, "EC単価".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_DUP_HEADER"));
    }

    #[test]
    fn optional_column_missing_confirms() {
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().retain(|(c, _)| *c != 6); // 注文番号 gone
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm);
        assert!(codes(&o).contains(&"C_OPTIONAL_COL_MISSING"));
    }

    // ---- Broken ----

    #[test]
    fn deleted_required_column_is_broken() {
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().retain(|(c, _)| *c != 1); // 型番 gone
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_REQUIRED_COL_MISSING"));
    }

    #[test]
    fn duplicate_header_is_broken() {
        let mut obs = base_obs();
        obs.labels.get_mut(&1).unwrap().push((8, "型番".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_DUP_HEADER"));
    }

    #[test]
    fn deleted_sheet_is_broken() {
        let o = verify_structure(&contract(), &[("Other".into(), SheetObs::default())]);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_SHEET_MISSING"));
    }

    #[test]
    fn ambiguous_sheets_are_broken() {
        let o = verify_structure(
            &contract(),
            &[("S1".into(), base_obs()), ("S2".into(), base_obs())],
        );
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_SHEET_AMBIGUOUS"));
    }

    #[test]
    fn renamed_app_column_is_broken() {
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 3);
        row.push((3, "単価".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_APP_COL_MISSING"));
    }

    #[test]
    fn formula_in_app_column_is_broken() {
        let mut obs = base_obs();
        obs.formulas.push((2, 3)); // data row, app column D
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_FORMULA_IN_APP_COL"));
    }

    #[test]
    fn ambiguous_header_rows_are_broken() {
        let mut obs = base_obs();
        let hdr = obs.labels.get(&1).unwrap().clone();
        obs.labels.remove(&1);
        obs.labels.insert(3, hdr.clone());
        obs.labels.insert(5, hdr);
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_HEADER_ROW_AMBIGUOUS"));
    }

    #[test]
    fn all_headers_gone_is_broken() {
        let mut obs = base_obs();
        obs.labels.remove(&1);
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_NO_CONTRACT_HEADER"));
    }

    // ---- combos: Broken must never hide behind Confirm (PoC tests 15-19) ----

    #[test]
    fn moved_header_row_with_app_formula_is_broken() {
        let mut obs = base_obs();
        let hdr = obs.labels.remove(&1).unwrap();
        obs.labels.insert(2, hdr);
        obs.formulas.push((3, 3)); // below the MOVED header: app column formula
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_FORMULA_IN_APP_COL"));
        assert!(codes(&o).contains(&"C_HEADER_ROW_MOVED")); // both collected
    }

    #[test]
    fn user_rename_with_app_formula_is_broken() {
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((2, "数".into()));
        obs.formulas.push((2, 3));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_FORMULA_IN_APP_COL"));
        assert!(codes(&o).contains(&"C_RENAME"));
    }

    #[test]
    fn user_rename_with_duplicate_is_broken() {
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((2, "数".into()));
        row.push((8, "型番".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_DUP_HEADER"));
    }

    #[test]
    fn user_rename_with_app_deletion_is_broken() {
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2 && *c != 3);
        row.push((2, "数".into()));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_APP_COL_MISSING"));
    }

    #[test]
    fn skipped_relabel_with_app_formula_is_broken() {
        // Key-based combo: the skipped-column Confirm must not mask the Broken.
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 4);
        row.push((4, "原価".into()));
        obs.formulas.push((2, 3));
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Broken);
        assert!(codes(&o).contains(&"E_FORMULA_IN_APP_COL"));
        assert!(codes(&o).contains(&"C_SKIPPED_RELABELED"));
    }

    // ---- tolerant constellation: intentional changes vs the PoC ----

    #[test]
    fn sheet_rename_plus_user_rename_is_confirm_not_broken() {
        // PoC: all-labels matching broke on one rename → Broken("deleted").
        // Tolerant scoring finds the sheet, then diagnoses the rename (never Safe).
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((2, "数".into()));
        let o = verify_structure(&contract(), &[("BOM2".into(), obs)]);
        assert_eq!(o.severity(), Severity::Confirm, "{:?}", o.anomalies);
        assert!(codes(&o).contains(&"C_SHEET_RENAMED"));
        assert!(codes(&o).contains(&"C_RENAME"));
    }

    #[test]
    fn header_move_plus_user_rename_is_confirm_not_broken() {
        // PoC: NoContractHeader Broken. Tolerant scoring finds the moved row and
        // diagnoses the rename (never Safe).
        let mut obs = base_obs();
        let mut hdr = obs.labels.remove(&1).unwrap();
        hdr.retain(|(c, _)| *c != 2);
        hdr.push((2, "数".into()));
        obs.labels.insert(4, hdr);
        let o = run(obs);
        assert_eq!(o.severity(), Severity::Confirm, "{:?}", o.anomalies);
        assert!(codes(&o).contains(&"C_HEADER_ROW_MOVED"));
        assert!(codes(&o).contains(&"C_RENAME"));
    }

    // ---- contract invariants ----

    #[test]
    fn contract_invariants_fail_closed() {
        let header = dummy_header();
        // mapped column without a label
        let mut cols = columns();
        cols[1].header_label = None;
        assert!(matches!(
            Contract::try_from_store(&header, &cols),
            Err(ContractError::MappedColumnWithoutLabel { .. })
        ));
        // duplicate mapped label
        let mut cols = columns();
        cols[5].header_label = Some("型番".into());
        assert!(matches!(
            Contract::try_from_store(&header, &cols),
            Err(ContractError::DuplicateMappedLabel { .. })
        ));
        // duplicate position
        let mut cols = columns();
        cols[5].excel_col = 0;
        assert!(matches!(
            Contract::try_from_store(&header, &cols),
            Err(ContractError::DuplicatePosition { .. })
        ));
        // header/data row order
        let bad = LinkHeader {
            data_start_row: 1,
            ..dummy_header()
        };
        assert!(matches!(
            Contract::try_from_store(&bad, &columns()),
            Err(ContractError::InvalidRows)
        ));
    }

    // ---- structure_fp inclusion rules ----

    #[test]
    fn structure_fp_ignores_row_edits_but_tracks_structure() {
        let base = run(base_obs()).structure_fp;
        // Row edits (more user formulas on data rows) must not change the fp as
        // long as the projection (labels/occupied cols/app-formula cols) holds.
        let mut obs = base_obs();
        obs.formulas.push((30, 5));
        assert_eq!(run(obs).structure_fp, base);
        // A rename changes it.
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 2);
        row.push((2, "数".into()));
        assert_ne!(run(obs).structure_fp, base);
        // An app-column data formula changes it.
        let mut obs = base_obs();
        obs.formulas.push((2, 3));
        assert_ne!(run(obs).structure_fp, base);
        // A skipped-column relabel changes it.
        let mut obs = base_obs();
        let row = obs.labels.get_mut(&1).unwrap();
        row.retain(|(c, _)| *c != 4);
        row.push((4, "原価".into()));
        assert_ne!(run(obs).structure_fp, base);
        // A contract difference changes it (same observation).
        let header = LinkHeader {
            data_start_row: 3,
            ..dummy_header()
        };
        let c2 = Contract::try_from_store(&header, &columns()).unwrap();
        assert_ne!(
            verify_structure(&c2, &sheets(base_obs())).structure_fp,
            base
        );
    }

    #[test]
    fn structure_fp_tracks_role_and_projection_changes() {
        // role/source_field/projection change what the contract MEANS (review R1):
        // the fp must react so stale Confirm candidates cannot apply across them.
        let base = run(base_obs()).structure_fp;
        type Change = Box<dyn Fn(&mut LinkColumn)>;
        let variants: Vec<Change> = vec![
            Box::new(|c| c.role = Some("partNo".into())),
            Box::new(|c| c.source_field = Some("product.name".into())),
        ];
        for change in variants {
            let mut cols = columns();
            change(&mut cols[1]); // 型番 (user column: source_field alone is a valid shape? use role)
            if cols[1].source_field.is_some() {
                cols[1].projection = Some(crate::model::LinkProjection::Suggest);
            }
            let c = Contract::try_from_store(&dummy_header(), &cols).unwrap();
            assert_ne!(verify_structure(&c, &sheets(base_obs())).structure_fp, base);
        }
    }
}
