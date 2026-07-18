# Builds the "semi-real" BOM for the step-3 preservation test (plan.md §7 step 3).
#
# Base data = the app's actual TestBom (real MISUMI part numbers, dumped by dump-testbom.py to
# fixtures/testbom.tsv — gitignored, never committed). On top of that, the rich Excel features
# from gen-fixture.ps1 are layered so the preservation test exercises them on a realistic sheet:
# formulas, cross-sheet lookup, named range, table, conditional formatting, merged cells,
# shapes, chart, print setup, and a dynamic-array spill.
#
# NOT a genuine production file — it is TestBom data + synthetic richness. Stated as such in the
# results. Column layout: A No | B 型番 | C 数量 | D 品名 | E 発注先 | F 材質 | G 注文番号 |
# H EC単価(app-owned, hand values) | I 小計(user formula) — header row 1, sheet 'BOM'.
#
# Usage: python dump-testbom.py && pwsh -File gen-testbom.ps1
#   then: cargo run -- rmw fixtures/testbom.xlsx zip H2
#         cargo run -- diff fixtures/testbom.xlsx out/testbom-after-zip.xlsx H2

$ErrorActionPreference = 'Stop'
$tsv = Join-Path $PSScriptRoot 'fixtures\testbom.tsv'
$outFile = Join-Path $PSScriptRoot 'fixtures\testbom.xlsx'
if (-not (Test-Path $tsv)) { Write-Error "run dump-testbom.py first ($tsv missing)"; exit 1 }
if (Test-Path $outFile) { Remove-Item $outFile -Force }

# The unary comma keeps each split row as ONE array element — without it the pipeline flattens
# everything into a single flat list of fields and $row[0] silently becomes "first CHARACTER".
$rows = Get-Content $tsv -Encoding UTF8 | Select-Object -Skip 1 | ForEach-Object { , ($_ -split "`t") }

$made = [ordered]@{}
function Try-Add($name, [scriptblock]$body) {
    try { & $body; $made[$name] = 'OK' }
    catch { $made[$name] = "FAILED: $($_.Exception.Message -replace '\s+', ' ')" }
}

try { $xl = New-Object -ComObject Excel.Application }
catch { Write-Error 'Excel COM unavailable.'; exit 1 }
$xl.Visible = $false; $xl.DisplayAlerts = $false
Write-Output "Excel COM $($xl.Version)"

$wb = $xl.Workbooks.Add()
while ($wb.Worksheets.Count -lt 2) { $wb.Worksheets.Add([System.Reflection.Missing]::Value, $wb.Worksheets.Item($wb.Worksheets.Count)) | Out-Null }
while ($wb.Worksheets.Count -gt 2) { $wb.Worksheets.Item($wb.Worksheets.Count).Delete() }
$s = $wb.Worksheets.Item(1); $s.Name = 'BOM'
$ref = $wb.Worksheets.Item(2); $ref.Name = 'Ref'

Try-Add 'TestBom data rows' {
    $s.Range('A1').Value2 = 'No';   $s.Range('B1').Value2 = '型番'
    $s.Range('C1').Value2 = '数量'; $s.Range('D1').Value2 = '品名'
    $s.Range('E1').Value2 = '発注先'; $s.Range('F1').Value2 = '材質'
    $s.Range('G1').Value2 = '注文番号'; $s.Range('H1').Value2 = 'EC単価'
    $s.Range('I1').Value2 = '小計'
    $r = 2
    foreach ($row in $rows) {
        # No / Qty are numeric — writing them as strings would poison every formula
        # (a string "1.0" times a number is #VALUE!).
        [double]$no = $row[0]; [string]$pn = $row[1]; [string]$nm = $row[2]
        [string]$ord = $row[3]; [double]$qty = $row[4]; [string]$mat = $row[5]
        $s.Range("A$r").Value2 = $no
        $s.Range("B$r").Value2 = $pn
        $s.Range("C$r").Value2 = $qty
        $s.Range("D$r").Value2 = $nm
        $s.Range("E$r").Value2 = $ord
        $s.Range("F$r").Value2 = $mat
        $s.Range("G$r").Value2 = "PO-A-00$($r - 1)"
        $s.Range("H$r").Value2 = 400          # app-owned values the app would overwrite
        $s.Range("I$r").Formula = "=C$r*H$r"  # 小計: user formula depending on the app column
        $r++
    }
}

Try-Add 'cross-sheet reference data + VLOOKUP' {
    $ref.Range('A1').Value2 = 'order'; $ref.Range('B1').Value2 = 'transport'
    $ref.Range('A2').Value2 = 'MISUMI'; $ref.Range('B2').Value2 = 'webview'
    $s.Range('K2').Formula = '=VLOOKUP(E2,Ref!A:B,2,FALSE)'
}
Try-Add 'SUM total' { $s.Range('I7').Formula = '=SUM(I2:I5)' }
Try-Add 'dynamic array spill (=UNIQUE(型番))' { $s.Range('K5').Formula2 = '=UNIQUE(B2:B5)' }
Try-Add 'named range' { $wb.Names.Add('BomParts', $s.Range('B2:B5')) | Out-Null }
Try-Add 'Excel table on Ref' {
    $lo = $ref.ListObjects.Add(1, $ref.Range('A1:B2'), $null, 1)
    $lo.Name = 'OrderTable'
}
Try-Add 'formatting (bold/fill/border/number format/col width)' {
    $h = $s.Range('A1:I1')
    $h.Font.Bold = $true; $h.Interior.Color = 15773696; $h.Borders.LineStyle = 1
    $s.Range('H2:I7').NumberFormatLocal = '#,##0" 円"'
    $s.Range('B:B').ColumnWidth = 36
}
Try-Add 'merged note cell' { $s.Range('A9:C9').Merge(); $s.Range('A9').Value2 = 'TestBom 準実データ' }
Try-Add 'conditional formatting on 数量' {
    $fc = $s.Range('C2:C5').FormatConditions.Add(1, 5, '1')
    $fc.Interior.Color = 255
}
Try-Add 'shape + chart' {
    $s.Shapes.AddShape(1, 500, 20, 90, 40) | Out-Null
    $co = $s.ChartObjects().Add(500, 80, 260, 160)
    $co.Chart.SetSourceData($s.Range('B1:B5,H1:H5'))
    $co.Chart.ChartType = 51
}
Try-Add 'print setup' {
    $s.PageSetup.Orientation = 2
    $s.PageSetup.PrintArea = '$A$1:$I$9'
    $s.PageSetup.PrintTitleRows = '$1:$1'
}

$wb.SaveAs($outFile, 51)
$wb.Close($false)
$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null

Write-Output ''
Write-Output '=== elements (FAILED ones are NOT verified by the preservation test) ==='
$made.GetEnumerator() | ForEach-Object {
    $mark = if ($_.Value -eq 'OK') { ' ok ' } else { 'FAIL' }
    Write-Output ("[{0}] {1}{2}" -f $mark, $_.Key, $(if ($_.Value -ne 'OK') { " -> $($_.Value)" } else { '' }))
}
Write-Output "wrote: $outFile ($((Get-Item $outFile).Length) bytes)"
