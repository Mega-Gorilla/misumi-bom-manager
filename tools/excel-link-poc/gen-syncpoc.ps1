# Generates the step-3c sync-PoC template workbook (Issue #23 §3, plan.md §6.2).
#
# One synthetic BOM with the version-marker cells the `marker` subcommand reads:
#   D2 = 800   (EC単価, app-owned)   -> app write flips it to 1234  = version A1
#   G2 = "B0"  (備考,   user-owned)  -> endpoint B types "R1" there = version R1
#
# No real part numbers (Issue #23 §5) — the fixture may live under fixtures/ (gitignored) but
# must stay committable-safe anyway. Layout matches POC_CONTRACT plus a 備考 column.
#
# Usage: pwsh -File gen-syncpoc.ps1

$ErrorActionPreference = 'Stop'
$outDir = Join-Path $PSScriptRoot 'fixtures'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$outPath = Join-Path $outDir 'syncpoc-template.xlsx'

try { $xl = New-Object -ComObject Excel.Application }
catch { Write-Error 'Excel COM unavailable.'; exit 1 }
$xl.Visible = $false; $xl.DisplayAlerts = $false
Write-Output "Excel COM $($xl.Version)"

$wb = $xl.Workbooks.Add()
while ($wb.Worksheets.Count -gt 1) { $wb.Worksheets.Item($wb.Worksheets.Count).Delete() }
$s = $wb.Worksheets.Item(1); $s.Name = 'BOM'

$s.Range('A1').Value2 = 'No'
$s.Range('B1').Value2 = '型番'
$s.Range('C1').Value2 = '数量'
$s.Range('D1').Value2 = 'EC単価'
$s.Range('E1').Value2 = '小計'
$s.Range('F1').Value2 = '注文番号'
$s.Range('G1').Value2 = '備考'

# Synthetic parts only. Numeric cells get explicit [double] casts (COM writes strings otherwise
# and formulas break with #VALUE! — step 3 lesson).
$parts = [string[]]@('CBT3-8', 'CBT3-10', 'SFB6-20', 'SFB6-20', 'CBT3-12')
for ($i = 0; $i -lt 5; $i++) {
    $r = $i + 2
    $s.Range("A$r").Value2 = [double]($i + 1)
    $s.Range("B$r").Value2 = [string]$parts[$i]
    $s.Range("C$r").Value2 = [double]($i + 1)
    $s.Range("D$r").Value2 = [double]400
    $s.Range("E$r").Formula = "=C$r*D$r"
    $s.Range("F$r").Value2 = "PO-00$($i + 1)"
}
# Marker cells (Issue #23 §3): B0 state.
$s.Range('D2').Value2 = [double]800
$s.Range('G2').Value2 = 'B0'

if (Test-Path $outPath) { Remove-Item $outPath -Force }
$wb.SaveAs($outPath, 51)
$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null

Write-Output "wrote $outPath"
Write-Output 'marker cells: D2=800 (B0->A1 via app write), G2=B0 (->R1 typed on endpoint B)'
