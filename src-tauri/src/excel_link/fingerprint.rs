// Content fingerprints for Excel link mode (plan.md §4.6.1).
//
// The fingerprint is SHA-256 over the WHOLE file. mtime/size are never a basis for
// integrity decisions (§4.6.1) — they may only serve as a cheap pre-check elsewhere.
// Ported from tools/excel-link-poc (main.rs fingerprint/parse_fp, sync.rs hex); the
// retry wrapper is new here per §4.6.1: a read that races a writer is not a stable
// snapshot and must be retried or abandoned, never trusted.

use sha2::{Digest, Sha256};
use std::io;
use std::path::Path;
use std::time::Duration;

/// A content fingerprint. SHA-256 over the whole file (§4.6.1). Compared by value.
pub type Fingerprint = [u8; 32];

/// The one algorithm this build produces and understands (§4.6.1: fingerprints are
/// stored as an algorithm+value PAIR). Matches the V5 defaults (bom_link_state.fp_algo,
/// bom_link_backup.fp_algo). Any stored fingerprint whose algo differs must be treated
/// as uninterpretable — comparing values across algorithms is meaningless and, in the
/// §4.4.2 restore rule, would fabricate a Trusted transition.
pub const ALGORITHM: &str = "sha256-v1";

/// Interpret a stored (algo, hex) pair as a Fingerprint. Err on an algo this build
/// does not produce and on malformed hex — both fail closed at the call site (`what`
/// names the column for the message).
pub fn parse_stored(algo: &str, hex: &str, what: &str) -> Result<Fingerprint, String> {
    if algo != ALGORITHM {
        return Err(format!(
            "{what}: stored fingerprint algorithm {algo:?} is not supported by this \
             build (expected {ALGORITHM:?}) — values are not comparable"
        ));
    }
    parse_hex(hex).ok_or_else(|| format!("{what} is not a valid {ALGORITHM} fingerprint: {hex:?}"))
}

/// SHA-256 of the whole file, single attempt.
pub fn file_fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let bytes = std::fs::read(path)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    Ok(h.finalize().into())
}

/// Windows sharing-violation family: another process holds the file with a share mode
/// that excludes our read. This is the "not a stable snapshot" case of §4.6.1 — the
/// only error class worth retrying (a writer is mid-save; a moment later the read may
/// succeed). Everything else (not found, permission, ...) propagates immediately.
fn is_sharing_violation(e: &io::Error) -> bool {
    // 32 = ERROR_SHARING_VIOLATION, 33 = ERROR_LOCK_VIOLATION
    matches!(e.raw_os_error(), Some(32) | Some(33))
}

/// SHA-256 with retries limited to sharing violations (§4.6.1). `retries` is the
/// number of ADDITIONAL attempts after the first; `backoff` is slept between attempts.
/// All-retries-exhausted returns the last error — the caller treats that as "no stable
/// snapshot" and aborts its write (fail closed), it must not fall back to mtime/size.
pub fn stable_fingerprint(path: &Path, retries: u32, backoff: Duration) -> io::Result<Fingerprint> {
    let mut last;
    match file_fingerprint(path) {
        Ok(fp) => return Ok(fp),
        Err(e) if is_sharing_violation(&e) => last = e,
        Err(e) => return Err(e),
    }
    for _ in 0..retries {
        std::thread::sleep(backoff);
        match file_fingerprint(path) {
            Ok(fp) => return Ok(fp),
            Err(e) if is_sharing_violation(&e) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Lower-case hex form — the representation stored in the V5 TEXT columns
/// (bom_link_state.last_read_fp / last_app_write_fp / ..., bom_link_backup.*_fp).
pub fn to_hex(fp: &Fingerprint) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse the hex form back. None on anything malformed (wrong length, non-hex) so the
/// caller can fail closed — a corrupt stored fingerprint must not silently compare
/// unequal-to-everything or equal-to-anything.
pub fn parse_hex(s: &str) -> Option<Fingerprint> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut fp = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        fp[i] = ((hi << 4) | lo) as u8;
    }
    Some(fp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors() {
        let dir = std::env::temp_dir();
        let empty = dir.join("mbm-fp-empty.bin");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            to_hex(&file_fingerprint(&empty).unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let abc = dir.join("mbm-fp-abc.bin");
        std::fs::write(&abc, b"abc").unwrap();
        assert_eq!(
            to_hex(&file_fingerprint(&abc).unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&empty).ok();
        std::fs::remove_file(&abc).ok();
    }

    #[test]
    fn hex_roundtrip_and_rejects_malformed() {
        let fp: Fingerprint = [0xAB; 32];
        let hex = to_hex(&fp);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex(&hex), Some(fp));

        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex("abc"), None); // wrong length
        assert_eq!(parse_hex(&"g".repeat(64)), None); // non-hex
        assert_eq!(parse_hex(&"a".repeat(63)), None); // odd length
        assert_eq!(parse_hex(&"a".repeat(66)), None); // too long
    }

    #[test]
    fn parse_stored_requires_matching_algorithm() {
        let hex = to_hex(&[7u8; 32]);
        assert!(parse_stored(ALGORITHM, &hex, "test_col").is_ok());
        // Well-formed hex under a foreign algo is uninterpretable, not comparable.
        assert!(parse_stored("sha256-v2", &hex, "test_col").is_err());
        // And a matching algo still rejects malformed hex.
        assert!(parse_stored(ALGORITHM, "not-hex", "test_col").is_err());
    }

    #[test]
    fn missing_file_propagates_immediately() {
        // NotFound is not a sharing violation: no retries, error comes straight back.
        let missing = std::env::temp_dir().join("mbm-fp-does-not-exist.bin");
        let start = std::time::Instant::now();
        let err = stable_fingerprint(&missing, 5, Duration::from_secs(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(start.elapsed() < Duration::from_millis(500)); // did not sleep 5x1s
    }

    /// Real sharing violation (§4.6.1): another handle holds the file with share_mode 0
    /// (no sharing at all), so our read-open fails with ERROR_SHARING_VIOLATION and the
    /// retry loop runs to exhaustion. Windows-only by nature; CI runs on Windows.
    #[cfg(windows)]
    #[test]
    fn sharing_violation_is_retried_then_fails_closed() {
        use std::os::windows::fs::OpenOptionsExt;
        let path = std::env::temp_dir().join("mbm-fp-locked.bin");
        std::fs::write(&path, b"locked").unwrap();
        let _excl = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0) // deny all sharing while this handle lives
            .open(&path)
            .unwrap();

        let start = std::time::Instant::now();
        let err = stable_fingerprint(&path, 2, Duration::from_millis(50)).unwrap_err();
        assert_eq!(err.raw_os_error(), Some(32)); // ERROR_SHARING_VIOLATION
        assert!(start.elapsed() >= Duration::from_millis(100)); // 2 backoffs happened

        drop(_excl);
        // Once the writer releases the handle, the same call succeeds.
        assert!(stable_fingerprint(&path, 0, Duration::ZERO).is_ok());
        std::fs::remove_file(&path).ok();
    }
}
