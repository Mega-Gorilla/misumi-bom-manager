# Environment-decision PoC for step 3b (plan.md §4.10).
#
# Question: without hardcoding drive letters, can we resolve a user-chosen path to its REAL
# location + file system, and reach the same decision through a junction, a symlink and a .lnk
# shortcut? Decision authority lives in Rust (`resolve` for canonicalization, `decide` for the
# §4.10 verdict) — this script only builds the test environment and gathers the FS name.
#
# Drive is READ-ONLY here: we resolve H:\マイドライブ.lnk and judge, but never write to it.
# A symlink needs developer mode/admin; if creation fails we record "untested", not a failure.
#
# Usage: pwsh -File check-env.ps1

$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$bin = Join-Path $dir 'target\debug\excel-link-poc.exe'
& cargo build --quiet 2>$null

$envRoot = Join-Path $dir 'out\env-test'
Remove-Item $envRoot -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $envRoot 'real') | Out-Null
$realFile = Join-Path $envRoot 'real\bom.xlsx'
'dummy' | Set-Content $realFile -NoNewline

function RealOf($p) {
    $out = & $bin resolve $p 2>$null | Where-Object { $_ -match '^\s*real\s*:' }
    if ($out) { ($out -split ':', 2)[1].Trim() } else { $null }
}
function FsOf($p) {
    # FS of the volume holding the RESOLVED path (never the doorway drive letter).
    $qualifier = [System.IO.Path]::GetPathRoot(($p -replace '^\\\\\?\\', ''))
    # Win32_LogicalDisk also covers virtual doorway drives that Get-Volume cannot see.
    $letter = $qualifier[0]
    (Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='${letter}:'").FileSystem
}
function Decide($fs, $resolved) {
    # Exit code is the authority (0 = Allow); discard stdout so only ONE value is returned.
    & $bin decide $fs $resolved *> $null
    if ($LASTEXITCODE -eq 0) { 'Allow' } else { 'WarnNoWriteBack' }
}
$failures = 0
function AssertEq($label, $actual, $expected) {
    if ("$actual" -ne "$expected") {
        Write-Output "   [FAIL] $label — expected $expected, got $actual"
        $script:failures++
    } else {
        Write-Output "   [ ok ] $label = $actual"
    }
}

Write-Output '== step 3b: environment decision (§4.10) =='
$realResolved = RealOf $realFile
Write-Output "   real file resolves to: $realResolved"

# ---- junction (no privilege needed) ----
$junc = Join-Path $envRoot 'junc'
cmd /c "mklink /J `"$junc`" `"$(Join-Path $envRoot 'real')`"" | Out-Null
$viaJunc = RealOf (Join-Path $junc 'bom.xlsx')
AssertEq 'junction resolves to the same real path' $viaJunc $realResolved

# ---- symlink (may need developer mode) ----
$sym = Join-Path $envRoot 'sym'
$symOk = $true
try {
    cmd /c "mklink /D `"$sym`" `"$(Join-Path $envRoot 'real')`"" 2>&1 | Out-Null
    if (-not (Test-Path (Join-Path $sym 'bom.xlsx'))) { throw 'symlink not usable' }
} catch { $symOk = $false }
if ($symOk) {
    $viaSym = RealOf (Join-Path $sym 'bom.xlsx')
    AssertEq 'symlink resolves to the same real path' $viaSym $realResolved
} else {
    Write-Output '   [untested] symlink (developer mode/admin required) — recorded, not failed'
}

# ---- .lnk shortcut (resolved by shell, then fed into the same pipeline) ----
$lnk = Join-Path $envRoot 'bom.lnk'
$sh = New-Object -ComObject WScript.Shell
$sc = $sh.CreateShortcut($lnk); $sc.TargetPath = $realFile; $sc.Save()
$lnkTarget = $sh.CreateShortcut($lnk).TargetPath
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($sh) | Out-Null
$viaLnk = RealOf $lnkTarget
AssertEq '.lnk target resolves to the same real path' $viaLnk $realResolved

# ---- decision on the local NTFS real path ----
$fs = FsOf $realResolved
Write-Output "   file system of resolved volume: $fs"
AssertEq 'local NTFS real path -> Allow' (Decide $fs 'true') 'Allow'

# ---- fail-closed decisions ----
AssertEq 'unresolved path -> WarnNoWriteBack' (Decide $fs 'false') 'WarnNoWriteBack'
AssertEq 'FAT32 (virtual doorway) -> WarnNoWriteBack' (Decide 'FAT32' 'true') 'WarnNoWriteBack'

# ---- Google Drive doorway, READ-ONLY ----
Write-Output ''
Write-Output '== Google Drive doorway (read-only) =='
$driveLnk = 'H:\マイドライブ.lnk'
if (Test-Path $driveLnk) {
    $sh2 = New-Object -ComObject WScript.Shell
    $target = $sh2.CreateShortcut($driveLnk).TargetPath
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($sh2) | Out-Null
    Write-Output "   H:\マイドライブ.lnk -> $target"
    $resolved = RealOf $target
    if ($resolved) {
        $dfs = FsOf $resolved
        Write-Output "   resolved: $resolved  (FS: $dfs)"
        AssertEq 'Drive mirror real path decision' (Decide $dfs 'true') $(if ($dfs -eq 'NTFS') { 'Allow' } else { 'WarnNoWriteBack' })
    } else {
        Write-Output '   [untested] mirror target not resolvable on this machine'
    }
    # The doorway drive itself reports FAT32 (§3.6) — must be refused.
    $doorFs = (Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='H:'").FileSystem
    Write-Output "   doorway H:\ reports FS: $doorFs"
    AssertEq 'doorway (virtual FS) -> WarnNoWriteBack' (Decide $doorFs 'true') $(if ($doorFs -eq 'NTFS') { 'Allow' } else { 'WarnNoWriteBack' })
} else {
    Write-Output '   [untested] H:\マイドライブ.lnk not present on this machine'
}

Write-Output ''
if ($failures -eq 0) {
    Write-Output '== ENV DECISION OK: all paths resolve to one real location; decisions fail closed =='
} else {
    Write-Error "$failures assertion(s) failed"
    exit 1
}