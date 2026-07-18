# Generates a deliberately "rich" .xlsx via real Excel (COM) for the read-modify-write PoC.
#
# Why real Excel: a library-generated file proves nothing. Excel stores things a library may not
# (formula <f> plus cached <v>, calcChain.xml, shared/array formulas, drawings, charts, pageSetup).
# The PoC asks "does a Rust RMW crate preserve what EXCEL wrote", so the fixture must come from Excel.
#
# Every element is tagged so that a failure to create it is reported rather than silently dropped
# (a missing element in the fixture would otherwise look like "preserved" in the diff).
#
# Usage: pwsh -File gen-fixture.ps1

$ErrorActionPreference = 'Stop'
$outDir = Join-Path $PSScriptRoot 'fixtures'
$outFile = Join-Path $outDir 'rich.xlsx'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
if (Test-Path $outFile) { Remove-Item $outFile -Force }

$made = [ordered]@{}
function Try-Add($name, [scriptblock]$body) {
    try { & $body; $made[$name] = 'OK' }
    catch { $made[$name] = "FAILED: $($_.Exception.Message -replace '\s+', ' ')" }
}

try { $xl = New-Object -ComObject Excel.Application }
catch { Write-Error "Excel COM unavailable. This PoC needs Excel installed."; exit 1 }
Write-Output "Excel COM version: $($xl.Version)"
$xl.Visible = $false
$xl.DisplayAlerts = $false

$wb = $xl.Workbooks.Add()
# Workbooks.Add() honours the user's "sheets in new workbook" setting; normalise to exactly 3.
while ($wb.Worksheets.Count -lt 3) { $wb.Worksheets.Add([System.Reflection.Missing]::Value, $wb.Worksheets.Item($wb.Worksheets.Count)) | Out-Null }
while ($wb.Worksheets.Count -gt 3) { $wb.Worksheets.Item($wb.Worksheets.Count).Delete() }

$s1 = $wb.Worksheets.Item(1); $s1.Name = 'BOM'
$s2 = $wb.Worksheets.Item(2); $s2.Name = 'Ref'
$s3 = $wb.Worksheets.Item(3); $s3.Name = 'Empty'

# ---- Sheet 'Ref': lookup target for cross-sheet formulas ----
Try-Add 'cross-sheet source data' {
    $s2.Range('A1').Value2 = 'code'; $s2.Range('B1').Value2 = 'price'
    $s2.Range('A2').Value2 = 'CBT3-8';  $s2.Range('B2').Value2 = 400
    $s2.Range('A3').Value2 = 'CBT3-10'; $s2.Range('B3').Value2 = 450
    $s2.Range('A4').Value2 = 'SFB6-20'; $s2.Range('B4').Value2 = 900
}

# ---- Sheet 'BOM': headers + the cell types we must not corrupt ----
Try-Add 'header row + cell types (number/string/date/bool/error)' {
    $s1.Range('A1').Value2 = 'No'
    $s1.Range('B1').Value2 = '型番'
    $s1.Range('C1').Value2 = '数量'
    $s1.Range('D1').Value2 = 'EC単価'      # app-owned column (the one the app would write)
    $s1.Range('E1').Value2 = '小計'
    $s1.Range('F1').Value2 = '備考'

    $s1.Range('A2').Value2 = 1
    $s1.Range('B2').Value2 = 'CBT3-8'
    $s1.Range('C2').Value2 = 2               # number — must stay numeric, not become text
    $s1.Range('D2').Value2 = 400
    $s1.Range('A3').Value2 = 2
    $s1.Range('B3').Value2 = 'CBT3-10'
    $s1.Range('C3').Value2 = 5
    $s1.Range('D3').Value2 = 450
    $s1.Range('A4').Value2 = 3
    $s1.Range('B4').Value2 = 'SFB6-20'
    $s1.Range('C4').Value2 = 1
    $s1.Range('D4').Value2 = 900

    $s1.Range('H1').Value2 = 'types:'
    $s1.Range('H2').Formula = '=DATE(2026,7,17)'   # date
    $s1.Range('H3').Value2 = $true                 # boolean
    $s1.Range('H4').Formula = '=1/0'               # error value (#DIV/0!)
    $s1.Range('H5').Value2 = 'plain string'
}

# ---- Formulas ----
Try-Add 'numeric formula (=C2*D2)' { $s1.Range('E2').Formula = '=C2*D2' }
Try-Add 'shared formula (same formula filled down E2:E4)' {
    # Excel collapses a filled-down identical formula into <f t="shared">.
    $s1.Range('E2').Copy() | Out-Null
    $s1.Range('E3:E4').PasteSpecial(-4123) | Out-Null   # xlPasteFormulas
    $xl.CutCopyMode = 0
}
Try-Add 'string concat formula (="CBT3-"&A2)' { $s1.Range('F2').Formula = '="CBT3-"&A2' }
Try-Add 'cross-sheet reference (VLOOKUP into Ref)' { $s1.Range('F3').Formula = '=VLOOKUP(B3,Ref!A:B,2,FALSE)' }
Try-Add 'SUM over range' { $s1.Range('E6').Formula = '=SUM(E2:E4)' }
Try-Add 'IF formula' { $s1.Range('F4').Formula = '=IF(C4>1,"multi","single")' }
Try-Add 'array formula (Ctrl+Shift+Enter → <f t="array" ref=...>)' {
    $s1.Range('J2').FormulaArray = '=SUM(C2:C4*D2:D4)'
}
Try-Add 'dynamic array / spill (=UNIQUE(...))' {
    # Excel 365 dynamic array. Spills J5:J7 from a single anchor cell.
    $s1.Range('J5').Formula2 = '=UNIQUE(Ref!A2:A4)'
}

# ---- Named range + Excel Table ----
Try-Add 'defined name (named range)' { $wb.Names.Add('PriceTable', $s2.Range('A1:B4')) | Out-Null }
Try-Add 'Excel Table (ListObject)' {
    $lo = $s1.ListObjects.Add(1, $s1.Range('A1:F4'), $null, 1)   # xlSrcRange, xlYes
    $lo.Name = 'BomTable'
}

# ---- Formatting ----
Try-Add 'bold font + fill + border on header' {
    $h = $s1.Range('A1:F1')
    $h.Font.Bold = $true
    $h.Interior.Color = 15773696      # BGR
    $h.Borders.LineStyle = 1
}
Try-Add 'number format (currency on D:E)' { $s1.Range('D2:E6').NumberFormatLocal = '#,##0" 円"' }
Try-Add 'column width' { $s1.Range('B:B').ColumnWidth = 22 }
Try-Add 'merged cells' { $s1.Range('A8:C8').Merge(); $s1.Range('A8').Value2 = 'merged note' }
Try-Add 'conditional formatting' {
    $fc = $s1.Range('C2:C4').FormatConditions.Add(1, 5, '3')   # xlCellValue, xlGreater, >3
    $fc.Interior.Color = 255
}

# ---- Drawing + Chart ----
Try-Add 'autoshape (drawing)' {
    $s1.Shapes.AddShape(1, 400, 20, 90, 40) | Out-Null   # msoShapeRectangle
}
Try-Add 'chart' {
    $co = $s1.ChartObjects().Add(400, 80, 260, 160)
    $co.Chart.SetSourceData($s1.Range('B1:B4,D1:D4'))
    $co.Chart.ChartType = 51                             # xlColumnClustered
}

# ---- Print setup ----
Try-Add 'pageSetup (landscape / print area / print titles)' {
    $s1.PageSetup.Orientation = 2          # xlLandscape
    $s1.PageSetup.PrintArea = '$A$1:$F$8'
    $s1.PageSetup.PrintTitleRows = '$1:$1'
}

# ---- Save as .xlsx ----
$wb.SaveAs($outFile, 51)                   # xlOpenXMLWorkbook
$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null

Write-Output ""
Write-Output "=== fixture elements (FAILED ones are NOT verified by this PoC) ==="
$made.GetEnumerator() | ForEach-Object {
    $mark = if ($_.Value -eq 'OK') { ' ok ' } else { 'FAIL' }
    Write-Output ("[{0}] {1}{2}" -f $mark, $_.Key, $(if ($_.Value -ne 'OK') { " -> $($_.Value)" } else { '' }))
}
$failed = ($made.Values | Where-Object { $_ -ne 'OK' }).Count
Write-Output ""
Write-Output "wrote: $outFile ($((Get-Item $outFile).Length) bytes)"
Write-Output "elements: $($made.Count - $failed)/$($made.Count) created; $failed failed"
if ($failed -gt 0) { Write-Output "NOTE: failed elements are absent from the fixture, so the diff cannot prove anything about them." }
