# End-to-end proof that the calc-state lifecycle matches real Excel (plan.md §4.4.1 / §4.4.2, step 2).
#
# The pure logic is unit-tested (cargo test), but this drives an ACTUAL Excel recalc so the
# fingerprint-based restore rule is checked against reality, not a model:
#
#   1. app writes an EC cell (zip edit, sets fullCalcOnLoad) -> record F1 (state = Stale)
#   2. WITHOUT Excel saving, re-check: fingerprint still F1 -> restore() must say Stale  (scenario 5/6)
#   3. Excel opens + recalculates + SAVES + closes          -> fingerprint becomes F2 != F1
#   4. app re-reads: restore() must say Trusted, and the recalculated value is now readable (4)
#
# Usage: pwsh -File lifecycle.ps1

$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$bin = Join-Path $dir 'target\debug\excel-link-poc.exe'
$fixture = Join-Path $dir 'fixtures\rich.xlsx'
$after = Join-Path $dir 'out\rich-after-zip.xlsx'

if (-not (Test-Path $fixture)) { pwsh -File (Join-Path $dir 'gen-fixture.ps1') | Out-Null }
& cargo build --quiet 2>$null

function FP($p) { (& $bin fingerprint $p).Trim() }
function Restore($p, $last, $recalc) { (& $bin restore $p $last $recalc).Trim() }

Write-Output '== step 1: app writes EC cell (zip edit, fullCalcOnLoad set) =='
& $bin rmw $fixture zip | Out-Null
$F1 = FP $after
Write-Output "   F1 (after app write) = $($F1.Substring(0,16))..."
$s1 = Restore $after $F1 'true'
Write-Output "   restore(current=F1, lastWrite=F1, recalc=true) = $s1"
if ($s1 -notmatch '^Stale') { Write-Error "expected Stale, got $s1"; exit 1 }

Write-Output ''
Write-Output '== step 2: nobody saved -> fingerprint unchanged -> still Stale (scenario 5/6) =='
$F1b = FP $after
Write-Output "   fingerprint now = $($F1b.Substring(0,16))...  (== F1: $($F1b -eq $F1))"
$s2 = Restore $after $F1 'true'
Write-Output "   restore = $s2   <- an app restart must NOT silently return to Unverified"
if ($s2 -notmatch '^Stale') { Write-Error "expected Stale, got $s2"; exit 1 }

Write-Output ''
Write-Output '== step 3: Excel opens + recalculates + saves + closes =='
$xl = New-Object -ComObject Excel.Application
$xl.Visible = $false; $xl.DisplayAlerts = $false
$wb = $xl.Workbooks.Open($after)
Write-Output "   RepairedRecords: $($wb.RepairedRecords.Count)"
$xl.CalculateFull()
$e2 = $wb.Worksheets.Item('BOM').Range('E2').Value2
Write-Output "   E2 after recalc = $e2   (C2*D2 = 2*1234 = 2468 expected)"
$wb.Save()
$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
Start-Sleep -Milliseconds 300

Write-Output ''
Write-Output '== step 4: app re-reads -> fingerprint changed -> Trusted (scenario 4) =='
$F2 = FP $after
Write-Output "   F2 (after Excel save) = $($F2.Substring(0,16))...  (!= F1: $($F2 -ne $F1))"
$s4 = Restore $after $F1 'true'
Write-Output "   restore(current=F2, lastWrite=F1, recalc=true) = $s4"
if ($s4 -notmatch '^Trusted') { Write-Error "expected Trusted, got $s4"; exit 1 }

Write-Output ''
Write-Output '== LIFECYCLE OK: Stale -> (unchanged) Stale -> (Excel recalc+save) Trusted =='
