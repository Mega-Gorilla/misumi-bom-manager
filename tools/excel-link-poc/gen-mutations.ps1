# Generates the structure-mutation fixtures for step 3 (plan.md §4.9 / §7.2).
#
# One base BOM plus ~14 mutants, each applying ONE structural edit a user might make in Excel.
# The file-name prefix IS the expected verdict (safe- / warn- / broken-), so
# `cargo run -- verify-structure fixtures/mutations` can machine-check the §4.9 judge:
#
#   safe-   -> Safe    (auto-ingest: row edits, clean new user column)
#   warn-   -> Confirm (stop + present candidates)
#   broken- -> Broken  (no ingest, no write-back)
#
# Base layout (matches POC_CONTRACT in src/main.rs):
#   sheet 'BOM', header row 1: No | 型番 | 数量 | EC単価 | 小計 | 注文番号
#   EC単価 is app-owned; 小計 is a USER formula column (=C*D) — allowed;
#   5 data rows, with the same 型番 on two rows (safe case baked into base).
#
# Usage: pwsh -File gen-mutations.ps1

$ErrorActionPreference = 'Stop'
$outDir = Join-Path $PSScriptRoot 'fixtures\mutations'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

try { $xl = New-Object -ComObject Excel.Application }
catch { Write-Error 'Excel COM unavailable.'; exit 1 }
$xl.Visible = $false; $xl.DisplayAlerts = $false
Write-Output "Excel COM $($xl.Version)"

function New-Mutant([string]$name, [scriptblock]$mutate) {
    $wb = $xl.Workbooks.Add()
    while ($wb.Worksheets.Count -gt 1) { $wb.Worksheets.Item($wb.Worksheets.Count).Delete() }
    $s = $wb.Worksheets.Item(1); $s.Name = 'BOM'

    $s.Range('A1').Value2 = 'No'
    $s.Range('B1').Value2 = '型番'
    $s.Range('C1').Value2 = '数量'
    $s.Range('D1').Value2 = 'EC単価'
    $s.Range('E1').Value2 = '小計'
    $s.Range('F1').Value2 = '注文番号'
    # 5 rows; rows 4 and 5 share the same 型番 (same part twice = safe, judged by header only).
    $parts = [string[]]@('CBT3-8', 'CBT3-10', 'SFB6-20', 'SFB6-20', 'CBT3-12')
    for ($i = 0; $i -lt 5; $i++) {
        $r = $i + 2
        [string]$part = $parts[$i]
        $s.Range("A$r").Value2 = $i + 1
        $s.Range("B$r").Value2 = $part
        $s.Range("C$r").Value2 = $i + 1
        $s.Range("D$r").Value2 = 400
        $s.Range("E$r").Formula = "=C$r*D$r"     # 小計: USER formula column (allowed)
        $s.Range("F$r").Value2 = "PO-00$($i + 1)"
    }

    if ($mutate) { & $mutate $wb $s }

    $p = Join-Path $outDir "$name.xlsx"
    if (Test-Path $p) { Remove-Item $p -Force }
    $wb.SaveAs($p, 51)
    $wb.Close($false)
    Write-Output "wrote $name.xlsx"
}

# ---- base ----
New-Mutant 'base' $null

# ---- safe: row-level edits and a clean new user column ----
New-Mutant 'safe-add-row' {
    param($wb, $s)
    $s.Range('A7').Value2 = 6; $s.Range('B7').Value2 = 'CBT3-16'
    $s.Range('C7').Value2 = 2; $s.Range('D7').Value2 = 500
    $s.Range('E7').Formula = '=C7*D7'; $s.Range('F7').Value2 = 'PO-006'
}
New-Mutant 'safe-del-row' { param($wb, $s) $s.Rows(4).Delete() | Out-Null }
New-Mutant 'safe-reorder-rows' {
    param($wb, $s)
    # Move row 2's values to the bottom (simulates a manual reorder).
    $s.Rows(2).Cut() | Out-Null
    $s.Rows(7).Insert(-4121) | Out-Null   # xlShiftDown
}
New-Mutant 'safe-new-user-col' { param($wb, $s) $s.Range('G1').Value2 = '備考'; $s.Range('G2').Value2 = 'メモ' }

# ---- warn: structure changed but a candidate interpretation exists ----
New-Mutant 'warn-move-col' {
    param($wb, $s)
    # Swap 数量 (C) and 小計 (E) columns wholesale.
    $s.Columns('C').Cut() | Out-Null
    $s.Columns('F').Insert(-4161) | Out-Null   # xlShiftToRight; C moves after E
}
New-Mutant 'warn-rename-header' { param($wb, $s) $s.Range('C1').Value2 = '数' }
New-Mutant 'warn-rename-sheet' { param($wb, $s) $s.Name = 'BOM2' }
New-Mutant 'warn-move-header-row' {
    param($wb, $s)
    $s.Rows(1).Insert(-4121) | Out-Null        # push everything down one row
    $s.Range('A1').Value2 = 'メモ: 部品表'      # something else on the old header row
}
New-Mutant 'warn-move-app-col' {
    param($wb, $s)
    # Move EC単価 (D) to the far end.
    $s.Columns('D').Cut() | Out-Null
    $s.Columns('H').Insert(-4161) | Out-Null
}

# ---- broken: no safe interpretation ----
New-Mutant 'broken-del-required-col' { param($wb, $s) $s.Columns('B').Delete() | Out-Null }   # 型番 gone
New-Mutant 'broken-dup-header' { param($wb, $s) $s.Range('G1').Value2 = '型番' }
New-Mutant 'broken-del-sheet' {
    param($wb, $s)
    # Replace the BOM sheet with an unrelated one.
    $other = $wb.Worksheets.Add()
    $other.Name = 'Other'
    $other.Range('A1').Value2 = 'その他'
    $s.Delete()
}
New-Mutant 'broken-formula-in-app-col' { param($wb, $s) $s.Range('D3').Formula = '=C3*100' }
New-Mutant 'broken-rename-app-col' { param($wb, $s) $s.Range('D1').Value2 = '単価' }

# ---- compound mutations (PR #22 review finding 1): Broken must outrank Confirm ----
New-Mutant 'broken-combo-movedhdr-formula' {
    param($wb, $s)
    $s.Rows(1).Insert(-4121) | Out-Null           # header row moves down (Confirm on its own)
    $s.Range('A1').Value2 = 'メモ: 部品表'
    $s.Range('D4').Formula = '=C4*100'            # AND a formula in the app-owned column (Broken)
}
New-Mutant 'broken-combo-rename-delapp' {
    param($wb, $s)
    $s.Range('C1').Value2 = '数'                  # user header renamed (Confirm on its own)
    $s.Columns('D').Delete() | Out-Null           # AND the app-owned column deleted (Broken)
}

New-Mutant 'broken-combo-rename-formula' {
    param($wb, $s)
    $s.Range('C1').Value2 = '数'                  # user header renamed (Confirm on its own)
    $s.Range('D3').Formula = '=C3*100'            # AND a formula in the surviving app column (Broken)
}
New-Mutant 'broken-combo-rename-dup' {
    param($wb, $s)
    $s.Range('C1').Value2 = '数'                  # user header renamed (Confirm on its own)
    $s.Range('G1').Value2 = '型番'                # AND a duplicated surviving header (Broken)
}

# ---- sheet reorder (PR #22 review finding 2): order-based part lookup reads the WRONG sheet ----
New-Mutant 'safe-reorder-sheets' {
    param($wb, $s)
    # A new sheet BEFORE 'BOM': workbook.xml order changes but sheetN.xml names do not, so only
    # relationship-based resolution still finds the right XML. Verdict must stay Safe.
    $other = $wb.Worksheets.Add($wb.Worksheets.Item(1))
    $other.Name = 'Cover'
    $other.Range('A1').Value2 = '表紙'
}

# ---- headerless data column (PR #22 review finding 4) ----
New-Mutant 'warn-headerless-col' {
    param($wb, $s)
    $s.Range('G3').Value2 = 999                   # data in G, no header in G1
}

$xl.Quit()
[System.Runtime.InteropServices.Marshal]::ReleaseComObject($xl) | Out-Null
Write-Output 'done'
