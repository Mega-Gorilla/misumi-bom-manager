//! Step 3c: real-sync PoC primitives (plan.md §6.2, Issue #23).
//!
//! Four subcommands, all with their decision logic factored into pure functions so the judge
//! can be regression-tested without Drive, Excel or a second machine:
//!
//! - `marker` — machine-read which version markers survive in a case file. The B0/A1/R1 verdict
//!   must come from cell contents + fingerprints, never from tray icons (Issue #23 §3: the tray
//!   is auxiliary evidence only).
//! - `watch-stable` — wait for a sync-driven change to arrive and settle. Encodes the Issue #23
//!   timeout rule: max wait, N seconds unchanged = converged, still churning at timeout =
//!   INCONCLUSIVE evidence (exit 3), no change at all = exit 2.
//! - `sync-init` — build the per-case test tree + sentinel inside the Drive mirror. Every case
//!   gets a FRESH copy of the template (new Drive file id, empty version history) so recovered
//!   versions can be attributed to THIS run.
//! - `sync-guard` — the ONLY path allowed to delete the test tree. Five conditions, all must
//!   hold; exit code is the authority (same pattern as `decide`, step 3b).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::state::Fingerprint;

type R<T> = Result<T, String>;

/// Case folders created under the PoC root. 05a/05b and the presence-ON set (91/92) are separate
/// folders because each scenario must start from a fresh file id + empty history (Issue #23 §2).
pub(crate) const CASES: &[&str] = &[
    "case-01-normal",
    "case-02-remote-first",
    "case-03-before-replace",
    "case-04-remote-after-replace",
    "case-05a-pause-both-a-first",
    "case-05b-pause-both-b-first",
    "case-06-offline-return",
    "case-07-conflict-copy",
    "case-08-backup-sync",
    "case-91-presence-normal",
    "case-92-presence-conflict",
];

pub(crate) const SENTINEL: &str = ".mbm-sync-poc-root";

fn root_name(run_id: &str) -> String {
    format!("__mbm_sync_poc_{run_id}")
}

// ---- marker ------------------------------------------------------------------------------

/// Display value of one cell: raw number, shared string (t="s"), or inline string. Returns None
/// when the cell is absent or empty. Pure over the sheet XML so it is testable on hand-built XML.
pub(crate) fn cell_display(sheet_xml: &str, cell_ref: &str, ss: &[String]) -> Option<String> {
    let mut rest = sheet_xml;
    while let Some(i) = rest.find("<c ") {
        rest = &rest[i..];
        let gt = rest.find('>')?;
        let open = &rest[..gt];
        let self_closing = open.ends_with('/');
        let r = crate::slice_between_pub(open, "r=\"", "\"").unwrap_or("");
        let body_end = if self_closing {
            gt + 1
        } else {
            match rest[gt..].find("</c>") {
                Some(j) => gt + j + 4,
                None => return None,
            }
        };
        if r == cell_ref {
            if self_closing {
                return None; // style-only cell, no content
            }
            let body = &rest[gt + 1..body_end - 4];
            if open.contains("t=\"s\"") {
                let idx: usize = crate::slice_between_pub(body, "<v>", "</v>")?
                    .parse()
                    .ok()?;
                return ss.get(idx).cloned();
            }
            if body.contains("<is>") {
                return crate::slice_between_pub(body, "<t", "</t>")
                    .and_then(|t| t.split_once('>').map(|(_, v)| v.to_string()));
            }
            return crate::slice_between_pub(body, "<v>", "</v>").map(|v| v.to_string());
        }
        rest = &rest[body_end..];
    }
    None
}

/// marker <xlsx> [ecCell] [userCell] — one machine-readable line:
/// `markers D2=1234 G2=R1 fp=<sha256>`. `(absent)` when a cell is missing/empty.
pub(crate) fn cmd_marker(path: &Path, ec_cell: &str, user_cell: &str) -> R<()> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("not a zip: {e}"))?;
    let part = crate::sheet_part_for(&mut zip, crate::TARGET_SHEET)?;
    let mut xml = String::new();
    zip.by_name(&part)
        .map_err(|e| e.to_string())?
        .read_to_string(&mut xml)
        .map_err(|e| e.to_string())?;
    let ss = crate::stale_scan::shared_strings(path);
    let fp = crate::fingerprint(path)?;
    let show = |c: &str| cell_display(&xml, c, &ss).unwrap_or_else(|| "(absent)".into());
    println!(
        "markers {ec_cell}={} {user_cell}={} fp={}",
        show(ec_cell),
        show(user_cell),
        hex(&fp)
    );
    Ok(())
}

pub(crate) fn hex(fp: &Fingerprint) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- watch-stable ------------------------------------------------------------------------

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum WatchOutcome {
    /// A change arrived and the file then stayed identical for the stability window.
    Converged,
    /// Timeout with no change at all (sync never delivered anything).
    NoChange,
    /// Still changing at timeout — the "does not settle" INCONCLUSIVE evidence.
    Unstable,
}

/// Convergence state machine, pure over an injected (t_seconds, fingerprint) sample stream.
/// A sample equal to the previous one only counts toward stability once a change has been seen
/// (or a baseline was given and the first sample already differs from it).
pub(crate) struct WatchJudge {
    timeout_s: u64,
    stable_s: u64,
    last_fp: Option<Fingerprint>,
    changed: bool,
    last_change_t: u64,
}

impl WatchJudge {
    pub(crate) fn new(timeout_s: u64, stable_s: u64, baseline: Option<Fingerprint>) -> Self {
        Self {
            timeout_s,
            stable_s,
            last_fp: baseline,
            changed: false,
            last_change_t: 0,
        }
    }

    /// Feed one sample; Some(outcome) ends the watch. Returns whether this sample was a change
    /// via the bool so the I/O shell can log it.
    pub(crate) fn observe(&mut self, t: u64, fp: Fingerprint) -> (bool, Option<WatchOutcome>) {
        let is_change = match self.last_fp {
            Some(prev) => prev != fp,
            None => false, // first sample with no baseline = just the baseline, not a change
        };
        if is_change {
            self.changed = true;
            self.last_change_t = t;
        }
        self.last_fp = Some(fp);
        if self.changed && !is_change && t.saturating_sub(self.last_change_t) >= self.stable_s {
            return (is_change, Some(WatchOutcome::Converged));
        }
        if t >= self.timeout_s {
            let out = if self.changed {
                WatchOutcome::Unstable
            } else {
                WatchOutcome::NoChange
            };
            return (is_change, Some(out));
        }
        (is_change, None)
    }
}

/// watch-stable <file> [timeoutS] [stableS] [baselineHex] — poll every second. Exit codes:
/// 0 = Converged, 2 = NoChange, 3 = Unstable. A transiently unreadable file (mid-sync lock)
/// is logged and skipped, not treated as a change.
pub(crate) fn cmd_watch_stable(
    path: &Path,
    timeout_s: u64,
    stable_s: u64,
    baseline: Option<Fingerprint>,
) -> R<()> {
    println!(
        "== watch-stable {} (timeout {timeout_s}s, stable {stable_s}s) ==",
        path.display()
    );
    let mut judge = WatchJudge::new(timeout_s, stable_s, baseline);
    let start = std::time::Instant::now();
    loop {
        let t = start.elapsed().as_secs();
        match crate::fingerprint(path) {
            Ok(fp) => {
                let (was_change, outcome) = judge.observe(t, fp);
                if was_change {
                    println!("   [t={t:>4}s] changed -> fp={}", &hex(&fp)[..16]);
                }
                if let Some(o) = outcome {
                    println!("   [t={t:>4}s] result: {o:?}");
                    match o {
                        WatchOutcome::Converged => return Ok(()),
                        WatchOutcome::NoChange => std::process::exit(2),
                        WatchOutcome::Unstable => std::process::exit(3),
                    }
                }
            }
            Err(e) => println!("   [t={t:>4}s] unreadable (mid-sync?): {e}"),
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

// ---- sync-init ---------------------------------------------------------------------------

/// sync-init <mirror-root> <run-id> <template.xlsx> — create `__mbm_sync_poc_<run-id>/` with the
/// sentinel and one fresh template copy per case. Refuses to touch an existing root.
pub(crate) fn cmd_sync_init(mirror_root: &Path, run_id: &str, template: &Path) -> R<()> {
    if !template.is_file() {
        return Err(format!("template not found: {}", template.display()));
    }
    if !mirror_root.is_dir() {
        return Err(format!("mirror root not a dir: {}", mirror_root.display()));
    }
    let root = mirror_root.join(root_name(run_id));
    if root.exists() {
        return Err(format!("refusing: {} already exists", root.display()));
    }
    std::fs::create_dir(&root).map_err(|e| e.to_string())?;
    std::fs::write(root.join(SENTINEL), format!("UUID={run_id}\n")).map_err(|e| e.to_string())?;
    for case in CASES {
        let dir = root.join(case);
        std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
        std::fs::copy(template, dir.join("bom.xlsx")).map_err(|e| e.to_string())?;
    }
    println!("== sync-init ==");
    println!("   root    : {}", root.display());
    println!("   sentinel: {SENTINEL} (UUID={run_id})");
    println!("   cases   : {} x bom.xlsx (fresh file each)", CASES.len());
    Ok(())
}

// ---- sync-guard --------------------------------------------------------------------------

/// Facts gathered by the I/O shell; the verdict over them is pure (unit-tested).
pub(crate) struct GuardFacts {
    /// canonical(parent) + file_name == canonical(dir): the final component is a real directory,
    /// not a junction/symlink pointing elsewhere.
    pub literal_dir: bool,
    /// symlink_metadata says the path itself is a reparse point.
    pub is_reparse: bool,
    /// Final component of the CANONICAL path (so a renamed doorway cannot spoof it).
    pub canonical_name: String,
    /// UUID= value read from the sentinel file, if present.
    pub sentinel_uuid: Option<String>,
}

/// Issue #23 §5: delete only when ALL conditions hold. Returns every violated condition
/// (anomaly-collecting, same design lesson as the structure judge).
pub(crate) fn cleanup_violations(f: &GuardFacts, run_id: &str) -> Vec<String> {
    let mut v = Vec::new();
    if f.is_reparse || !f.literal_dir {
        v.push("target is (or resolves through) a junction/symlink — refuse".into());
    }
    let expect = root_name(run_id);
    if f.canonical_name != expect {
        v.push(format!(
            "canonical dir name '{}' != '{expect}' (wrong folder or the Drive root itself)",
            f.canonical_name
        ));
    }
    match &f.sentinel_uuid {
        None => v.push(format!("sentinel {SENTINEL} missing")),
        Some(u) if u != run_id => v.push(format!("sentinel UUID '{u}' != run-id '{run_id}'")),
        _ => {}
    }
    v
}

fn gather_facts(dir: &Path) -> R<GuardFacts> {
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let is_reparse = meta.file_type().is_symlink();
    let canon = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let literal_dir = match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|cp| {
                cp.join(name).to_string_lossy().to_lowercase()
                    == canon.to_string_lossy().to_lowercase()
            })
            .unwrap_or(false),
        _ => false, // a bare drive root has no parent — never literal for our purposes
    };
    let canonical_name = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sentinel_uuid = std::fs::read_to_string(canon.join(SENTINEL))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("UUID=").map(|u| u.trim().to_string()))
        });
    Ok(GuardFacts {
        literal_dir,
        is_reparse,
        canonical_name,
        sentinel_uuid,
    })
}

/// sync-guard <dir> <run-id> [--delete] — verify the 5 conditions; with --delete, remove the
/// tree only when all hold. Exit code is the authority (Ok = allowed).
pub(crate) fn cmd_sync_guard(dir: &Path, run_id: &str, delete: bool) -> R<()> {
    let facts = gather_facts(dir)?;
    println!("== sync-guard {} ==", dir.display());
    println!("   canonical name: {}", facts.canonical_name);
    println!(
        "   literal dir   : {}   reparse: {}   sentinel UUID: {}",
        facts.literal_dir,
        facts.is_reparse,
        facts.sentinel_uuid.as_deref().unwrap_or("(missing)")
    );
    let violations = cleanup_violations(&facts, run_id);
    if !violations.is_empty() {
        for v in &violations {
            println!("   [REFUSE] {v}");
        }
        return Err(format!(
            "{} condition(s) violated — not deleting",
            violations.len()
        ));
    }
    println!("   [ ok ] all cleanup conditions hold");
    if delete {
        let canon = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
        std::fs::remove_dir_all(&canon).map_err(|e| e.to_string())?;
        println!("   deleted {}", canon.display());
    } else {
        println!("   (dry check only — pass --delete to remove)");
    }
    Ok(())
}

// ---- tests -------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(b: u8) -> Fingerprint {
        [b; 32]
    }

    // -- cell_display --

    const SHEET: &str = r#"<worksheet><sheetData>
        <row r="2"><c r="C2"><v>2</v></c><c r="D2" s="3"><v>800</v></c><c r="G2" t="s"><v>1</v></c></row>
        <row r="3"><c r="D3" s="1"/><c r="E3"><is><t xml:space="preserve">R1</t></is></c></row>
    </sheetData></worksheet>"#;

    fn ss() -> Vec<String> {
        vec!["irrelevant".into(), "B0".into()]
    }

    #[test]
    fn marker_reads_number_shared_and_inline() {
        assert_eq!(cell_display(SHEET, "D2", &ss()), Some("800".into()));
        assert_eq!(
            cell_display(SHEET, "G2", &ss()),
            Some("B0".into()),
            "shared string"
        );
        assert_eq!(
            cell_display(SHEET, "E3", &ss()),
            Some("R1".into()),
            "inline string"
        );
        assert_eq!(
            cell_display(SHEET, "D3", &ss()),
            None,
            "style-only cell is absent"
        );
        assert_eq!(
            cell_display(SHEET, "Z9", &ss()),
            None,
            "missing cell is absent"
        );
    }

    // -- WatchJudge --

    #[test]
    fn change_then_stability_converges() {
        let mut j = WatchJudge::new(300, 10, None);
        assert_eq!(j.observe(0, fp(0)), (false, None)); // baseline
        assert_eq!(j.observe(5, fp(1)), (true, None)); // change arrives
        assert_eq!(j.observe(10, fp(1)), (false, None)); // 5s stable — not yet
        assert_eq!(j.observe(15, fp(1)), (false, Some(WatchOutcome::Converged)));
    }

    #[test]
    fn no_change_times_out_as_nochange() {
        let mut j = WatchJudge::new(30, 10, None);
        assert_eq!(j.observe(0, fp(0)), (false, None));
        assert_eq!(j.observe(30, fp(0)), (false, Some(WatchOutcome::NoChange)));
    }

    #[test]
    fn churning_at_timeout_is_unstable() {
        let mut j = WatchJudge::new(30, 10, None);
        assert_eq!(j.observe(0, fp(0)), (false, None));
        assert_eq!(j.observe(10, fp(1)), (true, None));
        assert_eq!(j.observe(20, fp(2)), (true, None));
        assert_eq!(j.observe(30, fp(3)), (true, Some(WatchOutcome::Unstable)));
    }

    #[test]
    fn baseline_makes_first_differing_sample_a_change() {
        // With a baseline, R1 may already have landed before the watch starts — the first
        // sample differing from the baseline must count as the change.
        let mut j = WatchJudge::new(300, 10, Some(fp(0)));
        assert_eq!(j.observe(0, fp(1)), (true, None));
        assert_eq!(j.observe(10, fp(1)), (false, Some(WatchOutcome::Converged)));
    }

    // -- cleanup_violations --

    fn good_facts() -> GuardFacts {
        GuardFacts {
            literal_dir: true,
            is_reparse: false,
            canonical_name: "__mbm_sync_poc_run1".into(),
            sentinel_uuid: Some("run1".into()),
        }
    }

    #[test]
    fn all_conditions_hold_allows_cleanup() {
        assert!(cleanup_violations(&good_facts(), "run1").is_empty());
    }

    #[test]
    fn wrong_name_refused() {
        // The Drive root itself (or any folder not created by sync-init) must never qualify.
        let f = GuardFacts {
            canonical_name: "マイドライブ".into(),
            ..good_facts()
        };
        assert_eq!(cleanup_violations(&f, "run1").len(), 1);
    }

    #[test]
    fn missing_or_mismatched_sentinel_refused() {
        let missing = GuardFacts {
            sentinel_uuid: None,
            ..good_facts()
        };
        assert_eq!(cleanup_violations(&missing, "run1").len(), 1);
        let stale = GuardFacts {
            sentinel_uuid: Some("old-run".into()),
            ..good_facts()
        };
        assert_eq!(
            cleanup_violations(&stale, "run1").len(),
            1,
            "another run's tree is off-limits"
        );
    }

    #[test]
    fn reparse_point_refused_even_with_valid_sentinel() {
        // A junction NAMED like the poc root but pointing elsewhere must be refused by the
        // link checks alone — anomaly-collecting, so BOTH problems are reported if present.
        let f = GuardFacts {
            literal_dir: false,
            is_reparse: true,
            ..good_facts()
        };
        assert_eq!(cleanup_violations(&f, "run1").len(), 1);
    }
}
