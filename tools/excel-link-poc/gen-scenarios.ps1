# Generates 4 representative BOM workbooks (Excel COM) to measure how heavy the "all formulas ->
# stale" policy is (plan.md §5 / step 2). Each puts formulas in a different place so `stale-scan`
# can count how many BUSINESS-column cells go unusable after an EC fetch.
#
#   A: only 小計/合計 are formulas          (common: subtotal/total computed)
#   B: 型番/数量 are formulas               (heavy: the EC inputs themselves are computed)
#   C: 注文番号 is a concatenation formula  (order number built from parts)
#   D: business columns hand-entered, only a decorative formula elsewhere (light: no impact)
#
# Usage: pwsh -File gen-scenarios.ps1

$ErrorActionPreference = 'Stop'
$outDir = Join-Path $PSScriptRoot 'fixtures'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

try { $xl = New-Object -ComObject Excel.Application }
catch { Write-Error 'Excel COM unavailable.'; exit 1 }
$xl.Visible = $false; $xl.DisplayAlerts = $false
Write-Output "Excel COM $($xl.Version)"

function New-Bom([string]$name, [scriptblock]$fill) {
    $wb = $xl.Workbooks.Add()
    while ($wb.Worksheets.Count -gt 1) { $wb.Worksheets.Item($wb.Worksheets.Count).Delete() }
    $s = $wb.Worksheets.Item(1); $s.Name = 'BOM'
    # Header row shared by all scenarios (matches BUSINESS_HEADERS in stale_scan.rs).
    $s.Range('A1').Value2 = 'No'
    $s.Range('B1').Value2 = '型番'
    $s.Range('C1').Value2 = '数量'
    $s.Range('D1').Value2 = 'EC単価'
    $s.Range('E1').Value2 = '小計'
    $s.Range('F1').Value2 = '注文番号'
    # 3 data rows of plain values as the baseline; each scenario overrides some with formulas.
    for ($r = 2; $r -le 4; $r++) {
        $s.Range("A$r").Value2 = $r - 1
        $s.Range("B$r").Value2 = "CBT3-$r"
        $s.Range("C$r").Value2 = $r
        $s.Range("D$r").Value2 = 400
    }
    & $fill $s
    $p = Join-Path $outDir "scenario-$name.xlsx"
    if (Test-Path $p) { Remove-Item $p -Force }
    $wb.SaveAs($p, 51)
    $wb.Close($false)
    Write-Output "wrote $p"
}

# A: subtotal + total are formulas (business columns 小計).
New-Bom 'A' {
    param($s)
    for ($r = 2; $r -le 4; $r++) { $s.Range("E$r").Formula = "=C$r*D$r" }   # 小計
    $s.Range('E6').Formula = '=SUM(E2:E4)'                                  # 合計
}

# B: 型番 and 数量 are formulas (the heaviest case — EC inputs computed).
New-Bom 'B' {
    param($s)
    for ($r = 2; $r -le 4; $r++) {
        $s.Range("B$r").Formula = "=`"CBT3-`"&A$r"     # 型番 as a formula
        $s.Range("C$r").Formula = "=A$r+1"             # 数量 as a formula
        $s.Range("E$r").Formula = "=C$r*D$r"
    }
}

# C: 注文番号 built by concatenation formula.
New-Bom 'C' {
    param($s)
    for ($r = 2; $r -le 4; $r++) {
        $s.Range("F$r").Formula = "=`"PO-`"&TEXT(A$r,`"000`")"   # 注文番号
        $s.Range("E$r").Formula = "=C$r*D$r"
    }
}

# D: business columns all hand-entered; only a decorative note formula in an unrelated column.
New-Bom 'D' {
    param($s)
    for ($r = 2; $r -le 4; $r++) {
        $s.Range("E$r").Value2 = ($r) * 400            # 小計 hand-entered (a value, not a formula)
        $s.Range("F$r").Value2 = "PO-00$($r-1)"        # 注文番号 hand-entered
    }
    $s.Range('H1').Formula = '=TODAY()'                # decorative, non-business column
}

$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
Write-Output 'done'
