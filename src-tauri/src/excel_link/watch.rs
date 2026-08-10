// File watching for Excel link mode (plan.md §4.2.3 / implementation.md §2.4 — PR-7).
//
// `notify` is used DIRECTLY (not tauri-plugin-fs: its watch is a frontend API whose
// paths must be pre-declared in the ACL scope, which does not fit dialog-picked
// arbitrary workbook paths). Raw events are judged here and only SYNTHESIZED events
// reach the frontend.
//
// §4.2.3 requirements implemented here:
// - watch the PARENT DIRECTORY, not the file path (Excel saves via temp-file +
//   atomic replace, which would detach a path watch)
// - DEBOUNCE ("a quiet period after the last event") — the save burst fires many
//   raw events; one synthesized event per burst
// - filter by TARGET FILE NAME: the `~$` owner file's create/delete also raises
//   events. Its DELETION is meaningful though — Excel closed the workbook — and
//   becomes the dedicated ExcelClosed event (the §9-5 pending auto-retry trigger)
// - read retries are NOT here: a partial read during a save is absorbed by
//   read_stable's fingerprint sandwich, and a failed open is re-triggered by the
//   next debounced event
//
// Responsibility boundary (§4.2.3): watching is a CONVENIENCE TRIGGER only. Events
// may be lost; correctness always comes from read-time verification (§4.9) and the
// pre-write fingerprint check (§4.6.2).

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::Connection;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crate::excel_link::{store, stored_read_path};

/// One synthesized observation per debounce window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchEvent {
    /// The workbook itself changed (Excel save, external write, sync arrival) —
    /// the frontend re-opens (§2.3: emit → excel_link_open).
    Changed,
    /// The `~$` owner file disappeared — Excel closed the workbook. The frontend
    /// retries a pending apply (§9-5 completion).
    ExcelClosed,
}

/// Keep this alive to keep watching; dropping it stops the watcher, which closes
/// the event channel and ends the debounce thread.
pub struct WatchHandle {
    _watcher: RecommendedWatcher,
}

/// Resolve what to watch for a linked BOM: (parent dir, workbook file name).
/// Uses the STORED resolved path only (same cheap rule as the status lock hint —
/// no check_env under the DB mutex). An unresolved .lnk cannot be watched yet;
/// the next open records the resolved path.
pub fn watch_target(conn: &Connection, bom_id: &str) -> Result<(PathBuf, OsString), String> {
    let rec = store::get_link(conn, bom_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "この BOM は Excel にリンクされていません".to_string())?;
    let target = stored_read_path(&rec.header)
        .ok_or_else(|| "リンク先を解決できません。一度リンクを開いてください".to_string())?;
    match (target.parent(), target.file_name()) {
        (Some(dir), Some(name)) => Ok((dir.to_path_buf(), name.to_os_string())),
        _ => Err(format!("監視対象のパスが不正です: {}", target.display())),
    }
}

/// Start watching `dir` for changes to `file_name` (and its `~$` owner file).
/// Synthesized events are delivered on a dedicated debounce thread; `on_event`
/// must therefore be Send (the Tauri AppHandle emit closure is).
pub fn start_watch(
    dir: &Path,
    file_name: &std::ffi::OsStr,
    debounce: Duration,
    on_event: impl Fn(WatchEvent) + Send + 'static,
) -> Result<WatchHandle, String> {
    let target = file_name.to_string_lossy().to_lowercase();
    let owner = format!("~${target}");

    let (tx, rx) = mpsc::channel::<notify::Event>();
    let mut watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev); // receiver gone = watcher being dropped: ignore
            }
        })
        .map_err(|e| format!("監視の初期化に失敗しました: {e}"))?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .map_err(|e| format!("監視を開始できません ({}): {e}", dir.display()))?;

    std::thread::spawn(move || {
        let mut changed = false;
        let mut excel_closed = false;
        let mut raw_count = 0usize;
        loop {
            match rx.recv_timeout(debounce) {
                Ok(ev) => {
                    raw_count += 1;
                    for p in &ev.paths {
                        let Some(name) = p.file_name().map(|n| n.to_string_lossy().to_lowercase())
                        else {
                            continue;
                        };
                        if name == target {
                            // Any raw kind counts: the atomic-replace save shows up
                            // as remove/create/rename bursts depending on the OS.
                            changed = true;
                        } else if name == owner {
                            // Owner-file CREATION (Excel opened the file) is noise
                            // for us (§4.2.3: filter); only its removal matters.
                            if matches!(ev.kind, notify::EventKind::Remove(_)) {
                                excel_closed = true;
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if changed || excel_closed {
                        // Calibration aid (implementation.md §0: debounce width is
                        // decided by real-Excel measurement): debug builds report
                        // how many raw events each burst coalesced.
                        #[cfg(debug_assertions)]
                        eprintln!(
                            "[excel-link watch] {raw_count} raw events -> changed={changed} excelClosed={excel_closed}"
                        );
                        // Changed first: the frontend re-reads before a pending
                        // apply retries, so apply's step-1 fingerprint check sees
                        // the fresh last_read_fp.
                        if changed {
                            on_event(WatchEvent::Changed);
                        }
                        if excel_closed {
                            on_event(WatchEvent::ExcelClosed);
                        }
                        changed = false;
                        excel_closed = false;
                        raw_count = 0;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    Ok(WatchHandle { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const DEBOUNCE: Duration = Duration::from_millis(120);
    const WAIT: Duration = Duration::from_secs(3);

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("mbm-watch-{tag}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn start(d: &Path, name: &str) -> (WatchHandle, mpsc::Receiver<WatchEvent>) {
        let (tx, rx) = mpsc::channel();
        let h = start_watch(d, std::ffi::OsStr::new(name), DEBOUNCE, move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
        (h, rx)
    }

    /// Drain everything the debouncer emits within ~2 windows.
    fn drain(rx: &mpsc::Receiver<WatchEvent>) -> Vec<WatchEvent> {
        let mut out = Vec::new();
        if let Ok(ev) = rx.recv_timeout(WAIT) {
            out.push(ev);
            // keep collecting briefly in case a second synthesized event follows
            while let Ok(ev) = rx.recv_timeout(DEBOUNCE * 3) {
                out.push(ev);
            }
        }
        out
    }

    /// §4.2.3: the save burst (multiple writes + atomic replace) coalesces into
    /// exactly ONE Changed per quiet period.
    #[test]
    fn save_burst_coalesces_into_one_changed() {
        let d = dir("burst");
        let target = d.join("bom.xlsx");
        std::fs::write(&target, b"v0").unwrap();
        let (_h, rx) = start(&d, "bom.xlsx");

        // Excel-like save: temp write ×2 + atomic replace over the target.
        std::fs::write(&target, b"v1").unwrap();
        std::fs::write(&target, b"v2").unwrap();
        let tmp = d.join("bom.tmp");
        std::fs::write(&tmp, b"v3").unwrap();
        std::fs::rename(&tmp, &target).unwrap();

        let events = drain(&rx);
        assert_eq!(events, vec![WatchEvent::Changed], "{events:?}");
    }

    /// Parent-directory watching survives the atomic replace: a LATER change to
    /// the (new inode) target is still detected.
    #[test]
    fn watch_survives_atomic_replace() {
        let d = dir("replace");
        let target = d.join("bom.xlsx");
        std::fs::write(&target, b"v0").unwrap();
        let (_h, rx) = start(&d, "bom.xlsx");

        let tmp = d.join("bom.tmp");
        std::fs::write(&tmp, b"v1").unwrap();
        std::fs::rename(&tmp, &target).unwrap();
        assert_eq!(drain(&rx), vec![WatchEvent::Changed]);

        // Second, separate burst after the replace — the watch must still fire.
        std::fs::write(&target, b"v2").unwrap();
        assert_eq!(drain(&rx), vec![WatchEvent::Changed]);
    }

    /// Unrelated files and the owner file's CREATION are filtered out (§4.2.3).
    #[test]
    fn unrelated_and_owner_create_are_filtered() {
        let d = dir("filter");
        std::fs::write(d.join("bom.xlsx"), b"v0").unwrap();
        let (_h, rx) = start(&d, "bom.xlsx");

        std::fs::write(d.join("other.xlsx"), b"x").unwrap();
        std::fs::write(d.join("~$bom.xlsx"), b"owner").unwrap(); // Excel opened
        assert!(
            rx.recv_timeout(Duration::from_millis(600)).is_err(),
            "no event may fire for unrelated files / owner creation"
        );
    }

    /// §9-5 core trigger: deleting the `~$` owner file (= Excel closed the
    /// workbook) fires ExcelClosed.
    #[test]
    fn owner_file_removal_fires_excel_closed() {
        let d = dir("closed");
        std::fs::write(d.join("bom.xlsx"), b"v0").unwrap();
        let owner = d.join("~$bom.xlsx");
        std::fs::write(&owner, b"owner").unwrap();
        let (_h, rx) = start(&d, "bom.xlsx");

        std::fs::remove_file(&owner).unwrap();
        assert_eq!(drain(&rx), vec![WatchEvent::ExcelClosed]);
    }

    /// Dropping the handle stops the watcher (the excel_link_watch(enable=false)
    /// path) — later changes fire nothing.
    #[test]
    fn drop_stops_the_watch() {
        let d = dir("stop");
        let target = d.join("bom.xlsx");
        std::fs::write(&target, b"v0").unwrap();
        let (h, rx) = start(&d, "bom.xlsx");
        drop(h);
        std::thread::sleep(Duration::from_millis(100)); // let the thread wind down
        std::fs::write(&target, b"v1").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(600)).is_err());
    }
}
