# Generates readfix.xlsx — the real-Excel fixture for the §7.1.1 read-side
# permanent regression tests (docs/plans/0018-excel-link-mode/plan.md §7.1.1):
#   1. value range and formula range start at DIFFERENT origins (§3.4.1 trap)
#   2. sparse formula placement
#   3. multiple sheets
#   4. shared formulas / array formulas / dynamic arrays (spill)
#
# Run ONCE on a machine with desktop Excel (COM), then commit the .xlsx:
#   pwsh -File gen-readfix.ps1
# The tests read the committed binary; CI never runs Excel. No real part numbers.

$ErrorActionPreference = 'Stop'
$out = Join-Path $PSScriptRoot 'readfix.xlsx'
if (Test-Path $out) { Remove-Item $out }

$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
try {
    $wb = $excel.Workbooks.Add()
    while ($wb.Worksheets.Count -gt 1) { $wb.Worksheets.Item(2).Delete() }
    $ws = $wb.Worksheets.Item(1)
    $ws.Name = 'BOM'

    # §3.4.1: values exist from A1, but the FIRST formula sits at B1 → the formula
    # Range's origin differs from the value Range's origin.
    $ws.Range('A1').Value2 = [double]4
    $ws.Range('B1').Formula = '=A1*2'
    $ws.Range('C1').Formula = '=CONCATENATE("TEST-",A1)'

    # Data rows + shared formula (fill E2:E4 = C*D — Excel stores it shared).
    for ($r = 2; $r -le 4; $r++) {
        $ws.Range("A$r").Value2 = "TEST-PART-00$($r - 1)"
        $ws.Range("C$r").Value2 = [double]($r * 10)
        $ws.Range("D$r").Value2 = [double]100
    }
    $ws.Range('E2').Formula = '=C2*D2'
    $ws.Range('E2').AutoFill($ws.Range('E2:E4')) | Out-Null

    # Sparse formula far from the others.
    $ws.Range('H7').Formula = '=SUM(C2:C4)'

    # Legacy array formula (Ctrl+Shift+Enter style) across F2:F4.
    $ws.Range('F2:F4').FormulaArray = '=C2:C4+1'

    # Dynamic array (spill) at G2 if this Excel supports it; ignore if not.
    try { $ws.Range('G2').Formula2 = '=UNIQUE(A2:A4)' } catch {}

    # Second sheet with its own formula (multi-sheet case).
    $ws2 = $wb.Worksheets.Add([System.Reflection.Missing]::Value, $ws)
    $ws2.Name = 'Other'
    $ws2.Range('B2').Value2 = [double]5
    $ws2.Range('C3').Formula = '=B2*3'

    $wb.SaveAs($out, 51)  # xlOpenXMLWorkbook
    $wb.Close($false)
    Write-Output "wrote $out"
} finally {
    $excel.Quit()
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($excel) | Out-Null
}
