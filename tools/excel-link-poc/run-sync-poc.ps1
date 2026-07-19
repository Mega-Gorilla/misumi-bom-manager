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
    if ($out) { ($out -split ':', 2)[1].Trim() } else { $null }
}

function Read-Session {
    if (-not (Test-Path $sessionPath)) { Fail 'no session — run -Init first' }
    Get-Content $sessionPath -Raw | ConvertFrom-Json
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

# The app write flow with the Issue #23 barrier points. Returns 'replaced' or 'aborted'.
function Invoke-AppWrite([string]$caseDir, [switch]$StopAfterRead) {
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
    Complete-AppWrite $caseDir
}

function Complete-AppWrite([string]$caseDir) {
    $st = Read-CaseState $caseDir
    $target = Join-Path $caseDir 'bom.xlsx'
    $now = Get-Fp $target
    Write-CaseLog $caseDir @{ event = 'before-final-fingerprint-check'; f0 = $st.f0; current_fp = $now }
    if ($now -ne $st.f0) {
        Write-CaseLog $caseDir @{ event = 'conflict-stop'; reason = 'fingerprint changed since read' }
        Remove-Item $st.temp -Force -ErrorAction SilentlyContinue
        return 'aborted'
    }
    $backup = Join-Path $caseDir 'bom.backup.xlsx'
    Write-CaseLog $caseDir @{ event = 'before-replace' }
    [System.IO.File]::Replace($st.temp, $target, $backup)
    $post = Get-Fp $target
    $backupFp = Get-Fp $backup
    Write-CaseLog $caseDir @{ event = 'after-replace'; target_fp = $post; backup_fp = $backupFp }
    Save-CaseState $caseDir @{ postFp = $post; backupFp = $backupFp }
    Log-Markers $caseDir $target 'post-replace'
    return 'replaced'
}

# DRY-RUN ONLY: fake "endpoint B saved R1". Faithful to the real scenario: the remote version
# derives from the ORIGINAL template (B0) — B never saw A's change. G2 flips to 1234 (numeric
# stand-in for the hand-typed "R1"), then the file is overwritten in place like a sync client.
function Invoke-SimulateRemote($session, [string]$caseDir) {
    if (-not $session.dryRun) { Fail '-Simulate is dry-run only; on Drive the real endpoint B acts' }
    $target = Join-Path $caseDir 'bom.xlsx'
    $tempR = Join-Path $caseDir 'bom.remote-temp.xlsx'
    & $bin rmw $session.template zip G2 $tempR > $null
    if ($LASTEXITCODE -ne 0) { Fail 'remote temp generation failed' }
    Move-Item $tempR $target -Force
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

function Show-NewFiles($session) {
    $before = Get-Content $inventoryPath -Raw | ConvertFrom-Json
    $now = Get-TreeInventory $session
    $newPoc = @($now.pocRoot | Where-Object { $before.pocRoot -notcontains $_ })
    $newTop = @($now.parentTop | Where-Object { $before.parentTop -notcontains $_ })
    Write-Output '-- files new since -Init (conflict-copy candidates) --'
    if ($newPoc.Count -eq 0 -and $newTop.Count -eq 0) { Write-Output '   (none)' }
    $newPoc | ForEach-Object { Write-Output "   [poc ] $_" }
    $newTop | ForEach-Object { Write-Output "   [top ] $_  <- mirror-root top level" }
    Write-Output '   (Lost & Found and version history: check Drive Web — runbook)'
}

# ---- -Init -------------------------------------------------------------------------------

if ($Init) {
    & cargo build --quiet 2>$null
    $template = Join-Path $dir 'fixtures\syncpoc-template.xlsx'
    if (-not (Test-Path $template)) { Fail 'template missing — run gen-syncpoc.ps1 first' }
    New-Item -ItemType Directory -Force -Path (Join-Path $dir 'out') | Out-Null

    if ($DryRun) {
        $parentRoot = Join-Path $dir 'out\syncpoc-dryrun-root'
        New-Item -ItemType Directory -Force -Path $parentRoot | Out-Null
        $watchTimeout = 20; $watchStable = 3
    } else {
        $lnk = 'H:\マイドライブ.lnk'
        if (-not (Test-Path $lnk)) { Fail "$lnk not found — Drive for desktop not running?" }
        $sh = New-Object -ComObject WScript.Shell
        $lnkTarget = $sh.CreateShortcut($lnk).TargetPath
        [System.Runtime.InteropServices.Marshal]::ReleaseComObject($sh) | Out-Null
        $parentRoot = RealOf $lnkTarget
        if (-not $parentRoot) { Fail "cannot resolve $lnkTarget to a real path" }
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
    & $bin sync-guard $session.root $session.runId --delete
    if ($LASTEXITCODE -ne 0) { Fail 'sync-guard refused deletion' }
    Write-Output '(session file kept as the run record)'
    exit 0
}

if ($Scan) { $session = Read-Session; Show-NewFiles $session; exit 0 }

if ($Simulate) {
    if (-not $Case) { Fail '-Simulate needs -Case' }
    $session = Read-Session
    Invoke-SimulateRemote $session (Get-CaseDir $session $Case)
    exit 0
}

# ---- -Case NN -Step N --------------------------------------------------------------------

if (-not $Case -or -not $Step) { Fail 'need -Init, -Case NN -Step N, -Simulate, -Scan or -Cleanup' }
$session = Read-Session
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
        if ($w -eq 2) { Fail 'R1 never arrived — sync delivered nothing (INCONCLUSIVE, retry)' }
        $now = Get-Fp $target
        if ($now -eq $st.f0) { Fail 'fingerprint unchanged after watch reported a change' }
        Write-CaseLog $caseDir @{ event = 'write-refused'; f0 = $st.f0; current_fp = $now }
        Log-Markers $caseDir $target 'remote-first'
        Write-Output '[PASS] app refuses to start the write (remote update detected first)'
        exit 0
    }

    # -- 03 before-replace: temp generated, R1 lands in the window -> final check must stop --
    '03-1' {
        Invoke-AppWrite $caseDir -StopAfterRead | Out-Null
        Write-Output '[ ok ] barrier after-read-f0. Endpoint B: type R1 into G2 and save (runbook).'
        exit 0
    }
    '03-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.f0
        if ($w -eq 2) { Fail 'R1 never arrived — sync delivered nothing (INCONCLUSIVE, retry)' }
        if ((Complete-AppWrite $caseDir) -eq 'aborted') {
            Log-Markers $caseDir $target 'after-conflict-stop'
            Write-Output '[PASS] final fingerprint check stopped the replace (backup/F0 mismatch)'
            exit 0
        }
        Fail 'replace went through over a changed file — user change would be lost'
    }

    # -- 04 remote-after-replace (and 92 presence-ON): A1 replaced, then R1 arrives --
    { $_ -in '04-1', '92-1' } {
        if ((Invoke-AppWrite $caseDir) -ne 'replaced') { Fail 'expected clean replace' }
        Write-Output '[ ok ] A1 replaced. Endpoint B (sync PAUSED beforehand): type R1, save, resume (runbook).'
        exit 0
    }
    { $_ -in '04-2', '92-2' } {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        Log-Markers $caseDir $target 'after-remote-arrival'
        Show-NewFiles $session
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] observational: record which marker survived + where the loser went (runbook).'
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
        Write-Output '[ ok ] A1 replaced+syncing. Endpoint B (offline, UNCHANGED B0): reconnect per runbook.'
        exit 0
    }
    '06-2' {
        $st = Read-CaseState $caseDir
        $w = Invoke-Watch $session $caseDir $st.postFp
        $m = (& $bin marker $target)
        Log-Markers $caseDir $target 'after-b-reconnect'
        if ($w -eq 2 -and $m -match 'D2=1234') {
            Write-Output '[PASS] stale B0 did not overwrite A1 (no local change, A1 marker intact)'
            exit 0
        }
        if ($m -match 'D2=800') { Fail 'A1 was overwritten by the stale offline copy' }
        Write-CaseLog $caseDir @{ event = 'observed'; watch_exit = $w }
        Write-Output '[ ok ] file changed but A1 survives — record what arrived (runbook).'
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
