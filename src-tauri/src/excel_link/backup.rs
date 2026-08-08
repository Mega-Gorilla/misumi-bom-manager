// Backup handling for the write path (plan.md §4.2.2 バックアップ運用 +
// implementation.md §0 retention decision).
//
// The replace-backup is BOTH the conflict-detection evidence and the only copy of a
// displaced external version, so every transition here fails safe: a failed
// transfer keeps the original file in place (with a warning), and the retention
// policy never touches unresolved conflicts or files still mid-transfer.

use crate::excel_link::{fingerprint, store};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Unique backup name beside the workbook (same volume — the ReplaceFileW
/// constraint, §4.2.2). Regenerates on the (theoretical) nanosecond collision and
/// on a pre-existing file (§7.1.1 item 6: existing-name collisions must be safe).
pub fn unique_backup_path(dir: &Path, stem: &str) -> PathBuf {
    loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = dir.join(format!("{stem}.mbm-backup-{nanos}.xlsx"));
        if !p.exists() {
            return p;
        }
    }
}

/// §4.2.2 transfer steps 2-6: move the backup out of the (possibly cloud-synced)
/// workbook folder into the app-data backup dir. Cross-volume moves are copy+delete
/// and therefore non-atomic — hence the temp-name + hash-verify + atomic-rename
/// sequence, and the original is deleted ONLY after everything succeeded.
/// On failure the original backup stays and the returned warning tells the user.
pub fn transfer(
    conn: &Connection,
    ledger_id: i64,
    from: &Path,
    app_backup_dir: &Path,
) -> Result<(), String> {
    let fail = |step: &str, detail: String| {
        format!(
            "バックアップの移送に失敗しました ({step}): {detail}。元のバックアップは {} に残っています",
            from.display()
        )
    };

    std::fs::create_dir_all(app_backup_dir).map_err(|e| fail("準備", e.to_string()))?;
    let final_name = from
        .file_name()
        .ok_or_else(|| fail("準備", "バックアップ名が不正です".into()))?;
    let tmp = app_backup_dir.join(format!("{}.transfer-tmp", final_name.to_string_lossy()));
    let final_path = app_backup_dir.join(final_name);

    // (2) copy to a TEMP name in app data.
    std::fs::copy(from, &tmp).map_err(|e| fail("コピー", e.to_string()))?;
    // (3) flush/close happened (fs::copy closes); verify SHA-256 against the ledger.
    let expected: String = conn
        .query_row(
            "SELECT backup_fp FROM bom_link_backup WHERE id = ?1",
            [ledger_id],
            |r| r.get(0),
        )
        .map_err(|e| fail("照合", e.to_string()))?;
    let actual = fingerprint::file_fingerprint(&tmp)
        .map(|fp| fingerprint::to_hex(&fp))
        .map_err(|e| fail("照合", e.to_string()))?;
    if actual != expected {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail("照合", "コピーの SHA-256 が一致しません".into()));
    }
    // (4) atomic rename WITHIN the app-data volume.
    std::fs::rename(&tmp, &final_path).map_err(|e| fail("rename", e.to_string()))?;
    // (5) record the transfer, then delete the original.
    store::mark_transferred(conn, ledger_id, &final_path.to_string_lossy())
        .map_err(|e| fail("台帳更新", e.to_string()))?;
    std::fs::remove_file(from).map_err(|e| {
        // The transfer itself succeeded; only the original could not be removed.
        format!(
            "バックアップは移送済みですが、元ファイルの削除に失敗しました: {} ({e})。手動で削除してください",
            from.display()
        )
    })?;
    Ok(())
}

/// Retention policy execution (implementation.md §0): candidates come from the SQL
/// predicate (rank>5 AND age>30d, excluding unresolved conflicts / volume_temp /
/// already-deleted); the FILE delete must succeed before mark_deleted, and a
/// failure keeps ledger + file with a warning.
pub fn run_retention(conn: &Connection, origin_bom_id: &str) -> Vec<String> {
    let mut warnings = Vec::new();
    let candidates = match store::retention_candidates(conn, origin_bom_id) {
        Ok(c) => c,
        Err(e) => return vec![format!("バックアップ保持ポリシーの照会に失敗しました: {e}")],
    };
    for id in candidates {
        let path: Result<String, _> = conn.query_row(
            "SELECT backup_path FROM bom_link_backup WHERE id = ?1",
            [id],
            |r| r.get(0),
        );
        let Ok(path) = path else { continue };
        match std::fs::remove_file(&path) {
            Ok(()) => {
                if let Err(e) = store::mark_deleted(conn, id) {
                    warnings.push(format!(
                        "バックアップ台帳の更新に失敗しました (id={id}): {e}"
                    ));
                }
            }
            Err(e) => warnings.push(format!(
                "古いバックアップの削除に失敗しました: {path} ({e})。ファイルと台帳は残しています"
            )),
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::run_migrations(&conn).unwrap();
        conn
    }

    fn seed_bom(conn: &Connection, id: &str) {
        conn.execute(
            "INSERT INTO bom(id, name, created_at, updated_at) \
             VALUES(?1, 'test', datetime('now'), datetime('now'))",
            [id],
        )
        .unwrap();
    }

    fn dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("mbm-backup-{tag}"));
        let _ = std::fs::remove_dir_all(&base);
        let wb = base.join("workbook-dir");
        let app = base.join("app-data");
        std::fs::create_dir_all(&wb).unwrap();
        (wb, app)
    }

    /// Ledger row + on-disk backup file whose fingerprint the ledger records.
    fn ledger_backup(
        conn: &Connection,
        dir: &Path,
        name: &str,
        content: &[u8],
    ) -> (i64, std::path::PathBuf) {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        let fp = fingerprint::to_hex(&fingerprint::file_fingerprint(&path).unwrap());
        let id = store::insert_backup(
            conn,
            &store::NewBackup {
                origin_bom_id: "B1".into(),
                workbook_path: "wb.xlsx".into(),
                backup_path: path.to_string_lossy().into_owned(),
                backup_fp: fp.clone(),
                f0_fp: fp,
            },
        )
        .unwrap();
        (id, path)
    }

    #[test]
    fn unique_backup_path_avoids_existing_files() {
        let (wb, _) = dirs("unique");
        let p1 = unique_backup_path(&wb, "bom");
        assert!(p1
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("bom.mbm-backup-"));
        std::fs::write(&p1, b"x").unwrap();
        let p2 = unique_backup_path(&wb, "bom");
        assert_ne!(p1, p2, "existing file must force a fresh name");
    }

    #[test]
    fn transfer_moves_backup_into_app_data_and_updates_ledger() {
        let conn = mem();
        seed_bom(&conn, "B1");
        let (wb, app) = dirs("transfer-ok");
        let (id, from) = ledger_backup(&conn, &wb, "b.mbm-backup-1.xlsx", b"backup bytes");

        transfer(&conn, id, &from, &app).unwrap();

        assert!(!from.exists(), "original must be deleted after the move");
        let rec = &store::list_backups(&conn, "B1").unwrap()[0];
        assert_eq!(rec.location, crate::model::BackupLocation::AppData);
        assert!(rec.transferred_at.is_some());
        let moved = Path::new(&rec.backup_path);
        assert!(moved.starts_with(&app));
        assert_eq!(std::fs::read(moved).unwrap(), b"backup bytes");
        // No transfer-tmp litter left behind.
        assert!(std::fs::read_dir(&app).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("transfer-tmp")));
    }

    #[test]
    fn failed_hash_check_keeps_original_and_warns() {
        let conn = mem();
        seed_bom(&conn, "B1");
        let (wb, app) = dirs("transfer-bad");
        let (id, from) = ledger_backup(&conn, &wb, "b.mbm-backup-1.xlsx", b"backup bytes");
        // Simulate a copy that would not match the ledger: corrupt the ledger fp.
        // (backup_fp/f0_fp must stay equal or the derived-conflict CHECK fires.)
        conn.execute(
            "UPDATE bom_link_backup SET backup_fp = 'deadbeef', f0_fp = 'deadbeef' WHERE id = ?1",
            [id],
        )
        .unwrap();

        let err = transfer(&conn, id, &from, &app).unwrap_err();
        assert!(err.contains("照合"), "{err}");
        assert!(err.contains("元のバックアップは"), "{err}");
        assert!(from.exists(), "original must survive a failed transfer");
        let rec = &store::list_backups(&conn, "B1").unwrap()[0];
        assert_eq!(rec.location, crate::model::BackupLocation::VolumeTemp);
        // The bad tmp copy was cleaned up.
        assert!(std::fs::read_dir(&app).unwrap().next().is_none());
    }

    #[test]
    fn retention_deletes_only_on_file_success_and_spares_conflicts() {
        let conn = mem();
        seed_bom(&conn, "B1");
        let (wb, app) = dirs("retention");
        std::fs::create_dir_all(&app).unwrap();

        // 8 transferred backups, oldest first (distinct created_at for a stable
        // rank), all older than 30 days → rank>5 gives 3 candidates. One candidate
        // is an unresolved CONFLICT (must survive), one has its file already gone
        // (warn, ledger kept), one deletes cleanly.
        let mut ids = Vec::new();
        for i in 0..8 {
            let (id, from) = ledger_backup(&conn, &wb, &format!("b{i}.mbm-backup.xlsx"), b"x");
            transfer(&conn, id, &from, &app).unwrap();
            conn.execute(
                "UPDATE bom_link_backup SET created_at = datetime('now','localtime', ?2) WHERE id = ?1",
                rusqlite::params![id, format!("-{} days", 60 - i)],
            )
            .unwrap();
            ids.push(id);
        }
        // ids[0] is the oldest (rank 8). Make ids[1] (rank 7) an unresolved conflict.
        conn.execute(
            "UPDATE bom_link_backup SET f0_fp = 'other', is_conflict = 1 WHERE id = ?1",
            [ids[1]],
        )
        .unwrap();
        // Remove ids[2]'s file up front → its delete must fail with a warning.
        let recs = store::list_backups(&conn, "B1").unwrap();
        let path_of = |id: i64| {
            recs.iter()
                .find(|r| r.id == id)
                .map(|r| r.backup_path.clone())
                .unwrap()
        };
        std::fs::remove_file(path_of(ids[2])).unwrap();

        let warnings = run_retention(&conn, "B1");

        let recs = store::list_backups(&conn, "B1").unwrap();
        let rec_of = |id: i64| recs.iter().find(|r| r.id == id).unwrap();
        // ids[0]: clean delete — file gone, deleted_at set.
        assert!(rec_of(ids[0]).deleted_at.is_some());
        assert!(!Path::new(&path_of(ids[0])).exists());
        // ids[1]: unresolved conflict — never a candidate, file intact.
        assert!(rec_of(ids[1]).deleted_at.is_none());
        assert!(Path::new(&path_of(ids[1])).exists());
        // ids[2]: file delete failed — warned, ledger NOT marked deleted.
        assert!(rec_of(ids[2]).deleted_at.is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("削除に失敗"), "{warnings:?}");
        // Newest 5 (ids[3..8]) untouched.
        for &id in &ids[3..] {
            assert!(rec_of(id).deleted_at.is_none());
            assert!(Path::new(&path_of(id)).exists());
        }
    }
}
