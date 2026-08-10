// Persistence layer for Excel link mode (schema V5 in db.rs; design in
// docs/plans/0018-excel-link-mode/implementation.md §1). Conventions follow db.rs:
// take &Connection (or &mut for multi-statement transactions, like save_bom), return
// rusqlite::Result, stamp timestamps with datetime('now','localtime') on the SQL side.
//
// Record structs here are DB-facing; IPC-facing view types arrive with the link
// commands (PR-3) in model.rs. The API surface is limited to what implementation.md
// names for PR-3〜5 (reader/writeback/pending/backup) — no speculative queries.

use crate::model::{
    BackupLocation, CalcState, EnvVerdict, LinkOwnership, LinkProjection, SupplierQuote, SyncStatus,
};
use rusqlite::{params, Connection, OptionalExtension};

/// Map an unexpected enum string coming out of the DB onto a rusqlite error.
/// Reachable only if the DB was written by a newer schema or edited externally —
/// the CHECK constraints and the typed API keep normal writes in range.
fn bad_enum(col: &'static str, val: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("unexpected {col} value in DB: {val}").into(),
    )
}

// ---- link contract (bom_link + bom_link_column) -------------------------------------

/// Input for creating a link (timestamps and the initial state row are set here).
pub struct NewLink {
    pub bom_id: String,
    pub workbook_path: String,
    pub sheet_name: String,
    pub header_row: i64,
    pub data_start_row: i64,
    pub env_verdict: EnvVerdict,
    pub env_resolved_path: Option<String>,
    pub env_fs_name: Option<String>,
    pub columns: Vec<LinkColumn>,
}

/// One contract column (`bom_link_column` row, minus bom_id).
#[derive(Clone, PartialEq, Debug)]
pub struct LinkColumn {
    pub excel_col: i64,
    pub header_label: Option<String>,
    pub app_key: Option<String>,
    pub ownership: LinkOwnership,
    pub required: bool,
    pub role: Option<String>,
    pub source_field: Option<String>,
    pub projection: Option<LinkProjection>,
}

/// `bom_link` row as stored.
#[derive(Clone)]
pub struct LinkHeader {
    pub bom_id: String,
    pub workbook_path: String,
    pub sheet_name: String,
    pub header_row: i64,
    pub data_start_row: i64,
    pub env_verdict: EnvVerdict,
    pub env_resolved_path: Option<String>,
    pub env_fs_name: Option<String>,
    pub env_checked_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Full link record: contract header + columns + volatile state.
pub struct LinkRecord {
    pub header: LinkHeader,
    pub columns: Vec<LinkColumn>,
    pub state: LinkState,
}

/// Create a link: contract header + columns + the initial state row (defaults:
/// linked/unverified/generation 0). Takes a TRANSACTION by type: the multi-statement
/// write must never run half-committed (excel_link::create_link commits it together
/// with the BOM row, the display cache and the state update). The caller has already
/// run the §4.10 environment check (its verdict is part of the contract input).
pub fn create_link(tx: &rusqlite::Transaction, link: &NewLink) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO bom_link(bom_id, workbook_path, sheet_name, header_row, data_start_row, \
           env_verdict, env_resolved_path, env_fs_name, env_checked_at, created_at, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now', 'localtime'), \
           datetime('now', 'localtime'), datetime('now', 'localtime'))",
        params![
            link.bom_id,
            link.workbook_path,
            link.sheet_name,
            link.header_row,
            link.data_start_row,
            link.env_verdict.as_str(),
            link.env_resolved_path,
            link.env_fs_name,
        ],
    )?;
    for c in &link.columns {
        insert_column(tx, &link.bom_id, c)?;
    }
    tx.execute(
        "INSERT INTO bom_link_state(bom_id) VALUES(?1)",
        [&link.bom_id],
    )?;
    Ok(())
}

fn insert_column(conn: &Connection, bom_id: &str, c: &LinkColumn) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bom_link_column(bom_id, excel_col, header_label, app_key, ownership, \
           required, role, source_field, projection) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            bom_id,
            c.excel_col,
            c.header_label,
            c.app_key,
            c.ownership.as_str(),
            c.required as i64,
            c.role,
            c.source_field,
            c.projection.map(|p| p.as_str()),
        ],
    )?;
    Ok(())
}

/// Load the full link record, or None if the BOM is not linked.
pub fn get_link(conn: &Connection, bom_id: &str) -> rusqlite::Result<Option<LinkRecord>> {
    let header = conn
        .query_row(
            "SELECT bom_id, workbook_path, sheet_name, header_row, data_start_row, env_verdict, \
               env_resolved_path, env_fs_name, env_checked_at, created_at, updated_at \
             FROM bom_link WHERE bom_id = ?1",
            [bom_id],
            |r| {
                let verdict: String = r.get(5)?;
                Ok(LinkHeader {
                    bom_id: r.get(0)?,
                    workbook_path: r.get(1)?,
                    sheet_name: r.get(2)?,
                    header_row: r.get(3)?,
                    data_start_row: r.get(4)?,
                    env_verdict: EnvVerdict::from_db(&verdict)
                        .ok_or_else(|| bad_enum("env_verdict", &verdict))?,
                    env_resolved_path: r.get(6)?,
                    env_fs_name: r.get(7)?,
                    env_checked_at: r.get(8)?,
                    created_at: r.get(9)?,
                    updated_at: r.get(10)?,
                })
            },
        )
        .optional()?;
    let Some(header) = header else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT excel_col, header_label, app_key, ownership, required, role, source_field, \
           projection \
         FROM bom_link_column WHERE bom_id = ?1 ORDER BY excel_col",
    )?;
    let columns = stmt
        .query_map([bom_id], |r| {
            let ownership: String = r.get(3)?;
            let projection: Option<String> = r.get(7)?;
            Ok(LinkColumn {
                excel_col: r.get(0)?,
                header_label: r.get(1)?,
                app_key: r.get(2)?,
                ownership: LinkOwnership::from_db(&ownership)
                    .ok_or_else(|| bad_enum("ownership", &ownership))?,
                required: r.get::<_, i64>(4)? != 0,
                role: r.get(5)?,
                source_field: r.get(6)?,
                projection: match projection {
                    None => None,
                    Some(p) => Some(
                        LinkProjection::from_db(&p).ok_or_else(|| bad_enum("projection", &p))?,
                    ),
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // The state row is created with the link (create_link) and cascade-deleted with it,
    // so its absence is an integrity error — let QueryReturnedNoRows surface it.
    let state = get_state_required(conn, bom_id)?;
    Ok(Some(LinkRecord {
        header,
        columns,
        state,
    }))
}

/// All linked workbook identities: (bom_id, workbook_path, env_resolved_path).
/// Used by the 1-workbook=1-link guard (a shared workbook would let one BOM's
/// write-back read as the other's "Excel recalculated" and fabricate Trusted).
pub fn list_link_paths(
    conn: &Connection,
) -> rusqlite::Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare("SELECT bom_id, workbook_path, env_resolved_path FROM bom_link")?;
    let it = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    it.collect()
}

/// Unlink: delete the `bom_link` row; contract columns, state, quotes and pending are
/// removed by FK cascade. The backup ledger survives on purpose (origin_bom_id).
/// The snapshot side of implementation.md §1.3 (freezing bom_column/bom_row) is the
/// caller's job (PR-3 command) — this is only the DB part.
pub fn delete_link(conn: &Connection, bom_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM bom_link WHERE bom_id = ?1", [bom_id])?;
    Ok(())
}

/// Replace the contract columns (remapping / confirm resolution) without touching the
/// volatile state row (design principle 2 in implementation.md §1.1). Also bumps
/// bom_link.updated_at (the contract changed). Takes a TRANSACTION by type: the
/// DELETE-then-INSERT sequence must never be observable half-done (a plain
/// Connection caller could otherwise commit an empty/partial contract on a
/// mid-list constraint violation — review R2).
pub fn replace_columns(
    tx: &rusqlite::Transaction,
    bom_id: &str,
    columns: &[LinkColumn],
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM bom_link_column WHERE bom_id = ?1", [bom_id])?;
    for c in columns {
        insert_column(tx, bom_id, c)?;
    }
    tx.execute(
        "UPDATE bom_link SET updated_at = datetime('now', 'localtime') WHERE bom_id = ?1",
        [bom_id],
    )?;
    Ok(())
}

// ---- volatile state (bom_link_state) ------------------------------------------------
/// Update the contract HEADER coordinates (sheet name / header row / data start).
/// Only the confirm flow may call this (AdoptSheetRename / AdoptHeaderRowMove) and
/// it always travels with a replace_columns in the SAME transaction — hence the
/// Transaction-typed parameter.
pub fn update_link_header(
    tx: &rusqlite::Transaction,
    bom_id: &str,
    sheet_name: &str,
    header_row: i64,
    data_start_row: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE bom_link SET sheet_name = ?2, header_row = ?3, data_start_row = ?4,          updated_at = datetime('now', 'localtime') WHERE bom_id = ?1",
        params![bom_id, sheet_name, header_row, data_start_row],
    )?;
    Ok(())
}

/// `bom_link_state` row (minus bom_id).
#[derive(Clone, PartialEq, Debug)]
pub struct LinkState {
    pub sync_status: SyncStatus,
    pub sync_error: Option<String>,
    pub calc_state: CalcState,
    pub recalc_requested: bool,
    pub fp_algo: String,
    pub last_read_fp: Option<String>,
    pub last_read_at: Option<String>,
    pub last_app_write_fp: Option<String>,
    pub last_app_write_at: Option<String>,
    pub last_verified_read_fp: Option<String>,
    pub structure_fp: Option<String>,
    pub structure_fp_algo: String,
    pub data_first_row: Option<i64>,
    pub data_last_row: Option<i64>,
    pub row_count: Option<i64>,
    pub ec_generation: i64,
    pub applied_generation: i64,
}

fn get_state_required(conn: &Connection, bom_id: &str) -> rusqlite::Result<LinkState> {
    conn.query_row(
        "SELECT sync_status, sync_error, calc_state, recalc_requested, fp_algo, last_read_fp, \
           last_read_at, last_app_write_fp, last_app_write_at, last_verified_read_fp, \
           structure_fp, structure_fp_algo, data_first_row, data_last_row, row_count, \
           ec_generation, applied_generation \
         FROM bom_link_state WHERE bom_id = ?1",
        [bom_id],
        |r| {
            let sync: String = r.get(0)?;
            let calc: String = r.get(2)?;
            Ok(LinkState {
                sync_status: SyncStatus::from_db(&sync)
                    .ok_or_else(|| bad_enum("sync_status", &sync))?,
                sync_error: r.get(1)?,
                calc_state: CalcState::from_db(&calc)
                    .ok_or_else(|| bad_enum("calc_state", &calc))?,
                recalc_requested: r.get::<_, i64>(3)? != 0,
                fp_algo: r.get(4)?,
                last_read_fp: r.get(5)?,
                last_read_at: r.get(6)?,
                last_app_write_fp: r.get(7)?,
                last_app_write_at: r.get(8)?,
                last_verified_read_fp: r.get(9)?,
                structure_fp: r.get(10)?,
                structure_fp_algo: r.get(11)?,
                data_first_row: r.get(12)?,
                data_last_row: r.get(13)?,
                row_count: r.get(14)?,
                ec_generation: r.get(15)?,
                applied_generation: r.get(16)?,
            })
        },
    )
}

/// Read the volatile state, or None if the BOM is not linked.
pub fn get_state(conn: &Connection, bom_id: &str) -> rusqlite::Result<Option<LinkState>> {
    match get_state_required(conn, bom_id) {
        Ok(s) => Ok(Some(s)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Full-row update of the volatile state (single-writer via the DbState mutex, so a
/// read-modify-write roundtrip through LinkState is race-free).
pub fn update_state(conn: &Connection, bom_id: &str, s: &LinkState) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_link_state SET sync_status = ?2, sync_error = ?3, calc_state = ?4, \
           recalc_requested = ?5, fp_algo = ?6, last_read_fp = ?7, last_read_at = ?8, \
           last_app_write_fp = ?9, last_app_write_at = ?10, last_verified_read_fp = ?11, \
           structure_fp = ?12, structure_fp_algo = ?13, data_first_row = ?14, \
           data_last_row = ?15, row_count = ?16, ec_generation = ?17, applied_generation = ?18 \
         WHERE bom_id = ?1",
        params![
            bom_id,
            s.sync_status.as_str(),
            s.sync_error,
            s.calc_state.as_str(),
            s.recalc_requested as i64,
            s.fp_algo,
            s.last_read_fp,
            s.last_read_at,
            s.last_app_write_fp,
            s.last_app_write_at,
            s.last_verified_read_fp,
            s.structure_fp,
            s.structure_fp_algo,
            s.data_first_row,
            s.data_last_row,
            s.row_count,
            s.ec_generation,
            s.applied_generation,
        ],
    )?;
    Ok(())
}

// ---- adopted quote snapshot (bom_link_quote) ----------------------------------------

/// Outcome of the latest fetch attempt for one (supplier, parts_no) of a linked BOM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttemptStatus {
    Ok,
    Error,
}

impl AttemptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
        }
    }
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// `bom_link_quote` row (minus bom_id). payload_json == None means no successful
/// adoption yet (a failure-only row); the paired CHECKs keep payload/generation/
/// fetched_at consistent.
#[derive(Clone, PartialEq, Debug)]
pub struct QuoteRecord {
    pub supplier_code: String,
    pub parts_no: String,
    pub payload_json: Option<String>,
    pub currency: Option<String>,
    pub fetched_at: Option<String>,
    pub generation: Option<i64>,
    pub last_attempt_at: String,
    pub last_attempt_status: AttemptStatus,
    pub last_error_code: Option<String>,
    pub last_error_message: Option<String>,
}

/// Map invalid adoption input onto a rusqlite error before anything reaches the DB.
fn bad_input(msg: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(msg.into())
}

/// Record a successful adoption: payload/generation move forward, failure state clears.
/// The snapshot is the linked BOM's system of record, so only a self-consistent quote
/// is accepted (fail closed — nothing is written on rejection):
/// - `status` must be "ok" with no errors: the fetch pipeline stamps fetched_at on
///   error quotes too, and a JSON column cannot enforce this — this function is the
///   last line of defense against payload status="error" under last_attempt_status='ok'
///   (which would clear the §6.2.1 warning and adopt a failure as a generation)
/// - `fetched_at` is REQUIRED: the DB column and the payload's own fetchedAt must
///   agree, and an unknown fetch time must not masquerade as "now" (adopting a cache
///   hit keeps its ORIGINAL fetch time; `last_attempt_at` is what records "now")
/// - serialization failures propagate instead of storing an unrestorable placeholder
/// - the row's supplier code is derived from the quote itself (no second input to
///   disagree with the payload); currency likewise (the cache_put pattern in db.rs)
pub fn record_quote_ok(
    conn: &Connection,
    bom_id: &str,
    parts_no: &str,
    quote: &SupplierQuote,
    generation: i64,
) -> rusqlite::Result<()> {
    if quote.status != "ok" || !quote.errors.is_empty() {
        return Err(bad_input(format!(
            "adoption of {parts_no} rejected: quote status is '{}' with {} error(s) — \
             only a successful quote may become the adopted snapshot (use \
             record_quote_error for failures)",
            quote.status,
            quote.errors.len()
        )));
    }
    let Some(fetched) = quote.fetched_at.clone() else {
        return Err(bad_input(format!(
            "adoption of {parts_no} rejected: quote has no fetched_at (unknown fetch time \
             must not be recorded as the adoption time)"
        )));
    };
    let payload = serde_json::to_string(quote)
        .map_err(|e| bad_input(format!("adoption of {parts_no} rejected: {e}")))?;
    let currency = quote.quote.as_ref().and_then(|p| p.currency.clone());
    conn.execute(
        "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, payload_json, currency, \
           fetched_at, generation, last_attempt_at, last_attempt_status, last_error_code, \
           last_error_message) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, \
           datetime('now', 'localtime'), 'ok', NULL, NULL) \
         ON CONFLICT(bom_id, supplier_code, parts_no) DO UPDATE SET \
           payload_json = excluded.payload_json, currency = excluded.currency, \
           fetched_at = excluded.fetched_at, generation = excluded.generation, \
           last_attempt_at = excluded.last_attempt_at, last_attempt_status = 'ok', \
           last_error_code = NULL, last_error_message = NULL",
        params![
            bom_id,
            quote.supplier_code,
            parts_no,
            payload,
            currency,
            fetched,
            generation
        ],
    )?;
    Ok(())
}

/// Record a failed attempt: only the attempt metadata changes — an existing adopted
/// payload/generation is deliberately left in place (§6.2.1 decision: keep the last
/// value and warn). A first-ever failure creates a payload-less row so the warning
/// survives restart / re-open (implementation.md §2.3).
pub fn record_quote_error(
    conn: &Connection,
    bom_id: &str,
    supplier_code: &str,
    parts_no: &str,
    error_code: &str,
    error_message: Option<&str>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, payload_json, currency, \
           fetched_at, generation, last_attempt_at, last_attempt_status, last_error_code, \
           last_error_message) \
         VALUES(?1, ?2, ?3, NULL, NULL, NULL, NULL, datetime('now', 'localtime'), 'error', ?4, ?5) \
         ON CONFLICT(bom_id, supplier_code, parts_no) DO UPDATE SET \
           last_attempt_at = excluded.last_attempt_at, last_attempt_status = 'error', \
           last_error_code = excluded.last_error_code, \
           last_error_message = excluded.last_error_message",
        params![bom_id, supplier_code, parts_no, error_code, error_message],
    )?;
    Ok(())
}

/// All snapshot rows of a linked BOM (stable order for display and tests).
pub fn list_quotes(conn: &Connection, bom_id: &str) -> rusqlite::Result<Vec<QuoteRecord>> {
    let mut stmt = conn.prepare(
        "SELECT supplier_code, parts_no, payload_json, currency, fetched_at, generation, \
           last_attempt_at, last_attempt_status, last_error_code, last_error_message \
         FROM bom_link_quote WHERE bom_id = ?1 ORDER BY supplier_code, parts_no",
    )?;
    let it = stmt.query_map([bom_id], |r| {
        let status: String = r.get(7)?;
        Ok(QuoteRecord {
            supplier_code: r.get(0)?,
            parts_no: r.get(1)?,
            payload_json: r.get(2)?,
            currency: r.get(3)?,
            fetched_at: r.get(4)?,
            generation: r.get(5)?,
            last_attempt_at: r.get(6)?,
            last_attempt_status: AttemptStatus::from_db(&status)
                .ok_or_else(|| bad_enum("last_attempt_status", &status))?,
            last_error_code: r.get(8)?,
            last_error_message: r.get(9)?,
        })
    })?;
    it.collect()
}

// ---- pending reflection (bom_link_pending, §4.2.1 latest-wins) ----------------------

/// `bom_link_pending` row (minus bom_id).
#[derive(Clone, PartialEq, Debug)]
pub struct PendingRecord {
    pub requested_generation: i64,
    pub requested_at: String,
    pub last_attempt_at: Option<String>,
    pub attempt_count: i64,
    pub blocked_reason: Option<String>,
}

/// Latest-wins upsert with monotonicity enforced on the DB side: only a STRICTLY newer
/// generation replaces the stored request (async completions can reach the DB lock out
/// of order — a late generation-2 arriving after generation-3 must not regress the
/// pending row). The retry bookkeeping resets only on that real advance, because it
/// describes the superseded request; an equal-or-older upsert is a no-op, and a manual
/// retry goes through the apply command + record_pending_attempt, never through here.
pub fn upsert_pending(conn: &Connection, bom_id: &str, generation: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bom_link_pending(bom_id, requested_generation, requested_at) \
         VALUES(?1, ?2, datetime('now', 'localtime')) \
         ON CONFLICT(bom_id) DO UPDATE SET \
           requested_generation = excluded.requested_generation, \
           requested_at = excluded.requested_at, \
           last_attempt_at = NULL, attempt_count = 0, blocked_reason = NULL \
         WHERE excluded.requested_generation > bom_link_pending.requested_generation",
        params![bom_id, generation],
    )?;
    Ok(())
}

pub fn get_pending(conn: &Connection, bom_id: &str) -> rusqlite::Result<Option<PendingRecord>> {
    conn.query_row(
        "SELECT requested_generation, requested_at, last_attempt_at, attempt_count, \
           blocked_reason \
         FROM bom_link_pending WHERE bom_id = ?1",
        [bom_id],
        |r| {
            Ok(PendingRecord {
                requested_generation: r.get(0)?,
                requested_at: r.get(1)?,
                last_attempt_at: r.get(2)?,
                attempt_count: r.get(3)?,
                blocked_reason: r.get(4)?,
            })
        },
    )
    .optional()
}

/// Record one failed reflection attempt (file open, permission, env, ...).
pub fn record_pending_attempt(
    conn: &Connection,
    bom_id: &str,
    blocked_reason: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_link_pending SET last_attempt_at = datetime('now', 'localtime'), \
           attempt_count = attempt_count + 1, blocked_reason = ?2 \
         WHERE bom_id = ?1",
        params![bom_id, blocked_reason],
    )?;
    Ok(())
}

pub fn clear_pending(conn: &Connection, bom_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM bom_link_pending WHERE bom_id = ?1", [bom_id])?;
    Ok(())
}

// ---- backup ledger (bom_link_backup, §4.2.2) ----------------------------------------

/// Input for a new ledger row, written right after a successful ReplaceFileW.
/// No `location` / `is_conflict` fields on purpose: a fresh backup always starts
/// beside the workbook (volume_temp; app_data is reachable only via mark_transferred),
/// and conflict is a fact derived from the fingerprints — both are set by insert_backup
/// so a caller mistake cannot strip a conflict backup of its deletion protection.
pub struct NewBackup {
    pub origin_bom_id: String,
    pub workbook_path: String,
    pub backup_path: String,
    pub backup_fp: String,
    pub f0_fp: String,
}

/// Prepared write journal (V6): everything the reconcile needs to finish an
/// interrupted §4.2.2 replace. Persisted BEFORE ReplaceFileW; deleted inside the
/// post-replace finalize transaction. One row per BOM (a link runs one write at
/// a time), so upsert semantics are safe.
pub struct WriteJournal {
    pub backup_path: String,
    pub temp_path: String,
    pub f0_fp: String,
    pub new_fp: String,
    pub generation: i64,
}

/// INSERT only - by design there is no upsert: an existing row is the sole
/// recovery record of an unfinished replace, and overwriting it would lose the
/// only pointer to an unmanaged backup (2nd review #2). The caller must have
/// verified the journal is empty (apply fails closed otherwise); the PRIMARY KEY
/// enforces it against every other path.
pub fn insert_write_journal(
    conn: &Connection,
    bom_id: &str,
    j: &WriteJournal,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bom_link_write_journal \n           (bom_id, backup_path, temp_path, f0_fp, new_fp, generation, created_at) \n         VALUES(?1, ?2, ?3, ?4, ?5, ?6, datetime('now', 'localtime'))",
        params![
            bom_id,
            j.backup_path,
            j.temp_path,
            j.f0_fp,
            j.new_fp,
            j.generation
        ],
    )?;
    Ok(())
}

pub fn get_write_journal(
    conn: &Connection,
    bom_id: &str,
) -> rusqlite::Result<Option<WriteJournal>> {
    conn.query_row(
        "SELECT backup_path, temp_path, f0_fp, new_fp, generation          FROM bom_link_write_journal WHERE bom_id = ?1",
        [bom_id],
        |r| {
            Ok(WriteJournal {
                backup_path: r.get(0)?,
                temp_path: r.get(1)?,
                f0_fp: r.get(2)?,
                new_fp: r.get(3)?,
                generation: r.get(4)?,
            })
        },
    )
    .optional()
}

pub fn delete_write_journal(conn: &Connection, bom_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM bom_link_write_journal WHERE bom_id = ?1",
        [bom_id],
    )?;
    Ok(())
}

/// `bom_link_backup` row as stored.
pub struct BackupRecord {
    pub id: i64,
    pub bom_id: Option<String>,
    pub origin_bom_id: String,
    pub workbook_path: String,
    pub backup_path: String,
    pub location: BackupLocation,
    pub fp_algo: String,
    pub backup_fp: String,
    pub f0_fp: String,
    pub is_conflict: bool,
    pub created_at: String,
    pub transferred_at: Option<String>,
    pub resolved_at: Option<String>,
    pub deleted_at: Option<String>,
}

/// Insert a ledger row (bom_id = origin_bom_id while the BOM is alive) and return its
/// id. Location starts at volume_temp and conflict is derived (see NewBackup); the V5
/// CHECKs enforce both invariants against any other write path as well.
pub fn insert_backup(conn: &Connection, b: &NewBackup) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO bom_link_backup(bom_id, origin_bom_id, workbook_path, backup_path, \
           location, backup_fp, f0_fp, is_conflict, created_at) \
         VALUES(?1, ?1, ?2, ?3, 'volume_temp', ?4, ?5, ?4 <> ?5, datetime('now', 'localtime'))",
        params![
            b.origin_bom_id,
            b.workbook_path,
            b.backup_path,
            b.backup_fp,
            b.f0_fp,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn map_backup(r: &rusqlite::Row<'_>) -> rusqlite::Result<BackupRecord> {
    let location: String = r.get(5)?;
    Ok(BackupRecord {
        id: r.get(0)?,
        bom_id: r.get(1)?,
        origin_bom_id: r.get(2)?,
        workbook_path: r.get(3)?,
        backup_path: r.get(4)?,
        location: BackupLocation::from_db(&location)
            .ok_or_else(|| bad_enum("location", &location))?,
        fp_algo: r.get(6)?,
        backup_fp: r.get(7)?,
        f0_fp: r.get(8)?,
        is_conflict: r.get::<_, i64>(9)? != 0,
        created_at: r.get(10)?,
        transferred_at: r.get(11)?,
        resolved_at: r.get(12)?,
        deleted_at: r.get(13)?,
    })
}

const BACKUP_COLS: &str = "id, bom_id, origin_bom_id, workbook_path, backup_path, location, \
   fp_algo, backup_fp, f0_fp, is_conflict, created_at, transferred_at, resolved_at, deleted_at";

/// Ledger rows for one origin BOM, newest first (includes deleted rows — the ledger is
/// an audit trail; filter on deleted_at where needed).
pub fn list_backups(conn: &Connection, origin_bom_id: &str) -> rusqlite::Result<Vec<BackupRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BACKUP_COLS} FROM bom_link_backup \
         WHERE origin_bom_id = ?1 ORDER BY created_at DESC, id DESC"
    ))?;
    let it = stmt.query_map([origin_bom_id], map_backup)?;
    it.collect()
}

/// All unresolved conflict backups (the ones auto-deletion must never touch and the UI
/// must surface). Uses the partial index idx_bom_link_backup_open_conflict.
pub fn unresolved_conflicts(conn: &Connection) -> rusqlite::Result<Vec<BackupRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BACKUP_COLS} FROM bom_link_backup \
         WHERE is_conflict = 1 AND resolved_at IS NULL AND deleted_at IS NULL ORDER BY created_at DESC, id DESC"
    ))?;
    let it = stmt.query_map([], map_backup)?;
    it.collect()
}

/// Unresolved conflicts of ONE BOM, oldest first — the status/resolve surface.
/// Same predicate as has_unresolved_conflict (deleted rows excluded).
pub fn unresolved_conflicts_for(
    conn: &Connection,
    origin_bom_id: &str,
) -> rusqlite::Result<Vec<BackupRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BACKUP_COLS} FROM bom_link_backup          WHERE origin_bom_id = ?1 AND is_conflict = 1 AND resolved_at IS NULL            AND deleted_at IS NULL ORDER BY id"
    ))?;
    let it = stmt.query_map([origin_bom_id], map_backup)?;
    it.collect()
}

/// One ledger row by id (resolve_conflict validates ownership/state against it).
pub fn get_backup(conn: &Connection, id: i64) -> rusqlite::Result<Option<BackupRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BACKUP_COLS} FROM bom_link_backup WHERE id = ?1"
    ))?;
    let mut it = stmt.query_map([id], map_backup)?;
    it.next().transpose()
}

/// Step 6 of the transfer: the file now lives in app data under `new_path`.
/// §1.3 conflict gate: does this BOM have a conflict backup the user has not
/// resolved yet? While true, open keeps sync_status=conflict and apply refuses.
pub fn has_unresolved_conflict(conn: &Connection, origin_bom_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bom_link_backup \
          WHERE origin_bom_id = ?1 AND is_conflict = 1 AND resolved_at IS NULL \
            AND deleted_at IS NULL)",
        [origin_bom_id],
        |r| r.get(0),
    )
}

/// Ledger rows whose backup file still sits beside the workbook (volume_temp) —
/// the §4.2.2 transfer sweep re-targets ALL of them, not only the row the
/// current apply created (2nd review #3: recovered backups need a way out too).
pub fn untransferred_backups(
    conn: &Connection,
    origin_bom_id: &str,
) -> rusqlite::Result<Vec<BackupRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {BACKUP_COLS} FROM bom_link_backup \
         WHERE origin_bom_id = ?1 AND location = 'volume_temp' AND deleted_at IS NULL \
         ORDER BY id"
    ))?;
    let it = stmt.query_map([origin_bom_id], map_backup)?;
    it.collect()
}

pub fn mark_transferred(conn: &Connection, id: i64, new_path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_link_backup SET backup_path = ?2, location = 'app_data', \
           transferred_at = datetime('now', 'localtime') \
         WHERE id = ?1",
        params![id, new_path],
    )?;
    Ok(())
}

/// The user resolved this conflict (the file becomes eligible for retention deletion).
pub fn mark_resolved(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_link_backup SET resolved_at = datetime('now', 'localtime') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// The backup FILE was deleted by the retention policy; the ledger row stays (audit).
/// Call only after the filesystem delete actually succeeded (implementation.md §0).
pub fn mark_deleted(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE bom_link_backup SET deleted_at = datetime('now', 'localtime') WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Retention policy (implementation.md §0): delete only rows that are BOTH ranked
/// beyond the newest 5 (per origin BOM, among still-existing files) AND older than
/// 30 days — and never an unresolved conflict or a file still mid-transfer
/// (volume_temp). Returns candidate ids; the caller deletes files and then calls
/// mark_deleted per success.
pub fn retention_candidates(conn: &Connection, origin_bom_id: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "WITH ranked AS ( \
           SELECT id, created_at, location, is_conflict, resolved_at, \
                  ROW_NUMBER() OVER (ORDER BY created_at DESC, id DESC) AS rn \
           FROM bom_link_backup WHERE origin_bom_id = ?1 AND deleted_at IS NULL \
         ) \
         SELECT id FROM ranked \
         WHERE rn > 5 AND created_at < datetime('now', 'localtime', '-30 days') \
           AND NOT (is_conflict = 1 AND resolved_at IS NULL) \
           AND location = 'app_data' \
         ORDER BY id",
    )?;
    let it = stmt.query_map([origin_bom_id], |r| r.get(0))?;
    it.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::run_migrations;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        run_migrations(&conn).unwrap();
        conn
    }

    /// bom_link references bom(id): every linked test BOM needs a bom row first.
    fn seed_bom(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO bom(id, name, created_at, updated_at) \
             VALUES(?1, 'test', datetime('now'), datetime('now'))",
            [id],
        )
        .unwrap();
    }

    fn sample_columns() -> Vec<LinkColumn> {
        vec![
            LinkColumn {
                excel_col: 0,
                header_label: Some("型番".into()),
                app_key: Some("partsNo".into()),
                ownership: LinkOwnership::User,
                required: true,
                role: Some("partNo".into()),
                source_field: None,
                projection: None,
            },
            LinkColumn {
                excel_col: 1,
                header_label: Some("品名".into()),
                app_key: Some("partsName".into()),
                ownership: LinkOwnership::User,
                required: false,
                role: None,
                source_field: Some("product.name".into()),
                projection: Some(LinkProjection::Suggest),
            },
            LinkColumn {
                excel_col: 2,
                header_label: Some("EC単価".into()),
                app_key: Some("ecUnitPrice".into()),
                ownership: LinkOwnership::App,
                required: false,
                role: None,
                source_field: Some("quote.unitPrice".into()),
                projection: Some(LinkProjection::Writeback),
            },
            LinkColumn {
                excel_col: 3,
                header_label: None,
                app_key: None,
                ownership: LinkOwnership::Skipped,
                required: false,
                role: None,
                source_field: None,
                projection: None,
            },
        ]
    }

    fn sample_link(bom_id: &str) -> NewLink {
        NewLink {
            bom_id: bom_id.into(),
            workbook_path: "C:\\work\\bom.xlsx".into(),
            sheet_name: "BOM".into(),
            header_row: 1,
            data_start_row: 2,
            env_verdict: EnvVerdict::Allow,
            env_resolved_path: Some("C:\\work\\bom.xlsx".into()),
            env_fs_name: Some("NTFS".into()),
            columns: sample_columns(),
        }
    }

    fn linked(conn: &mut Connection, bom_id: &str) {
        seed_bom(conn, bom_id);
        let tx = conn.transaction().unwrap();
        create_link(&tx, &sample_link(bom_id)).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn create_get_delete_link_roundtrip() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        let rec = get_link(&conn, "b1").unwrap().unwrap();
        assert_eq!(rec.header.sheet_name, "BOM");
        assert_eq!(rec.header.env_verdict, EnvVerdict::Allow);
        assert_eq!(rec.columns, sample_columns());
        // Initial state row comes from the DDL defaults.
        assert_eq!(rec.state.sync_status, SyncStatus::Linked);
        assert_eq!(rec.state.calc_state, CalcState::Unverified);
        assert!(!rec.state.recalc_requested);
        assert_eq!(rec.state.fp_algo, "sha256-v1");
        assert_eq!(rec.state.ec_generation, 0);
        assert_eq!(rec.state.applied_generation, 0);

        assert!(get_link(&conn, "nope").unwrap().is_none());

        // Unlink: contract + state + quote + pending disappear via cascade.
        record_quote_error(&conn, "b1", "MISUMI", "TEST-PART-001", "network", None).unwrap();
        upsert_pending(&conn, "b1", 1).unwrap();
        delete_link(&conn, "b1").unwrap();
        assert!(get_link(&conn, "b1").unwrap().is_none());
        for table in [
            "bom_link_column",
            "bom_link_state",
            "bom_link_quote",
            "bom_link_pending",
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} not cascaded");
        }
    }

    #[test]
    fn replace_columns_keeps_state() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // Advance the volatile state, then remap the contract.
        let mut st = get_state(&conn, "b1").unwrap().unwrap();
        st.ec_generation = 7;
        st.calc_state = CalcState::Stale;
        update_state(&conn, "b1", &st).unwrap();

        let remapped = vec![LinkColumn {
            excel_col: 5,
            header_label: Some("型番(新)".into()),
            app_key: Some("partsNo".into()),
            ownership: LinkOwnership::User,
            required: true,
            role: Some("partNo".into()),
            source_field: None,
            projection: None,
        }];
        let tx = conn.transaction().unwrap();
        replace_columns(&tx, "b1", &remapped).unwrap();
        tx.commit().unwrap();

        let rec = get_link(&conn, "b1").unwrap().unwrap();
        assert_eq!(rec.columns, remapped);
        // Design principle 2 (implementation.md §1.1): state survives remapping.
        assert_eq!(rec.state.ec_generation, 7);
        assert_eq!(rec.state.calc_state, CalcState::Stale);
    }

    #[test]
    fn replace_columns_rolls_back_atomically_on_mid_list_failure() {
        // The tx-typed API (review R2): a constraint violation in the middle of the
        // DELETE-then-INSERT sequence must leave header/columns/state untouched
        // when the transaction is dropped (rolled back).
        let mut conn = mem();
        linked(&mut conn, "b1");
        let before = get_link(&conn, "b1").unwrap().unwrap();

        let dup_pos = vec![
            LinkColumn {
                excel_col: 5,
                header_label: Some("A".into()),
                app_key: Some("a".into()),
                ownership: LinkOwnership::User,
                required: false,
                role: None,
                source_field: None,
                projection: None,
            },
            LinkColumn {
                excel_col: 5, // PK violation on the second insert
                header_label: Some("B".into()),
                app_key: Some("b".into()),
                ownership: LinkOwnership::User,
                required: false,
                role: None,
                source_field: None,
                projection: None,
            },
        ];
        {
            let tx = conn.transaction().unwrap();
            assert!(replace_columns(&tx, "b1", &dup_pos).is_err());
            // tx dropped here -> rollback
        }
        let after = get_link(&conn, "b1").unwrap().unwrap();
        assert_eq!(after.columns, before.columns);
        assert_eq!(after.state, before.state);
        assert_eq!(after.header.updated_at, before.header.updated_at);
    }

    // -- bom_link_column CHECK combinations (DoD: enumerated CHECK must reject every
    //    NULL-mixed shape — SQLite CHECKs pass on NULL, hence the whitelist form) --

    fn raw_col(conn: &Connection, sql_tail: &str) -> rusqlite::Result<usize> {
        conn.execute(
            &format!("INSERT INTO bom_link_column(bom_id, {sql_tail}"),
            [],
        )
    }

    #[test]
    fn link_column_check_accepts_the_four_valid_shapes() {
        let mut conn = mem();
        linked(&mut conn, "b1"); // sample_columns() already covers: user-plain (required),
                                 // user+suggest, app+writeback, skipped.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM bom_link_column", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn link_column_check_rejects_invalid_shapes() {
        let mut conn = mem();
        linked(&mut conn, "b1");
        let cases: &[(&str, &str)] = &[
            (
                "app without source_field",
                "excel_col, app_key, ownership, projection) VALUES('b1', 10, 'k10', 'app', 'writeback')",
            ),
            (
                "app with suggest projection",
                "excel_col, app_key, ownership, source_field, projection) VALUES('b1', 11, 'k11', 'app', 'quote.unitPrice', 'suggest')",
            ),
            (
                "app without projection",
                "excel_col, app_key, ownership, source_field) VALUES('b1', 12, 'k12', 'app', 'quote.unitPrice')",
            ),
            (
                "app without app_key",
                "excel_col, ownership, source_field, projection) VALUES('b1', 13, 'app', 'quote.unitPrice', 'writeback')",
            ),
            (
                "user with source_field but no projection",
                "excel_col, app_key, ownership, source_field) VALUES('b1', 14, 'k14', 'user', 'product.name')",
            ),
            (
                "user with writeback projection",
                "excel_col, app_key, ownership, source_field, projection) VALUES('b1', 15, 'k15', 'user', 'product.name', 'writeback')",
            ),
            (
                "user without app_key",
                "excel_col, ownership) VALUES('b1', 16, 'user')",
            ),
            (
                "skipped with app_key",
                "excel_col, app_key, ownership) VALUES('b1', 17, 'k17', 'skipped')",
            ),
            (
                "skipped with required=1",
                "excel_col, ownership, required) VALUES('b1', 18, 'skipped', 1)",
            ),
            (
                "unknown ownership value",
                "excel_col, app_key, ownership) VALUES('b1', 19, 'k19', 'owner')",
            ),
            (
                "unknown projection value",
                "excel_col, app_key, ownership, source_field, projection) VALUES('b1', 20, 'k20', 'user', 'product.name', 'overwrite')",
            ),
        ];
        for (name, tail) in cases {
            assert!(raw_col(&conn, tail).is_err(), "accepted: {name}");
        }
    }

    #[test]
    fn link_column_app_key_unique_but_skipped_nulls_allowed() {
        let mut conn = mem();
        linked(&mut conn, "b1");
        // Duplicate app_key within the BOM → rejected by the partial unique index.
        assert!(raw_col(
            &conn,
            "excel_col, app_key, ownership) VALUES('b1', 30, 'partsNo', 'user')",
        )
        .is_err());
        // A second skipped column (app_key NULL) is fine — NULLs are outside the index.
        raw_col(&conn, "excel_col, ownership) VALUES('b1', 31, 'skipped')").unwrap();
        // Same app_key on a DIFFERENT linked BOM is fine (index is per bom_id).
        seed_bom(&conn, "b2");
        let tx = conn.transaction().unwrap();
        create_link(&tx, &sample_link("b2")).unwrap();
        tx.commit().unwrap();
    }

    // -- bom_link_quote: paired CHECKs + §6.2.1 keep-and-warn semantics --

    #[test]
    fn quote_checks_reject_inconsistent_rows() {
        let mut conn = mem();
        linked(&mut conn, "b1");
        let cases: &[(&str, &str)] = &[
            (
                "payload without generation",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, payload_json, fetched_at, last_attempt_at, last_attempt_status) \
                 VALUES('b1','MISUMI','P1','{}', datetime('now'), datetime('now'), 'ok')",
            ),
            (
                "generation without payload",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, generation, last_attempt_at, last_attempt_status) \
                 VALUES('b1','MISUMI','P2', 1, datetime('now'), 'ok')",
            ),
            (
                "payload without fetched_at",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, payload_json, generation, last_attempt_at, last_attempt_status) \
                 VALUES('b1','MISUMI','P3','{}', 1, datetime('now'), 'ok')",
            ),
            (
                "error without error code",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, last_attempt_at, last_attempt_status) \
                 VALUES('b1','MISUMI','P4', datetime('now'), 'error')",
            ),
            (
                "ok with leftover error code",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, payload_json, fetched_at, generation, last_attempt_at, last_attempt_status, last_error_code) \
                 VALUES('b1','MISUMI','P5','{}', datetime('now'), 1, datetime('now'), 'ok', 'network')",
            ),
            (
                "unknown attempt status",
                "INSERT INTO bom_link_quote(bom_id, supplier_code, parts_no, last_attempt_at, last_attempt_status) \
                 VALUES('b1','MISUMI','P6', datetime('now'), 'pending')",
            ),
        ];
        for (name, sql) in cases {
            assert!(conn.execute(sql, []).is_err(), "accepted: {name}");
        }
    }

    /// A quote the way the fetch pipeline produces it (fetched_at set at network time).
    fn mk_quote(price: &str, fetched_at: &str) -> SupplierQuote {
        SupplierQuote {
            supplier_code: "MISUMI".into(),
            status: "ok".into(),
            product: None,
            quote: Some(crate::model::SupplierPricing {
                currency: Some("JPY".into()),
                unit_price: Some(price.into()),
                ..Default::default()
            }),
            errors: vec![],
            warnings: vec![],
            fetched_at: Some(fetched_at.into()),
            raw: None,
        }
    }

    #[test]
    fn quote_error_preserves_adopted_payload() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // First-ever failure → payload-less row persists the warning.
        record_quote_error(
            &conn,
            "b1",
            "MISUMI",
            "TEST-PART-001",
            "network",
            Some("timeout"),
        )
        .unwrap();
        let q = &list_quotes(&conn, "b1").unwrap()[0];
        assert_eq!(q.last_attempt_status, AttemptStatus::Error);
        assert!(q.payload_json.is_none());
        assert!(q.generation.is_none());

        // Successful adoption clears the failure state.
        record_quote_ok(
            &conn,
            "b1",
            "TEST-PART-001",
            &mk_quote("115", "2026-07-05 09:10:00"),
            1,
        )
        .unwrap();
        let q = &list_quotes(&conn, "b1").unwrap()[0];
        assert_eq!(q.last_attempt_status, AttemptStatus::Ok);
        assert!(q.payload_json.as_deref().unwrap().contains("\"115\""));
        assert_eq!(q.generation, Some(1));
        assert_eq!(q.currency.as_deref(), Some("JPY"));
        assert!(q.last_error_code.is_none());

        // A later failure keeps the adopted payload/generation (§6.2.1: keep and warn).
        record_quote_error(&conn, "b1", "MISUMI", "TEST-PART-001", "http_500", None).unwrap();
        let q = &list_quotes(&conn, "b1").unwrap()[0];
        assert_eq!(q.last_attempt_status, AttemptStatus::Error);
        assert_eq!(q.last_error_code.as_deref(), Some("http_500"));
        assert!(q.payload_json.as_deref().unwrap().contains("\"115\""));
        assert_eq!(q.generation, Some(1));
    }

    #[test]
    fn quote_adoption_keeps_original_fetch_time() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // Adopting a cache hit later must keep the ORIGINAL fetch time: the DB column
        // and the payload's own fetchedAt agree, and "now" only lands in last_attempt_at.
        let old = "2026-07-05 09:10:00";
        record_quote_ok(&conn, "b1", "TEST-PART-001", &mk_quote("115", old), 1).unwrap();
        let q = &list_quotes(&conn, "b1").unwrap()[0];
        assert_eq!(q.fetched_at.as_deref(), Some(old));
        assert!(q.payload_json.as_deref().unwrap().contains(old));
        assert_ne!(q.last_attempt_at, old);
        // The row's supplier code comes from the quote itself — no second input that
        // could disagree with the payload.
        assert_eq!(q.supplier_code, "MISUMI");
        assert!(q.payload_json.as_deref().unwrap().contains("\"MISUMI\""));
    }

    #[test]
    fn quote_adoption_without_fetch_time_is_rejected() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // fetched_at=None must fail closed: recording "now" would present the adoption
        // time as a fetch time, and DB column vs payload would disagree again.
        let mut q = mk_quote("115", "unused");
        q.fetched_at = None;
        assert!(record_quote_ok(&conn, "b1", "TEST-PART-001", &q, 1).is_err());
        // Nothing was written — not even a failure row (this is caller input error,
        // not a fetch attempt).
        assert!(list_quotes(&conn, "b1").unwrap().is_empty());
    }

    #[test]
    fn quote_adoption_of_non_ok_quote_is_rejected() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // The fetch pipeline stamps fetched_at on error quotes too — such a quote must
        // never become the adopted snapshot (payload status="error" would sit under
        // last_attempt_status='ok', clearing the failure warning and adopting a
        // failure as a generation).
        let mut q = mk_quote("115", "2026-07-05 09:10:00");
        q.status = "error".into();
        q.errors = vec!["型番が見つかりません".into()];
        assert!(record_quote_ok(&conn, "b1", "TEST-PART-001", &q, 1).is_err());
        assert!(list_quotes(&conn, "b1").unwrap().is_empty());

        // status="ok" but with leftover errors violates the normalization contract
        // (ok ⇔ errors empty) — also rejected.
        let mut q = mk_quote("115", "2026-07-05 09:10:00");
        q.errors = vec!["stale error".into()];
        assert!(record_quote_ok(&conn, "b1", "TEST-PART-001", &q, 1).is_err());
        assert!(list_quotes(&conn, "b1").unwrap().is_empty());

        // Other non-ok statuses (pending/idle/unknown) fail closed the same way.
        for status in ["pending", "idle", "???"] {
            let mut q = mk_quote("115", "2026-07-05 09:10:00");
            q.status = status.into();
            assert!(
                record_quote_ok(&conn, "b1", "TEST-PART-001", &q, 1).is_err(),
                "accepted status: {status}"
            );
        }
        assert!(list_quotes(&conn, "b1").unwrap().is_empty());
    }

    // -- pending: latest-wins --

    #[test]
    fn pending_latest_wins_and_attempts() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        upsert_pending(&conn, "b1", 1).unwrap();
        record_pending_attempt(&conn, "b1", "file_open").unwrap();
        let p = get_pending(&conn, "b1").unwrap().unwrap();
        assert_eq!(p.requested_generation, 1);
        assert_eq!(p.attempt_count, 1);
        assert_eq!(p.blocked_reason.as_deref(), Some("file_open"));

        // Newer request replaces the old one and restarts the retry bookkeeping.
        upsert_pending(&conn, "b1", 3).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM bom_link_pending", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let p = get_pending(&conn, "b1").unwrap().unwrap();
        assert_eq!(p.requested_generation, 3);
        assert_eq!(p.attempt_count, 0);
        assert!(p.blocked_reason.is_none());

        // Monotonicity: a late generation-2 completion (async completions can reach the
        // DB lock out of order) must NOT regress the pending row — and must not reset
        // the retry bookkeeping either.
        record_pending_attempt(&conn, "b1", "file_open").unwrap();
        upsert_pending(&conn, "b1", 2).unwrap();
        let p = get_pending(&conn, "b1").unwrap().unwrap();
        assert_eq!(p.requested_generation, 3);
        assert_eq!(p.attempt_count, 1);
        assert_eq!(p.blocked_reason.as_deref(), Some("file_open"));

        // Equal generation is a no-op too (reset happens only on a real advance).
        upsert_pending(&conn, "b1", 3).unwrap();
        let p = get_pending(&conn, "b1").unwrap().unwrap();
        assert_eq!(p.attempt_count, 1);

        clear_pending(&conn, "b1").unwrap();
        assert!(get_pending(&conn, "b1").unwrap().is_none());
    }

    // -- backup ledger --

    /// Fresh ledger row the way writeback will create it (always volume_temp at first;
    /// conflict derives from the fingerprints).
    fn seed_backup(conn: &Connection, bom_id: &str, conflict: bool) -> i64 {
        insert_backup(
            conn,
            &NewBackup {
                origin_bom_id: bom_id.into(),
                workbook_path: "C:\\work\\bom.xlsx".into(),
                backup_path: format!("C:\\work\\bom.backup.{}.xlsx", conn.last_insert_rowid()),
                backup_fp: if conflict { "ff" } else { "aa" }.into(),
                f0_fp: "aa".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn backup_ledger_survives_bom_delete_with_origin_retained() {
        let mut conn = mem();
        linked(&mut conn, "b1");
        let id = seed_backup(&conn, "b1", true);

        // A fresh backup starts beside the workbook and untransferred (API + CHECK).
        let b = &list_backups(&conn, "b1").unwrap()[0];
        assert_eq!(b.location, BackupLocation::VolumeTemp);
        assert!(b.transferred_at.is_none());

        // Transfer bookkeeping (the only path to app_data).
        mark_transferred(&conn, id, "C:\\appdata\\backups\\b1-1.xlsx").unwrap();
        let b = &list_backups(&conn, "b1").unwrap()[0];
        assert_eq!(b.location, BackupLocation::AppData);
        assert!(b.transferred_at.is_some());

        // Unresolved conflict is surfaced; resolving clears it.
        assert_eq!(unresolved_conflicts(&conn).unwrap().len(), 1);
        mark_resolved(&conn, id).unwrap();
        assert!(unresolved_conflicts(&conn).unwrap().is_empty());

        // Deleting the BOM nulls bom_id (SET NULL) but keeps the immutable origin.
        crate::db::delete_bom(&conn, "b1").unwrap();
        let b = &list_backups(&conn, "b1").unwrap()[0];
        assert!(b.bom_id.is_none());
        assert_eq!(b.origin_bom_id, "b1");

        // The bom_id/origin mismatch guard: an alive-FK row must match its origin.
        seed_bom(&conn, "b2");
        assert!(conn
            .execute(
                "INSERT INTO bom_link_backup(bom_id, origin_bom_id, workbook_path, backup_path, \
                   location, backup_fp, f0_fp, created_at) \
                 VALUES('b2', 'b1', 'w', 'p', 'volume_temp', 'aa', 'aa', datetime('now'))",
                [],
            )
            .is_err());
    }

    #[test]
    fn backup_conflict_is_derived_and_cannot_be_understated() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // Differing fingerprints → conflict, regardless of what a caller might intend.
        let id = seed_backup(&conn, "b1", true);
        let b = &list_backups(&conn, "b1").unwrap()[0];
        assert!(b.is_conflict);
        assert_eq!(unresolved_conflicts(&conn).unwrap().len(), 1);

        // The V5 CHECK rejects any write path that understates the derived fact
        // (is_conflict=0 with differing fingerprints would strip the deletion
        // protection from the only copy of the displaced external version).
        assert!(conn
            .execute(
                "INSERT INTO bom_link_backup(bom_id, origin_bom_id, workbook_path, backup_path, \
                   location, backup_fp, f0_fp, is_conflict, created_at) \
                 VALUES('b1', 'b1', 'w', 'p2', 'volume_temp', 'ff', 'aa', 0, datetime('now'))",
                [],
            )
            .is_err());
        // Location/transfer integrity: app_data without transferred_at is rejected.
        assert!(conn
            .execute(
                "INSERT INTO bom_link_backup(bom_id, origin_bom_id, workbook_path, backup_path, \
                   location, backup_fp, f0_fp, created_at) \
                 VALUES('b1', 'b1', 'w', 'p3', 'app_data', 'aa', 'aa', datetime('now'))",
                [],
            )
            .is_err());

        // An unresolved conflict never becomes a retention candidate, even old + rank>5.
        mark_transferred(&conn, id, "C:\\appdata\\backups\\b1-c.xlsx").unwrap();
        conn.execute(
            "UPDATE bom_link_backup SET created_at = datetime('now', 'localtime', '-90 days') \
             WHERE id = ?1",
            [id],
        )
        .unwrap();
        for _ in 0..6 {
            let nid = seed_backup(&conn, "b1", false);
            mark_transferred(&conn, nid, "C:\\appdata\\backups\\x.xlsx").unwrap();
        }
        assert_eq!(
            retention_candidates(&conn, "b1").unwrap(),
            Vec::<i64>::new()
        );
    }

    #[test]
    fn retention_candidates_apply_rank_age_and_exclusions() {
        let mut conn = mem();
        linked(&mut conn, "b1");

        // 7 backups, ALL older than 30 days (34-40 days): the age test passes for every
        // row, so what protects ids[2..7] is purely the newest-five rank — and what
        // exposes ids[0] (rank 7) / ids[1] (rank 6) is rank+age minus the exclusions:
        //   ids[0] = unresolved conflict (derived from differing fps at seed time),
        //   ids[1] = never transferred (stays volume_temp — transfer incomplete).
        let mut ids = Vec::new();
        for i in 0..7 {
            let id = seed_backup(&conn, "b1", i == 0);
            if i != 1 {
                mark_transferred(&conn, id, &format!("C:\\appdata\\backups\\b1-{i}.xlsx")).unwrap();
            }
            ids.push(id);
        }
        // Spread created_at: ids[0] oldest (-40d) ... ids[6] newest (-34d).
        for (i, id) in ids.iter().enumerate() {
            conn.execute(
                "UPDATE bom_link_backup SET created_at = datetime('now', 'localtime', ?2) \
                 WHERE id = ?1",
                params![id, format!("-{} days", 40 - i as i64)],
            )
            .unwrap();
        }
        // ids[2..7] (ranks 1-5): old, but within the newest five → kept by rank alone.
        assert_eq!(
            retention_candidates(&conn, "b1").unwrap(),
            Vec::<i64>::new()
        );

        // Resolve ids[0]'s conflict → it becomes deletable (rank 7 + old + resolved).
        mark_resolved(&conn, ids[0]).unwrap();
        assert_eq!(retention_candidates(&conn, "b1").unwrap(), vec![ids[0]]);

        // Once deleted, it leaves the ranking base (deleted_at IS NULL) — ids[1] moves
        // up to rank 6 but stays excluded as volume_temp.
        mark_deleted(&conn, ids[0]).unwrap();
        assert_eq!(
            retention_candidates(&conn, "b1").unwrap(),
            Vec::<i64>::new()
        );
    }
}
