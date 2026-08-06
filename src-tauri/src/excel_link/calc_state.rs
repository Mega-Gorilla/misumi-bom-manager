// Calc-state machine for Excel link mode (plan.md §4.4.1 / §4.4.2).
//
// Ported from tools/excel-link-poc/src/state.rs (verified in step 2 of the technical
// verification; tests carried over verbatim). Pure logic — no Excel, no I/O. The
// formula cache staleness problem is the reason link mode needs this at all: a zip
// edit changes a value cell but cannot recompute the formulas that depend on it, so
// those formulas' cached <v> go stale until Excel reopens and recalculates.
//
// Difference from the PoC: `CalcState` itself lives in model.rs (schema V5 / IPC
// vocabulary, PR-1) — this module adds the behavior on top instead of redefining it.

use crate::excel_link::fingerprint::{parse_hex, Fingerprint};
use crate::excel_link::store::LinkState;
use crate::model::CalcState;

impl CalcState {
    /// Whether the cached formula values may drive business logic (EC lookup, qty,
    /// subtotal, MOQ, cart, totals). Unverified is allowed but must be flagged in the
    /// UI (§4.4).
    pub fn is_usable(self) -> bool {
        matches!(self, CalcState::Unverified | CalcState::Trusted)
    }
}

/// What the structure contract (§4.7) persists between sessions about a linked
/// workbook. `None` for `last_app_write` means the app has never written this file.
#[derive(Debug, Clone, Copy)]
pub struct Persisted {
    /// Fingerprint captured immediately after the app's last write. §4.4.2.
    pub last_app_write: Option<Fingerprint>,
    /// Whether that write set fullCalcOnLoad (i.e. we asked Excel to recompute on open).
    pub recalc_requested: bool,
    /// False when a formula cell's cached value could not be read (missing `<v>`).
    /// Overrides everything else: an unreadable value is never usable (§4.4).
    pub value_readable: bool,
}

/// Build `Persisted` from the V5 state row (implementation.md §2.2: the supply side
/// of the restore rule). `value_readable` comes from the reader (missing `<v>` is a
/// property of the file just read, not of the DB). A corrupt stored fingerprint is an
/// error, not a "never wrote" — treating it as None would silently drop Stale.
pub fn persisted_from_state(s: &LinkState, value_readable: bool) -> Result<Persisted, String> {
    let last_app_write = match s.last_app_write_fp.as_deref() {
        None => None,
        Some(hex) => Some(parse_hex(hex).ok_or_else(|| {
            format!("bom_link_state.last_app_write_fp is not a valid fingerprint: {hex:?}")
        })?),
    };
    Ok(Persisted {
        last_app_write,
        recalc_requested: s.recalc_requested,
        value_readable,
    })
}

/// Restore the calc state on open/reload from the current file fingerprint plus what
/// we persisted (§4.4.2 table). This is what keeps `Stale` sticky across an app
/// restart: a plain re-read must NOT silently return to `Unverified`, or the safety
/// guarantee is lost.
///
/// | current vs last-app-write | recalc_requested | result     |
/// |---------------------------|------------------|------------|
/// | equal (nobody saved)      | (any)            | Stale      |
/// | differs                   | true             | Trusted    |
/// | differs                   | false            | Unverified |
/// | no app write recorded     | (any)            | Unverified |
///
/// Residual risk (documented, accepted): "fingerprint changed" is not proof that
/// *Excel* recalculated — a third-party tool could have saved without recomputing.
pub fn restore(current: &Fingerprint, p: &Persisted) -> CalcState {
    if !p.value_readable {
        return CalcState::Missing;
    }
    match p.last_app_write {
        None => CalcState::Unverified,
        Some(after_write) => {
            if *current == after_write {
                // The file is byte-identical to what we wrote: nobody (not even Excel)
                // has saved since. The stale caches are still stale.
                CalcState::Stale
            } else if p.recalc_requested {
                // The file changed and we had asked Excel to recalc-on-load: treat as
                // recomputed.
                CalcState::Trusted
            } else {
                // Changed, but we never requested a recalc — cannot claim trusted.
                CalcState::Unverified
            }
        }
    }
}

// ---- array / spill range intersection (§4.4.2, guards §4.2.2 step 4) ----------------

/// A1-style cell reference → (row, col), both 0-based. Returns None on anything we
/// cannot parse safely (absolute `$`, whole-column `A:A`, ranges) so the caller can
/// fail closed.
pub fn parse_cell(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    if s.is_empty() || s.contains('$') || s.contains(':') {
        return None;
    }
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (col, row) = s.split_at(split);
    if col.is_empty() || !col.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let col_num = col.bytes().try_fold(0u32, |acc, b| {
        acc.checked_mul(26)?.checked_add((b - b'A' + 1) as u32)
    })?;
    let row_num: u32 = row.parse().ok()?;
    if row_num == 0 {
        return None;
    }
    Some((row_num - 1, col_num - 1))
}

/// A1:B4 → ((r0,c0),(r1,c1)) inclusive, normalised so min<=max. Single cell "A1" is a
/// 1x1 range.
pub fn parse_range(s: &str) -> Option<((u32, u32), (u32, u32))> {
    let s = s.trim();
    match s.split_once(':') {
        Some((a, b)) => {
            let (ar, ac) = parse_cell(a)?;
            let (br, bc) = parse_cell(b)?;
            Some(((ar.min(br), ac.min(bc)), (ar.max(br), ac.max(bc))))
        }
        None => {
            let (r, c) = parse_cell(s)?;
            Some(((r, c), (r, c)))
        }
    }
}

/// Does a target cell fall inside a range? Used to detect that writing an app-owned
/// cell would land on a user's array/spill range (which the write must not clobber).
pub fn cell_in_range(cell: &str, range: &str) -> Option<bool> {
    let (r, c) = parse_cell(cell)?;
    let ((r0, c0), (r1, c1)) = parse_range(range)?;
    Some(r >= r0 && r <= r1 && c >= c0 && c <= c1)
}

/// Should the write be blocked? §4.4.2: block if any target cell intersects any
/// array/spill range, OR if any range could not be safely identified (fail closed).
/// `unresolved` means the reader could not determine a formula's range (so we cannot
/// prove non-intersection).
pub fn write_blocked(targets: &[&str], spill_ranges: &[&str], unresolved: bool) -> bool {
    if unresolved {
        return true; // cannot prove safety → refuse
    }
    for t in targets {
        for r in spill_ranges {
            match cell_in_range(t, r) {
                Some(true) => return true, // intersects a spill range
                Some(false) => {}
                None => return true, // unparseable target/range → refuse
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::excel_link::fingerprint::to_hex;

    fn fp(byte: u8) -> Fingerprint {
        [byte; 32]
    }

    // ---- restore rule (§4.4.2) ----

    fn persisted(last: Option<Fingerprint>, recalc: bool) -> Persisted {
        Persisted {
            last_app_write: last,
            recalc_requested: recalc,
            value_readable: true,
        }
    }

    #[test]
    fn unverified_when_never_written() {
        assert_eq!(
            restore(&fp(1), &persisted(None, false)),
            CalcState::Unverified
        );
    }

    #[test]
    fn stale_sticky_when_file_unchanged_since_app_write() {
        // The whole point of persistence: an app restart (plain re-read) must keep Stale.
        assert_eq!(
            restore(&fp(7), &persisted(Some(fp(7)), true)),
            CalcState::Stale
        );
    }

    #[test]
    fn trusted_when_changed_and_recalc_requested() {
        assert_eq!(
            restore(&fp(9), &persisted(Some(fp(7)), true)),
            CalcState::Trusted
        );
    }

    #[test]
    fn unverified_when_changed_without_recalc_request() {
        assert_eq!(
            restore(&fp(9), &persisted(Some(fp(7)), false)),
            CalcState::Unverified
        );
    }

    #[test]
    fn missing_overrides_everything() {
        let p = Persisted {
            last_app_write: Some(fp(7)),
            recalc_requested: true,
            value_readable: false,
        };
        assert_eq!(restore(&fp(9), &p), CalcState::Missing);
    }

    #[test]
    fn usability() {
        assert!(CalcState::Unverified.is_usable());
        assert!(CalcState::Trusted.is_usable());
        assert!(!CalcState::Stale.is_usable());
        assert!(!CalcState::Missing.is_usable());
    }

    // ---- persisted_from_state (V5 supply side, implementation.md §2.2) ----

    fn state_row(last_app_write_fp: Option<String>, recalc: bool) -> LinkState {
        LinkState {
            sync_status: crate::model::SyncStatus::Linked,
            sync_error: None,
            calc_state: CalcState::Unverified,
            recalc_requested: recalc,
            fp_algo: "sha256-v1".into(),
            last_read_fp: None,
            last_read_at: None,
            last_app_write_fp,
            last_app_write_at: None,
            last_verified_read_fp: None,
            structure_fp: None,
            structure_fp_algo: "structure-v1".into(),
            data_first_row: None,
            data_last_row: None,
            row_count: None,
            ec_generation: 0,
            applied_generation: 0,
        }
    }

    #[test]
    fn persisted_from_state_roundtrips_hex() {
        let s = state_row(Some(to_hex(&fp(7))), true);
        let p = persisted_from_state(&s, true).unwrap();
        assert_eq!(p.last_app_write, Some(fp(7)));
        assert!(p.recalc_requested);
        // End to end through the restore rule: unchanged file stays Stale.
        assert_eq!(restore(&fp(7), &p), CalcState::Stale);
    }

    #[test]
    fn persisted_from_state_never_written() {
        let p = persisted_from_state(&state_row(None, false), true).unwrap();
        assert_eq!(p.last_app_write, None);
        assert_eq!(restore(&fp(1), &p), CalcState::Unverified);
    }

    #[test]
    fn persisted_from_state_rejects_corrupt_hex() {
        // A corrupt stored fingerprint must be an error, not "never wrote" — mapping it
        // to None would drop Stale and break the §4.4.2 restart guarantee.
        let s = state_row(Some("not-hex".into()), true);
        assert!(persisted_from_state(&s, true).is_err());
    }

    // ---- cell / range parsing ----

    #[test]
    fn parse_cells() {
        assert_eq!(parse_cell("A1"), Some((0, 0)));
        assert_eq!(parse_cell("B2"), Some((1, 1)));
        assert_eq!(parse_cell("Z1"), Some((0, 25)));
        assert_eq!(parse_cell("AA1"), Some((0, 26)));
        assert_eq!(parse_cell("D2"), Some((1, 3)));
    }

    #[test]
    fn parse_cell_fails_closed() {
        assert_eq!(parse_cell("$A$1"), None); // absolute
        assert_eq!(parse_cell("A:A"), None); // whole column
        assert_eq!(parse_cell("A0"), None); // row 0 invalid
        assert_eq!(parse_cell(""), None);
        assert_eq!(parse_cell("1A"), None);
    }

    #[test]
    fn range_membership() {
        assert_eq!(cell_in_range("B2", "A1:C3"), Some(true));
        assert_eq!(cell_in_range("D2", "A1:C3"), Some(false));
        assert_eq!(cell_in_range("J6", "J5:J7"), Some(true)); // spill
        assert_eq!(cell_in_range("K6", "J5:J7"), Some(false));
        assert_eq!(cell_in_range("A1", "A1"), Some(true)); // 1x1
    }

    // ---- write_blocked (§4.4.2) ----

    #[test]
    fn block_on_spill_intersection() {
        // App wants to write J6, which is inside a user's UNIQUE() spill J5:J7 → block.
        assert!(write_blocked(&["J6"], &["J5:J7"], false));
    }

    #[test]
    fn allow_when_clear_of_spill() {
        // App writes D2; the only spill is far away → allowed.
        assert!(!write_blocked(&["D2"], &["J5:J7"], false));
    }

    #[test]
    fn block_when_range_unresolved() {
        // Reader could not determine some formula's range → fail closed even with no
        // known ranges.
        assert!(write_blocked(&["D2"], &[], true));
    }

    #[test]
    fn block_when_range_unparseable() {
        // A spill range we cannot parse (e.g. cross-sheet) must not be assumed safe.
        assert!(write_blocked(&["D2"], &["Sheet2!A1:A3"], false));
    }
}
