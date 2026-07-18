# End-to-end proof that the calc-state lifecycle matches real Excel (plan.md §4.4.1 / §4.4.2, step 2).
#
# The pure logic is unit-tested (cargo test); this drives REAL Excel so the fingerprint-based
# restore rule and — critically — the fullCalcOnLoad premise are checked against reality.
#
# PR #21 review finding 2: an earlier version called $xl.CalculateFull(), which would have made
# the test pass even if fullCalcOnLoad were broken, and displayed E2 without asserting it. This
# version never calls CalculateFull, asserts every value, and runs a CONTROL (an identical write
# WITHOUT fullCalcOnLoad) to isolate the flag as the cause of the recalculation.
#
#   control: write without fullCalcOnLoad -> open  -> E2 must STILL be the old cache (800)
#   real   : write with    fullCalcOnLoad -> open  -> E2 must be recalculated (2468) with NO
#            CalculateFull call -> save -> fingerprint changes -> restore() = Trusted, and the
#            saved file's raw <v> cache must hold 2468
#
# Usage: pwsh -File lifecycle.ps1

$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$bin = Join-Path $dir 'target\debug\excel-link-poc.exe'
$fixture = Join-Path $dir 'fixtures\rich.xlsx'

if (-not (Test-Path $fixture)) { pwsh -File (Join-Path $dir 'gen-fixture.ps1') | Out-Null }
& cargo build --quiet 2>$null

function FP($p) { (& $bin fingerprint $p).Trim() }
function Restore($p, $last, $recalc) { (& $bin restore $p $last $recalc).Trim() }
function RawCache($zipPath, $cellRef) {
    # The <v> Excel persisted for a cell, read straight from the sheet XML (no Excel involved).
    $tmp = Join-Path $dir 'out\_lifecycle_extract'
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    Expand-Archive $zipPath -DestinationPath $tmp
    $xml = Get-Content (Join-Path $tmp 'xl\worksheets\sheet1.xml') -Raw
    $m = [regex]::Match($xml, "<c r=`"$cellRef`"[^>]*>(?:<f[^>]*>[^<]*</f>|<f[^>]*/>)?<v>([^<]*)</v>")
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    if ($m.Success) { $m.Groups[1].Value } else { '(none)' }
}
function OpenReadE2($path) {
    # Open in real Excel WITHOUT any explicit recalculation, read E2, close without saving.
    $xl = New-Object -ComObject Excel.Application
    $xl.Visible = $false; $xl.DisplayAlerts = $false
    $wb = $xl.Workbooks.Open($path)
    $repaired = $wb.RepairedRecords.Count
    $v = $wb.Worksheets.Item('BOM').Range('E2').Value2
    $wb.Close($false)
    $xl.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
    Start-Sleep -Milliseconds 300
    @{ value = $v; repaired = $repaired }
}
function AssertEq($label, $actual, $expected) {
    if ("$actual" -ne "$expected") { Write-Error "ASSERT FAILED: $label — expected $expected, got $actual"; exit 1 }
    Write-Output "   [assert] $label = $actual (ok)"
}

Write-Output '== CONTROL: identical write WITHOUT fullCalcOnLoad =='
& $bin rmw $fixture zip-nofco | Out-Null
$ctl = Join-Path $dir 'out\rich-after-zip-nofco.xlsx'
AssertEq 'control: raw E2 cache before opening' (RawCache $ctl 'E2') '800'
$r = OpenReadE2 $ctl
AssertEq 'control: RepairedRecords' $r.repaired 0
AssertEq 'control: E2 after opening WITHOUT the flag (must stay stale)' $r.value '800'
Write-Output '   -> merely opening does NOT recalculate; the flag is required'

Write-Output ''
Write-Output '== step 1: app writes EC cell (zip edit, fullCalcOnLoad SET) =='
& $bin rmw $fixture zip | Out-Null
$after = Join-Path $dir 'out\rich-after-zip.xlsx'
AssertEq 'raw E2 cache before opening (stale by construction)' (RawCache $after 'E2') '800'
$F1 = FP $after
Write-Output "   F1 (after app write) = $($F1.Substring(0,16))..."
$s1 = Restore $after $F1 'true'
AssertEq 'restore(current=F1, lastWrite=F1, recalc=true)' $s1 'Stale usable=false'

Write-Output ''
Write-Output '== step 2: nobody saved -> fingerprint unchanged -> still Stale (scenario 5/6) =='
AssertEq 'fingerprint unchanged' (FP $after) $F1
$s2 = Restore $after $F1 'true'
AssertEq 'restore after app restart (must NOT return to Unverified)' $s2 'Stale usable=false'

Write-Output ''
Write-Output '== step 3: Excel merely OPENS the file (no CalculateFull anywhere) =='
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($after)
AssertEq 'RepairedRecords' $wb.RepairedRecords.Count 0
AssertEq 'E2 immediately after opening (fullCalcOnLoad did the recalc)' $wb.Worksheets.Item('BOM').Range('E2').Value2 '2468'
$wb.Save()
$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
Start-Sleep -Milliseconds 300

Write-Output ''
Write-Output '== step 4: app re-reads -> fingerprint changed -> Trusted, cache now correct (scenario 4) =='
$F2 = FP $after
if ($F2 -eq $F1) { Write-Error 'ASSERT FAILED: fingerprint did not change after Excel save'; exit 1 }
Write-Output "   [assert] fingerprint changed: F2 != F1 (ok)"
$s4 = Restore $after $F1 'true'
AssertEq 'restore(current=F2, lastWrite=F1, recalc=true)' $s4 'Trusted usable=true'
AssertEq 'raw E2 cache in the SAVED file (what calamine would read)' (RawCache $after 'E2') '2468'

Write-Output ''
Write-Output '== LIFECYCLE OK: control stayed stale; fullCalcOnLoad alone recalculated; Stale -> Trusted =='
