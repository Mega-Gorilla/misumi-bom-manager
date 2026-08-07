// Link-time environment decision (plan.md §4.10, verified in step 3b).
//
// Ported from tools/excel-link-poc/src/state.rs `link_allowed`, with the PoC's
// `LinkDecision` unified into `model::EnvVerdict` (the V5/IPC vocabulary from PR-1) —
// Allow ⇔ the old Allow, NoWriteback ⇔ the old WarnNoWriteBack.
//
// This module holds only the pure decision. The real-path resolution feeding it
// (canonicalize, .lnk chains, the 7-step reparse walk of implementation.md §2.1) is
// PR-3 scope; symlink handling stays fail closed until the 3b measurement completes.

use crate::model::EnvVerdict;

/// §4.10 as a pure function: never hardcode drive letters; judge by the RESOLVED
/// location's file system. `resolved=false` means canonicalize failed or the .lnk
/// chain could not be followed — fail closed.
pub fn link_allowed(fs_name: &str, resolved: bool) -> EnvVerdict {
    if resolved && fs_name.eq_ignore_ascii_case("NTFS") {
        EnvVerdict::Allow
    } else {
        EnvVerdict::NoWriteback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntfs_resolved_is_allowed() {
        assert_eq!(link_allowed("NTFS", true), EnvVerdict::Allow);
        assert_eq!(link_allowed("ntfs", true), EnvVerdict::Allow); // case-insensitive
    }

    #[test]
    fn everything_else_fails_closed() {
        // The Drive entry drive reports FAT32 (§3.6) — that is a virtual doorway, not a
        // place to write a master BOM.
        assert_eq!(link_allowed("FAT32", true), EnvVerdict::NoWriteback);
        assert_eq!(link_allowed("NTFS", false), EnvVerdict::NoWriteback);
        assert_eq!(link_allowed("", true), EnvVerdict::NoWriteback);
        assert_eq!(link_allowed("ReFS", true), EnvVerdict::NoWriteback);
    }
}
