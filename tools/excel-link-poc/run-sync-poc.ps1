# Step-3c sync-PoC orchestrator (Issue #23, plan.md §6.2).
#
# Non-interactive by design: every barrier in the Issue #23 timing diagram is a SEPARATE
# invocation (-Case NN -Step N), so timing is controlled by when the operator runs the next
# step, not by sleeps. State between steps lives in <case>/state.json; every observation is
# appended to <case>/log.jsonl. Verdicts come from markers + fingerprints (exit codes), never
# from tray icons.
#
# Usage:
#   pwsh -File run-sync-poc.ps1 -Init [-DryRun]      # build root (+sentinel) inside the mirror
#   pwsh -File run-sync-poc.ps1 -Case 01 -Step 1     # run one barrier-delimited step
#   pwsh -File run-sync-poc.ps1 -Case 02 -Simulate   # DRY-RUN ONLY: fake the endpoint-B edit
#   pwsh -File run-sync-poc.ps1 -Scan                # list files new since -Init (conflict copies)
#   pwsh -File run-sync-poc.ps1 -Cleanup             # delete the tree via sync-guard (5 checks)
#
# Cases: 01 normal / 02 remote-first / 03 before-replace / 04 remote-after-replace /
#        05a,05b pause-both / 06 offline-return / 07 conflict-copy / 08 backup-sync /
#        91,92 presence-ON re-runs. See sync-poc-runbook.md for the endpoint-B actions.

param(
    [switch]$Init,
    [switch]$DryRun,
    [string]$MirrorRoot,   # explicit mirror-root override (else resolved via H:\マイドライブ.lnk)
    [string]$Case,
    [int]$Step,
    [switch]$Simulate,
    [switch]$Scan,
    [switch]$Cleanup
)

$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$bin = Join-Path $dir 'target\debug\excel-link-poc.exe'
$sessionPath = Join-Path $dir 'out\syncpoc-session.json'
$inventoryPath = Join-Path $dir 'out\syncpoc-inventory.json'

function Fail([string]$msg) { Write-Output "[FAIL] $msg"; exit 1 }

function Get-Fp([string]$file) {
    $fp = (& $bin fingerprint $file)
    if ($LASTEXITCODE -ne 0) { Fail "fingerprint failed for $file" }
    $fp.Trim()
}

function RealOf([string]$p) {
    $out = & $bin resolve $p 2>$null | Where-Object { $_ -match '^\s*real\s*:' }
    # Strip the \\?\ long-path prefix: Get-ChildItem -Filter silently matches nothing under it.
    if ($out) { ($out -split ':', 2)[1].Trim() -replace '^\\\\\?\\', '' } else { $null }
}

function Read-Session {
    if (-not (Test-Path $sessionPath)) { Fail 'no session — run -Init first' }
    Get-Content $sessionPath -Raw | ConvertFrom-Json
}

# Re-derive the expected mirror root INDEPENDENTLY of session.json (PR #24 re-review finding 2:
# the session file is editable state, never the write/delete authority). DryRun roots come from a
# fixed in-repo path; real roots from -MirrorRoot or a fresh .lnk resolution.
function Get-ExpectedParent($session) {
    if ($session.dryRun) { return (Join-Path $dir 'out\syncpoc-dryrun-root') }
    if ($MirrorRoot) {
        $entry = $MirrorRoot
    } else {
        $lnk = 'H:\マイドライブ.lnk'
        if (-not (Test-Path $lnk)) { Fail "cannot independently resolve the mirror root ($lnk absent) — pass -MirrorRoot" }
        $sh = New-Object -ComObject WScript.Shell
        $entry = $sh.CreateShortcut($lnk).TargetPath
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($sh) | Out-Null
    }
    $p = RealOf $entry
    if (-not $p) { Fail "cannot resolve $entry to a real path" }
    $p
}

# Common preflight before ANY operation that touches session.root: the independently derived
# root must agree with the session, and the sync-guard dry check must pass. Returns the
# independently derived expected parent for use in --delete.
function Invoke-Preflight($session) {
    $expected = Get-ExpectedParent $session
    if ("$expected".ToLower() -ne "$($session.parentRoot)".ToLower()) {
        Fail "session parentRoot '$($session.parentRoot)' != independently resolved root '$expected' — refusing"
    }
    & $bin sync-guard $session.root $expected $session.runId | ForEach-Object { Write-Host "   $_" }
    if ($LASTEXITCODE -ne 0) { Fail 'preflight guard refused — session root is not a valid PoC tree' }
    $expected
}

function Get-CaseDir($session, [string]$c) {
    $hits = @(Get-ChildItem $session.root -Directory -Filter "case-$c-*")
    if ($hits.Count -ne 1) { Fail "case '$c' resolves to $($hits.Count) folders" }
    $hits[0].FullName
}

function Write-CaseLog([string]$caseDir, [hashtable]$fields) {
    $fields['ts'] = (Get-Date).ToString('o')
    $line = $fields | ConvertTo-Json -Compress
    Add-Content -Path (Join-Path $caseDir 'log.jsonl') -Value $line
    # Write-Host, NOT Write-Output: these helpers run inside functions whose RETURN VALUE is
    # checked ('replaced'/'aborted') — output-stream text would pollute that value.
    Write-Host "   log: $line"
}

function Save-CaseState([string]$caseDir, [hashtable]$merge) {
    $p = Join-Path $caseDir 'state.json'
    $cur = @{}
    if (Test-Path $p) {
        (Get-Content $p -Raw | ConvertFrom-Json).PSObject.Properties |
            ForEach-Object { $cur[$_.Name] = $_.Value }
    }
    foreach ($k in $merge.Keys) { $cur[$k] = $merge[$k] }
    $cur | ConvertTo-Json | Set-Content $p
}

function Read-CaseState([string]$caseDir) {
    $p = Join-Path $caseDir 'state.json'
    if (-not (Test-Path $p)) { Fail "no state.json in $caseDir — run step 1 first" }
    Get-Content $p -Raw | ConvertFrom-Json
}

function Log-Markers([string]$caseDir, [string]$file, [string]$label) {
    $m = (& $bin marker $file)
    Write-Host "   $label : $m"
    Write-CaseLog $caseDir @{ event = 'markers'; label = $label; markers = "$m" }
}

# The app write flow with the Issue #23 barrier points. Returns 'replaced', 'aborted',
# 'conflict-detected' (post-replace, backup != F0) or 'stopped' (at a barrier).
function Invoke-AppWrite([string]$caseDir, [switch]$StopAfterRead, [switch]$StopBeforeReplace) {
    $target = Join-Path $caseDir 'bom.xlsx'
    $f0 = Get-Fp $target
    Write-CaseLog $caseDir @{ event = 'after-read-f0'; f0 = $f0 }
    $temp = Join-Path $caseDir 'bom.app-temp.xlsx'
    & $bin rmw $target zip D2 $temp > $null
    if ($LASTEXITCODE -ne 0) { Fail 'rmw temp generation failed' }
    Save-CaseState $caseDir @{ f0 = $f0; temp = $temp }
    if ($StopAfterRead) {
        Write-CaseLog $caseDir @{ event = 'barrier-stop'; barrier = 'after-read-f0' }
        return 'stopped'
    }
    Complete-AppWrite $caseDir -StopBeforeReplace:$StopBeforeReplace
}

function Complete-AppWrite([string]$caseDir, [switch]$StopBeforeReplace) {
    $st = Read-CaseState $caseDir
    $target = Join-Path $caseDir 'bom.xlsx'
    $now = Get-Fp $target
    Write-CaseLog $caseDir @{ event = 'before-final-fingerprint-check'; f0 = $st.f0; current_fp = $now }
    if ($now -ne $st.f0) {
        Write-CaseLog $caseDir @{ event = 'conflict-stop'; reason = 'fingerprint changed since read' }
        Remove-Item $st.temp -Force -ErrorAction SilentlyContinue
        return 'aborted'
    }
    if ($StopBeforeReplace) {
        # PR #24 finding 1: this barrier sits AFTER a passing final check and BEFORE ReplaceFileW —
        # the window plan.md §4.2.2 covers with the post-hoc backup/F0 comparison (case 03).
        Write-CaseLog $caseDir @{ event = 'barrier-stop'; barrier = 'before-replace' }
        return 'stopped'
    }
    Invoke-ReplaceOnly $caseDir
}

# ReplaceFileW + the §4.2.2 POST-HOC conflict detection: in the clean path the displaced backup
# is byte-identical to F0; if it differs, a remote version slipped in after the final check.
# The backup is NEVER auto-deleted — on conflict it is the only copy of the user's version.
function Invoke-ReplaceOnly([string]$caseDir) {
    $st = Read-CaseState $caseDir
    $target = Join-Path $caseDir 'bom.xlsx'
    $backup = Join-Path $caseDir 'bom.backup.xlsx'
    Write-CaseLog $caseDir @{ event = 'before-replace'; f0 = $st.f0 }
    [System.IO.File]::Replace($st.temp, $target, $backup)
    $post = Get-Fp $target
    $backupFp = Get-Fp $backup
    Write-CaseLog $caseDir @{ event = 'after-replace'; target_fp = $post; backup_fp = $backupFp }
    Save-CaseState $caseDir @{ postFp = $post; backupFp = $backupFp }
    Log-Markers $caseDir $target 'post-replace'
    if ($backupFp -ne $st.f0) {
        Write-CaseLog $caseDir @{ event = 'post-replace-conflict'; f0 = $st.f0; backup_fp = $backupFp }
        Log-Markers $caseDir $backup 'displaced-remote-version (preserved in backup)'
        return 'conflict-detected'
    }
    return 'replaced'
}

# Issue #23 §4 timeout rule for pass/fail cases: ONLY exit 0 ("change arrived then settled") may
# proceed. Anything else — including unexpected codes from a broken invocation — is fail closed
# (PR #24 re-review finding 1: a crashed watch must not let the case reach a PASS).
function Assert-WatchArrived([int]$code) {
    switch ($code) {
        0 { return }
        2 { Fail 'INCONCLUSIVE: expected remote change never arrived within the timeout — retry per runbook' }
        3 { Fail 'INCONCLUSIVE: file kept changing and did not settle — retry per runbook' }
        default { Fail "INCONCLUSIVE: watch-stable failed unexpectedly (exit $code)" }
    }
}

# DRY-RUN ONLY: fake "endpoint B saved R1". Faithful to the real scenario: the remote version
# is the Excel-saved R1 twin of the ORIGINAL template (B never saw A's change, and the marker
# is the same G2="R1" string the live session produces — PR #24 recommendation 2).
function Invoke-SimulateRemote($session, [string]$caseDir) {
    if (-not $session.dryRun) { Fail '-Simulate is dry-run only; on Drive the real endpoint B acts' }
    $r1 = Join-Path $dir 'fixtures\syncpoc-remote-r1.xlsx'
    if (-not (Test-Path $r1)) { Fail 'R1 fixture missing — run gen-syncpoc.ps1 first' }
    $target = Join-Path $caseDir 'bom.xlsx'
    Copy-Item $r1 $target -Force
    Write-CaseLog $caseDir @{ event = 'simulated-remote-overwrite'; target_fp = (Get-Fp $target) }
    Log-Markers $caseDir $target 'simulated-remote'
}

function Invoke-Watch($session, [string]$caseDir, [string]$baseline) {
    $target = Join-Path $caseDir 'bom.xlsx'
    $watchArgs = @('watch-stable', $target, $session.watchTimeout, $session.watchStable)
    if ($baseline) { $watchArgs += $baseline }
    & $bin @watchArgs | ForEach-Object { Write-Host "   $_" }
    $code = $LASTEXITCODE
    Write-CaseLog $caseDir @{ event = 'watch-stable'; exit = $code }
    $code
}

function Get-TreeInventory($session) {
    $poc = @(Get-ChildItem $session.root -Recurse -File | ForEach-Object {
            $_.FullName.Substring($session.root.Length + 1) })
    $top = @(Get-ChildItem $session.parentRoot -Force | ForEach-Object { $_.Name })
    @{ pocRoot = $poc; parentTop = $top }
}

# Files the harness itself creates during a run — excluded from the conflict-copy hunt so the
# listing stays readable as cases accumulate (PR #24 re-review). A Drive conflict copy always
# carries a DIFFERENT name (e.g. "bom (1).xlsx"), so filtering exact known names loses nothing.
$harnessArtifacts = @('bom.xlsx', 'bom.backup.xlsx', 'bom.app-temp.xlsx', 'bom.remote-temp.xlsx',
    'log.jsonl', 'state.json')

function Show-NewFiles($session) {
    $before = Get-Content $inventoryPath -Raw | ConvertFrom-Json
    $now = Get-TreeInventory $session
    $newPoc = @($now.pocRoot | Where-Object {
            $before.pocRoot -notcontains $_ -and $harnessArtifacts -notcontains (Split-Path $_ -Leaf) })
    $newTop = @($now.parentTop | Where-Object { $before.parentTop -notcontains $_ })
    Write-Output '-- files new since -Init, harness artifacts excluded (conflict-copy candidates) --'
    if ($newPoc.Count -eq 0 -and $newTop.Count -eq 0) { Write-Output '   (none)' }
    $newPoc | ForEach-Object { Write-Output "   [poc ] $_" }
    $newTop | ForEach-Object { Write-Output "   [top ] $_  <- mirror-root top level" }
    Write-Output '   (Lost & Found and version history: check Drive Web — runbook)'
}

# ---- -Init -------------------------------------------------------------------------------

if ($Init) {
    & cargo build --quiet 2>$null
    # Refuse to orphan an existing run: a still-existing root must be cleaned up (via the guard)
    # before a new session may overwrite session.json (PR #24 finding 3).
    if (Test-Path $sessionPath) {
        $old = Get-Content $sessionPath -Raw | ConvertFrom-Json
        if ($old.root -and (Test-Path $old.root)) {
            Fail "previous session root still exists: $($old.root) — run -Cleanup first"
        }
    }
    $template = Join-Path $dir 'fixtures\syncpoc-template.xlsx'
    if (-not (Test-Path $template)) { Fail 'template missing — run gen-syncpoc.ps1 first' }
    New-Item -ItemType Directory -Force -Path (Join-Path $dir 'out') | Out-Null

    if ($DryRun) {
        $parentRoot = Join-Path $dir 'out\syncpoc-dryrun-root'
        New-Item -ItemType Directory -Force -Path $parentRoot | Out-Null
        $watchTimeout = 20; $watchStable = 3
    } else {
        if ($MirrorRoot) {
            $entry = $MirrorRoot
        } else {
            $lnk = 'H:\マイドライブ.lnk'
            if (-not (Test-Path $lnk)) { Fail "$lnk not found — pass -MirrorRoot or start Drive for desktop" }
            $sh = New-Object -ComObject WScript.Shell
            $entry = $sh.CreateShortcut($lnk).TargetPath
            [System.Runtime.InteropServices.Marshal]::ReleaseComObject($sh) | Out-Null
        }
        $parentRoot = RealOf $entry
        if (-not $parentRoot) { Fail "cannot resolve $entry to a real path" }
        # §4.10 authority check on the RESOLVED volume before writing anything (3b decision).
        $letter = ([System.IO.Path]::GetPathRoot(($parentRoot -replace '^\\\\\?\\', '')))[0]
        $fs = (Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='${letter}:'").FileSystem
        & $bin decide $fs 'true' *> $null
        if ($LASTEXITCODE -ne 0) { Fail "link decision refused for resolved volume FS '$fs' (fail closed)" }
        Write-Output "mirror root resolved: $parentRoot  (FS: $fs, decision: Allow)"
        $watchTimeout = 300; $watchStable = 10
    }

    $runId = (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + (-join ((48..57) + (97..102) | Get-Random -Count 4 | ForEach-Object { [char]$_ }))
    & $bin sync-init $parentRoot $runId $template
    if ($LASTEXITCODE -ne 0) { Fail 'sync-init failed' }
    $root = Join-Path $parentRoot "__mbm_sync_poc_$runId"

    @{ runId = $runId; root = $root; parentRoot = $parentRoot; dryRun = [bool]$DryRun
       template = $template; watchTimeout = $watchTimeout; watchStable = $watchStable
    } | ConvertTo-Json | Set-Content $sessionPath
    $session = Read-Session
    Get-TreeInventory $session | ConvertTo-Json | Set-Content $inventoryPath
    Write-Output "session: $sessionPath (dryRun=$([bool]$DryRun))"
    Write-Output "root   : $root"
    exit 0
}

# ---- -Cleanup / -Scan / -Simulate --------------------------------------------------------

if ($Cleanup) {
    $session = Read-Session
    $expected = Invoke-Preflight $session
    & $bin sync-guard $session.root $expected $session.runId --delete
    if ($LASTEXITCODE -ne 0) { Fail 'sync-guard refused deletion' }
    Write-Output '(session file kept as the run record)'
    exit 0
}

if ($Scan) {
    $session = Read-Session
    Invoke-Preflight $session | Out-Null
    Show-NewFiles $session
    exit 0
}

if ($Simulate) {
    if (-not $Case) { Fail '-Simulate needs -Case' }
    $session = Read-Session
    Invoke-Preflight $session | Out-Null
    Invoke-SimulateRemote $session (Get-CaseDir $session $Case)
    exit 0
}

# ---- -Case NN -Step N --------------------------------------------------------------------

if (-not $Case -or -not $Step) { Fail 'need -Init, -Case NN -Step N, -Simulate, -Scan or -Cleanup' }
$session = Read-Session
Invoke-Preflight $session | Out-Null
$caseDir = Get-CaseDir $session $Case
$target = Join-Path $caseDir 'bom.xlsx'
Write-Output "== case $Case step $Step  ($caseDir) =="

switch ("$Case-$Step") {

    # -- 01 normal (and 91 presence-ON rerun): full write flow, then confirm arrival on B --
    { $_ -in '01-1', '91-1' } {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[PASS] app write replaced cleanly. Now confirm A1 arrives on endpoint B (runbook).'
        exit 0
    }
    { $_ -in '01-2', '91-2' } {
        $m = (& $bin marker $target)
        Log-Markers $caseDir $target 'final'
        if ($m -match 'D2=1234') { Write-Output '[PASS] A1 survives locally'; exit 0 }
        Fail "A1 marker missing: $m"
    }

    # -- 02 remote-first: F0 recorded, R1 lands BEFORE the app starts writing -> refuse --
    '02-1' {
        $f0 = Get-Fp $target
        Save-CaseState $caseDir @{ f0 = $f0 }
        Write-CaseLog $caseDir @{ event = 'after-read-f0'; f0 = $f0 }
        Write-Output '[ ok ] F0 recorded. Endpoint B: type R1 into G2 and save (runbook).'
        exit 0
    }
    '02-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.f0
        Assert-WatchArrived $w
        $now = Get-Fp $target
        if ($now -eq $st.f0) { Fail 'fingerprint unchanged after watch reported a change' }
        Write-CaseLog $caseDir @{ event = 'write-refused'; f0 = $st.f0; current_fp = $now }
        Log-Markers $caseDir $target 'remote-first'
        Write-Output '[PASS] app refuses to start the write (remote update detected first)'
        exit 0
    }

    # -- 03 before-replace: final check PASSES, R1 lands in the check->replace window, the
    # -- replace goes through, and the §4.2.2 backup/F0 comparison must catch it AFTERWARDS --
    '03-1' {
        $r = Invoke-AppWrite $caseDir -StopBeforeReplace
        if ($r -ne 'stopped') { Fail "expected barrier stop before replace, got '$r'" }
        Write-Output '[ ok ] final check passed; barrier before-replace. Endpoint B: type R1 into G2 and save (runbook).'
        exit 0
    }
    '03-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.f0
        Assert-WatchArrived $w
        # Deliberately NO re-check of F0 here — this case proves the post-hoc detection.
        $r = Invoke-ReplaceOnly $caseDir
        if ($r -eq 'conflict-detected') {
            Write-Output '[PASS] backup != F0 detected the conflict after ReplaceFileW; R1 preserved in bom.backup.xlsx'
            exit 0
        }
        Fail "replace saw no conflict (result '$r') — the R1 version would be silently lost"
    }

    # -- 04 remote-after-replace (and 92 presence-ON): A1 replaced, then R1 arrives --
    { $_ -in '04-1', '92-1' } {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[ ok ] A1 replaced. BEFORE endpoint B acts: confirm on Drive Web that A1 reached the cloud (runbook).'
        Write-Output '       Then endpoint B (sync PAUSED beforehand): type R1, save, resume.'
        exit 0
    }
    { $_ -in '04-2', '92-2' } {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        Assert-WatchArrived $w
        Log-Markers $caseDir $target 'after-remote-arrival'
        Show-NewFiles $session
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] change arrived and settled: record which marker survived + where the loser went (runbook).'
        exit 0
    }

    # -- 05a/05b pause both, change both, resume in each order (reference test) --
    { $_ -in '05a-1', '05b-1' } {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[ ok ] A changed while paused. Endpoint B: change G2 while paused, then resume per runbook order.'
        exit 0
    }
    { $_ -in '05a-2', '05b-2' } {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        Log-Markers $caseDir $target 'after-resume'
        Show-NewFiles $session
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] observational: record convergence time + surviving marker (reference test).'
        exit 0
    }

    # -- 06 offline return: B held B0 offline; A1 must NOT be overwritten by stale B0 --
    '06-1' {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[ ok ] A1 replaced+syncing. Confirm on Drive Web that A1 reached the cloud,'
        Write-Output '       THEN endpoint B (offline, UNCHANGED B0): reconnect per runbook.'
        exit 0
    }
    '06-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        # Here exit 2 (no local change) is an EXPECTED outcome, exit 0 is observational; anything
        # else — including unknown codes — is fail closed.
        if ($w -notin 0, 2) { Fail "INCONCLUSIVE: watch-stable did not end cleanly (exit $w) — retry per runbook" }
        $m = (& $bin marker $target)
        Log-Markers $caseDir $target 'after-b-reconnect'
        if ($m -match 'D2=800') { Fail 'A1 was overwritten by the stale offline copy' }
        if ($w -eq 2 -and $m -match 'D2=1234') {
            Write-Output '[PASS-local] endpoint A unchanged, A1 marker intact. The case verdict ALSO'
            Write-Output '             requires the A1 marker confirmed on endpoint B and Drive Web (runbook).'
            exit 0
        }
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] file changed but A1 survives locally — record what arrived + confirm B/Web markers (runbook).'
        exit 0
    }

    # -- 07 conflict copy (reference): provoke a conflict, then hunt ALL candidate locations --
    '07-1' {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[ ok ] A side changed. Endpoint B: provoke the conflict per runbook, then run step 2.'
        exit 0
    }
    '07-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        Log-Markers $caseDir $target 'after-conflict'
        Show-NewFiles $session
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] observational: also check My Drive root / Lost & Found / version history on Drive Web.'
        exit 0
    }

    # -- 08 backup sync: does the ReplaceFileW backup itself get uploaded? --
    '08-1' {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        $backup = Join-Path $caseDir 'bom.backup.xlsx'
        if (-not (Test-Path $backup)) { Fail 'backup file missing after replace' }
        Log-Markers $caseDir $backup 'backup'
        Write-Output '[PASS] backup exists locally (should hold B0). Watch B/Web: does it appear there? (runbook)'
        exit 0
    }
    '08-2' {
        $backup = Join-Path $caseDir 'bom.backup.xlsx'
        $m = (& $bin marker $backup)
        Log-Markers $caseDir $backup 'backup-final'
        if ($m -match 'D2=800') { Write-Output '[PASS] backup still holds pre-replace B0'; exit 0 }
        Fail "backup content unexpected: $m"
    }

    default { Fail "unknown case/step: $Case-$Step" }
}
