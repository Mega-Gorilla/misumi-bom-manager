// Link-time environment decision (plan.md §4.10, step 3b) + real-path resolution
// (implementation.md §2.1 — the 7-step fail-closed order).
//
// Three layers: a pure decision core (testable on any OS), thin Windows API
// providers, and `check_env` which strings the 7 steps together and folds EVERY
// failure into NoWriteback + reason (nothing but a panic can escape fail closed).
//
// The PoC's LinkDecision is unified into model::EnvVerdict (Allow ⇔ Allow,
// WarnNoWriteBack ⇔ NoWriteback). Symlinks stay refused until the 3b measurement
// completes (§3.11); junctions are measured and allowed to proceed to canonicalize.

use crate::model::EnvVerdict;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

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

/// Result of the full environment check. Two SEPARATE concerns (review R3):
/// - `read_target`: the file actually read (the .lnk chain followed and
///   canonicalized, symlinks and all — §4.10 gates WRITEBACK only, reading
///   continues in a NoWriteback environment)
/// - `resolved_path` + `verdict` + `fs_name`: the WRITEBACK verification (reparse
///   walk + real FS); `resolved_path` is only Some when verification reached
///   canonicalize
///
/// `reason` is NOT persisted (no V5 column — display-only, regenerated per check).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvCheck {
    pub verdict: EnvVerdict,
    pub resolved_path: Option<String>,
    pub fs_name: Option<String>,
    /// Canonical path all file IO must use; None = nothing readable exists
    /// (missing target, broken/cyclic .lnk).
    pub read_target: Option<String>,
    /// Always Some("env:<code>: <detail>") when NoWriteback, always None when Allow.
    pub reason: Option<String>,
}

impl EnvCheck {
    fn refused(reason: String) -> Self {
        EnvCheck {
            verdict: EnvVerdict::NoWriteback,
            resolved_path: None,
            fs_name: None,
            read_target: None,
            reason: Some(reason),
        }
    }
}

// ---- pure core ----------------------------------------------------------------------

/// Step (4)(5)(6) policy for one reparse tag. Allowlist: everything that is not the
/// measured junction tag or the (deliberately refused) symlink tag is unknown and
/// refused — cloud placeholders and future tags land there by construction.
pub(crate) enum TagPolicy {
    /// (5) IO_REPARSE_TAG_MOUNT_POINT — junction, measured in 3b: keep walking.
    Continue,
    /// (4) IO_REPARSE_TAG_SYMLINK — refused until the 3b measurement completes.
    RefuseSymlink,
    /// (6) anything else — fail closed.
    RefuseUnknown,
}

const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

pub(crate) fn classify_tag(tag: u32) -> TagPolicy {
    match tag {
        IO_REPARSE_TAG_MOUNT_POINT => TagPolicy::Continue,
        IO_REPARSE_TAG_SYMLINK => TagPolicy::RefuseSymlink,
        _ => TagPolicy::RefuseUnknown,
    }
}

/// Judge a walked sequence of (component path, reparse tag if any).
pub(crate) fn judge_walk<'a>(
    observed: impl IntoIterator<Item = (&'a Path, Option<u32>)>,
) -> Result<(), String> {
    for (component, tag) in observed {
        if let Some(tag) = tag {
            match classify_tag(tag) {
                TagPolicy::Continue => {}
                TagPolicy::RefuseSymlink => {
                    return Err(format!("env:symlink: {}", component.display()));
                }
                TagPolicy::RefuseUnknown => {
                    return Err(format!(
                        "env:reparse_unknown: tag=0x{tag:08X} at {}",
                        component.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) const MAX_LNK_DEPTH: usize = 8;

/// Steps (1)(2)(3): check the input's components, then follow .lnk hops — checking
/// every hop's components BEFORE canonicalize (a .lnk pointing under a symlink must
/// not slip through) — with cycle detection and a depth bound.
pub(crate) fn resolve_lnk_chain(
    input: &Path,
    resolve_one: &dyn Fn(&Path) -> Result<PathBuf, String>,
    check_components: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let is_lnk = |p: &Path| {
        p.extension()
            .map(|e| e.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
    };
    let norm = |p: &Path| p.to_string_lossy().to_lowercase();

    check_components(input)?; // step (1)
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(norm(input));
    let mut current = input.to_path_buf();
    let mut depth = 0usize;
    while is_lnk(&current) {
        depth += 1;
        if depth > MAX_LNK_DEPTH {
            return Err(format!("env:lnk_depth: exceeded {MAX_LNK_DEPTH}"));
        }
        let mut target = resolve_one(&current)?; // step (2)
        if target.as_os_str().is_empty() {
            return Err(format!(
                "env:lnk_unresolvable: no file target in {}",
                current.display()
            ));
        }
        if target.is_relative() {
            if let Some(parent) = current.parent() {
                target = parent.join(target);
            }
        }
        if !visited.insert(norm(&target)) {
            return Err(format!("env:lnk_cycle: {}", target.display()));
        }
        check_components(&target)?; // step (3)
        current = target;
    }
    Ok(current)
}

/// Strip the canonicalize() verbatim prefixes for display/API use (PoC-measured:
/// `\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share\x` → `\\srv\share\x`).
pub(crate) fn strip_verbatim(p: &Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.into_owned()
    }
}

// ---- Windows providers --------------------------------------------------------------

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstFileW, GetFileAttributesW, GetVolumeInformationW, GetVolumePathNameW,
        FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW,
    };

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().chars_lossy_to_wide()
    }

    // Small helper because OsStr has no direct wide+NUL conversion method name.
    trait ToWide {
        fn chars_lossy_to_wide(&self) -> Vec<u16>;
    }
    impl ToWide for std::ffi::OsStr {
        fn chars_lossy_to_wide(&self) -> Vec<u16> {
            self.encode_wide().chain(std::iter::once(0)).collect()
        }
    }

    /// Reparse tag of one existing path component. Ok(None) = not a reparse point.
    pub(super) fn reparse_tag_of(path: &Path) -> Result<Option<u32>, String> {
        let w = wide(path);
        let attrs = unsafe { GetFileAttributesW(w.as_ptr()) };
        if attrs == INVALID_FILE_ATTRIBUTES {
            return Err(format!("env:not_found: {}", path.display()));
        }
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            return Ok(None);
        }
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let h = unsafe { FindFirstFileW(w.as_ptr(), &mut data) };
        if h == INVALID_HANDLE_VALUE {
            return Err(format!("env:not_found: {}", path.display()));
        }
        unsafe { FindClose(h) };
        Ok(Some(data.dwReserved0))
    }

    /// Walk every component of `path` (skipping the drive prefix/root, which cannot
    /// be a reparse point) including the final element, feeding tags to judge_walk.
    pub(super) fn walk_check_components(path: &Path) -> Result<(), String> {
        let abs = std::path::absolute(path).map_err(|e| format!("env:bad_path: {e}"))?;
        let mut acc = PathBuf::new();
        let mut observed: Vec<(PathBuf, Option<u32>)> = Vec::new();
        for comp in abs.components() {
            use std::path::Component::*;
            match comp {
                Prefix(_) | RootDir => acc.push(comp.as_os_str()),
                _ => {
                    acc.push(comp.as_os_str());
                    observed.push((acc.clone(), reparse_tag_of(&acc)?));
                }
            }
        }
        super::judge_walk(observed.iter().map(|(p, t)| (p.as_path(), *t)))
    }

    /// Resolve one .lnk on a fresh thread (fresh COM apartment: RPC_E_CHANGED_MODE
    /// cannot occur, and the guard may unconditionally CoUninitialize).
    pub(super) fn resolve_lnk_com(lnk: &Path) -> Result<PathBuf, String> {
        let lnk = lnk.to_path_buf();
        std::thread::spawn(move || resolve_on_fresh_thread(&lnk))
            .join()
            .map_err(|_| "env:lnk_unresolvable: resolver thread panicked".to_string())?
    }

    fn resolve_on_fresh_thread(lnk: &Path) -> Result<PathBuf, String> {
        use windows::core::Interface;
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

        struct ComGuard;
        impl Drop for ComGuard {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                .ok()
                .map_err(|e| format!("env:lnk_unresolvable: CoInitializeEx: {e}"))?;
            let _guard = ComGuard;
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("env:lnk_unresolvable: CoCreateInstance: {e}"))?;
            let pf: IPersistFile = link
                .cast()
                .map_err(|e| format!("env:lnk_unresolvable: IPersistFile: {e}"))?;
            let wide: Vec<u16> = lnk
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            pf.Load(windows::core::PCWSTR(wide.as_ptr()), STGM_READ)
                .map_err(|e| format!("env:lnk_unresolvable: Load: {e}"))?;
            let mut buf = [0u16; 32768];
            let mut fd = std::mem::zeroed();
            // Do NOT call Resolve(): the PoC-measured behaviour (WScript.Shell
            // TargetPath) is the raw stored target; search/relink UI is unwanted.
            link.GetPath(&mut buf, &mut fd, 0)
                .map_err(|e| format!("env:lnk_unresolvable: GetPath: {e}"))?;
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let target = String::from_utf16_lossy(&buf[..len]);
            if target.is_empty() {
                return Err("env:lnk_unresolvable: no file target".into());
            }
            Ok(PathBuf::from(target))
        }
    }

    /// FS name of the volume holding the RESOLVED path (never the doorway drive).
    pub(super) fn fs_name_of(resolved_display: &str) -> Result<String, String> {
        let path_w: Vec<u16> = std::ffi::OsStr::new(resolved_display)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut root = [0u16; 512];
        let ok = unsafe { GetVolumePathNameW(path_w.as_ptr(), root.as_mut_ptr(), 512) };
        if ok == 0 {
            return Err(format!(
                "env:fs_query_failed: GetVolumePathNameW {resolved_display}"
            ));
        }
        let mut fs = [0u16; 262];
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                262,
            )
        };
        if ok == 0 {
            return Err(format!(
                "env:fs_query_failed: GetVolumeInformationW {resolved_display}"
            ));
        }
        let len = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
        Ok(String::from_utf16_lossy(&fs[..len]))
    }
}

// ---- orchestration ------------------------------------------------------------------

/// The full §4.10 environment check. Read-side discovery and write-side
/// verification are separate (review R3): the read target follows the .lnk chain
/// WITHOUT reparse gating (reading may legitimately cross symlinks — §4.10 gates
/// writeback only), while the 7-step verification decides the writeback verdict.
/// Never panics on bad input; every verification failure folds into
/// NoWriteback + reason, with the read target retained when one exists.
#[cfg(windows)]
pub fn check_env(input: &Path) -> EnvCheck {
    let abs = match std::path::absolute(input) {
        Ok(p) => p,
        Err(e) => return EnvCheck::refused(format!("env:bad_path: {e}")),
    };

    // ---- read-side discovery: what would we actually read? ----
    let read_target = resolve_lnk_chain(&abs, &win::resolve_lnk_com, &|_| Ok(()))
        .ok()
        .and_then(|f| std::fs::canonicalize(f).ok())
        .map(|p| strip_verbatim(&p));

    // ---- write-side verification: steps (1)..(6) ----
    let resolved = match resolve_lnk_chain(&abs, &win::resolve_lnk_com, &win::walk_check_components)
    {
        Ok(p) => p,
        Err(reason) => {
            return EnvCheck {
                read_target,
                ..EnvCheck::refused(reason)
            }
        }
    };
    // Step (7).
    let real = match std::fs::canonicalize(&resolved) {
        Ok(p) => p,
        Err(e) => {
            return EnvCheck {
                read_target,
                ..EnvCheck::refused(format!(
                    "env:canonicalize_failed: {}: {e}",
                    resolved.display()
                ))
            }
        }
    };
    let display = strip_verbatim(&real);
    let fs = match win::fs_name_of(&display) {
        Ok(fs) => fs,
        Err(reason) => {
            return EnvCheck {
                verdict: EnvVerdict::NoWriteback,
                resolved_path: Some(display),
                fs_name: None,
                read_target,
                reason: Some(reason),
            }
        }
    };
    let verdict = link_allowed(&fs, true);
    EnvCheck {
        verdict,
        resolved_path: Some(display),
        reason: match verdict {
            EnvVerdict::Allow => None,
            EnvVerdict::NoWriteback => Some(format!("env:fs_not_ntfs: {fs}")),
        },
        fs_name: Some(fs),
        read_target,
    }
}

#[cfg(not(windows))]
pub fn check_env(_input: &Path) -> EnvCheck {
    EnvCheck::refused("env:unsupported_os: environment checks are Windows-only".into())
}

/// Test-only .lnk writer via the same COM family the resolver reads with
/// (SetPath + IPersistFile::Save on a fresh-apartment thread). Shared with the
/// orchestration tests in mod.rs (review R2: .lnk end-to-end flows).
#[cfg(all(test, windows))]
pub(crate) fn write_lnk_for_tests(lnk: &Path, target: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::Interface;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let lnk = lnk.to_path_buf();
    let target = target.to_path_buf();
    std::thread::spawn(move || unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
            .ok()
            .unwrap();
        struct G;
        impl Drop for G {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _g = G;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
        let tw: Vec<u16> = target
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        link.SetPath(windows::core::PCWSTR(tw.as_ptr())).unwrap();
        let pf: IPersistFile = link.cast().unwrap();
        let lw: Vec<u16> = lnk
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        pf.Save(windows::core::PCWSTR(lw.as_ptr()), true).unwrap();
    })
    .join()
    .unwrap();
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

    // ---- pure core: tag policy / walk judgement ----

    #[test]
    fn tag_allowlist_fails_closed() {
        assert!(matches!(classify_tag(0xA000_0003), TagPolicy::Continue)); // junction
        assert!(matches!(
            classify_tag(0xA000_000C),
            TagPolicy::RefuseSymlink
        ));
        // Cloud placeholder family and arbitrary tags are unknown → refused.
        for tag in [0x9000_001Au32, 0x8000_0017, 0xDEAD_BEEF] {
            assert!(matches!(classify_tag(tag), TagPolicy::RefuseUnknown));
        }
    }

    #[test]
    fn walk_judgement_reports_the_offending_component() {
        let a = Path::new(r"C:\x");
        let b = Path::new(r"C:\x\link");
        // Junction mid-path: fine.
        assert!(judge_walk(vec![(a, None), (b, Some(0xA000_0003))]).is_ok());
        // Symlink mid-path: refused with the component in the reason.
        let err = judge_walk(vec![(a, None), (b, Some(0xA000_000C))]).unwrap_err();
        assert!(err.starts_with("env:symlink:") && err.contains("link"));
        // Unknown tag: refused with the hex tag.
        let err = judge_walk(vec![(b, Some(0x9000_001A))]).unwrap_err();
        assert!(err.contains("env:reparse_unknown") && err.contains("0x9000001A"));
    }

    // ---- pure core: .lnk chain (fake resolver / fake component check) ----

    fn fake_resolver(map: Vec<(&str, &str)>) -> impl Fn(&Path) -> Result<PathBuf, String> {
        let map: Vec<(String, String)> = map
            .into_iter()
            .map(|(a, b)| (a.to_lowercase(), b.to_string()))
            .collect();
        move |p: &Path| {
            let key = p.to_string_lossy().to_lowercase();
            map.iter()
                .find(|(a, _)| *a == key)
                .map(|(_, b)| PathBuf::from(b))
                .ok_or_else(|| format!("env:lnk_unresolvable: {key}"))
        }
    }

    #[test]
    fn lnk_cycle_is_detected() {
        let resolver = fake_resolver(vec![(r"c:\a.lnk", r"C:\b.lnk"), (r"c:\b.lnk", r"C:\a.lnk")]);
        let err = resolve_lnk_chain(Path::new(r"C:\a.lnk"), &resolver, &|_| Ok(())).unwrap_err();
        assert!(err.starts_with("env:lnk_cycle:"), "{err}");
    }

    #[test]
    fn lnk_depth_is_bounded() {
        // 1.lnk -> 2.lnk -> ... -> 10.lnk: exceeds MAX_LNK_DEPTH.
        let chain: Vec<(String, String)> = (1..=10)
            .map(|i| (format!(r"c:\{i}.lnk"), format!(r"C:\{}.lnk", i + 1)))
            .collect();
        let chain_ref: Vec<(&str, &str)> = chain
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let resolver = fake_resolver(chain_ref);
        let err = resolve_lnk_chain(Path::new(r"C:\1.lnk"), &resolver, &|_| Ok(())).unwrap_err();
        assert!(err.starts_with("env:lnk_depth:"), "{err}");
    }

    #[test]
    fn lnk_target_components_are_checked_before_acceptance() {
        // Step (3): a .lnk pointing under a symlink must be refused even though the
        // .lnk itself sits on a clean path.
        let resolver = fake_resolver(vec![(r"c:\clean\doc.lnk", r"C:\via-symlink\bom.xlsx")]);
        let check = |p: &Path| {
            if p.to_string_lossy().to_lowercase().contains("via-symlink") {
                Err(format!("env:symlink: {}", p.display()))
            } else {
                Ok(())
            }
        };
        let err = resolve_lnk_chain(Path::new(r"C:\clean\doc.lnk"), &resolver, &check).unwrap_err();
        assert!(err.starts_with("env:symlink:"), "{err}");
        // Non-.lnk input passes straight through (components already checked).
        let ok = resolve_lnk_chain(Path::new(r"C:\clean\bom.xlsx"), &resolver, &check).unwrap();
        assert_eq!(ok, PathBuf::from(r"C:\clean\bom.xlsx"));
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\C:\x\y.xlsx")),
            r"C:\x\y.xlsx"
        );
        assert_eq!(
            strip_verbatim(Path::new(r"\\?\UNC\srv\share\y.xlsx")),
            r"\\srv\share\y.xlsx"
        );
        assert_eq!(strip_verbatim(Path::new(r"C:\plain")), r"C:\plain");
    }

    // ---- Windows integration (real FS) ----

    #[cfg(windows)]
    mod win_integration {
        use super::super::*;
        use std::process::Command;

        fn temp_root(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("mbm-env-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        }

        use super::super::write_lnk_for_tests as write_lnk;

        #[test]
        fn plain_ntfs_file_is_allowed() {
            let dir = temp_root("plain");
            let file = dir.join("bom.xlsx");
            std::fs::write(&file, b"x").unwrap();
            let r = check_env(&file);
            assert_eq!(r.verdict, EnvVerdict::Allow, "{:?}", r.reason);
            assert_eq!(r.fs_name.as_deref(), Some("NTFS"));
            assert!(r.reason.is_none());
            assert!(r.resolved_path.unwrap().ends_with("bom.xlsx"));
        }

        #[test]
        fn missing_file_fails_closed() {
            let r = check_env(Path::new(r"C:\mbm-does-not-exist\bom.xlsx"));
            assert_eq!(r.verdict, EnvVerdict::NoWriteback);
            assert!(r.reason.unwrap().starts_with("env:not_found:"));
        }

        #[test]
        fn junction_resolves_and_is_allowed() {
            let dir = temp_root("junc");
            let real = dir.join("real");
            std::fs::create_dir_all(&real).unwrap();
            let file = real.join("bom.xlsx");
            std::fs::write(&file, b"x").unwrap();
            let junc = dir.join("junc");
            let status = Command::new("cmd")
                .args([
                    "/c",
                    "mklink",
                    "/J",
                    &junc.to_string_lossy(),
                    &real.to_string_lossy(),
                ])
                .status()
                .unwrap();
            assert!(status.success(), "mklink /J failed");
            let r = check_env(&junc.join("bom.xlsx"));
            assert_eq!(r.verdict, EnvVerdict::Allow, "{:?}", r.reason);
            // canonicalize collapsed the junction onto the real location.
            assert!(r
                .resolved_path
                .unwrap()
                .to_lowercase()
                .contains(&real.file_name().unwrap().to_string_lossy().to_lowercase()));
            let _ = std::fs::remove_dir(&junc);
        }

        #[test]
        fn real_lnk_to_plain_file_is_allowed_and_resolved() {
            let dir = temp_root("lnk");
            let file = dir.join("bom.xlsx");
            std::fs::write(&file, b"x").unwrap();
            let lnk = dir.join("bom.lnk");
            write_lnk(&lnk, &file);
            let r = check_env(&lnk);
            assert_eq!(r.verdict, EnvVerdict::Allow, "{:?}", r.reason);
            assert!(r.resolved_path.unwrap().ends_with("bom.xlsx"));
        }

        #[test]
        fn real_lnk_cycle_fails_closed() {
            let dir = temp_root("lnkcycle");
            let a = dir.join("a.lnk");
            let b = dir.join("b.lnk");
            // IShellLink::Save fails on a nonexistent target path — seed both
            // files first, then overwrite them with the cyclic links.
            std::fs::write(&a, b"").unwrap();
            std::fs::write(&b, b"").unwrap();
            write_lnk(&a, &b);
            write_lnk(&b, &a);
            let r = check_env(&a);
            assert_eq!(r.verdict, EnvVerdict::NoWriteback);
            // Measured shell behaviour: GetPath renders a .lnk-to-.lnk target as an
            // EMPTY path, so a real cycle terminates as lnk_unresolvable — itself
            // fail closed. The cycle/depth loop logic is pinned by the pure fake
            // tests above; this test pins that a real cycle can never be Allowed.
            let reason = r.reason.unwrap();
            assert!(reason.starts_with("env:lnk_"), "{reason}");
        }

        fn try_symlink_dir(src: &Path, dst: &Path) -> bool {
            match std::os::windows::fs::symlink_dir(src, dst) {
                Ok(()) => true,
                Err(e) if e.raw_os_error() == Some(1314) => {
                    if std::env::var_os("EXCEL_LINK_REQUIRE_SYMLINK").is_some() {
                        panic!("symlink required by EXCEL_LINK_REQUIRE_SYMLINK but unavailable");
                    }
                    eprintln!(
                        "SKIP(symlink): ERROR_PRIVILEGE_NOT_HELD — fail-closed is covered by the \
                         classify_tag pure tests"
                    );
                    false
                }
                Err(e) => panic!("unexpected: {e}"),
            }
        }

        #[test]
        fn symlink_parent_fails_closed_when_creatable() {
            let dir = temp_root("symlink");
            let real = dir.join("real");
            std::fs::create_dir_all(&real).unwrap();
            std::fs::write(real.join("bom.xlsx"), b"x").unwrap();
            let sym = dir.join("sym");
            if !try_symlink_dir(&real, &sym) {
                return; // recorded skip
            }
            let r = check_env(&sym.join("bom.xlsx"));
            assert_eq!(r.verdict, EnvVerdict::NoWriteback);
            assert!(r.reason.unwrap().starts_with("env:symlink:"));
            let _ = std::fs::remove_dir(&sym);
        }
    }
}
