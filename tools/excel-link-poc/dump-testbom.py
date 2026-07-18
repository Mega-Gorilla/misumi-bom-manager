# Dump the TestBom rows from the app's SQLite DB to a TSV for gen-testbom.ps1.
#
# SELECT-only: this script never writes to the DB. The TSV lands in fixtures/ (gitignored), so
# the real part numbers it contains are never committed.
#
# Usage: python dump-testbom.py [bom-name]   (default: TestBom)

import os
import sqlite3
import sys

name = sys.argv[1] if len(sys.argv) > 1 else "TestBom"
db = os.path.join(os.environ["APPDATA"], "com.misumi.bommanager", "misumi-bom.db")
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "testbom.tsv")
os.makedirs(os.path.dirname(out), exist_ok=True)

con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)  # read-only open, belt and braces
bid = con.execute("select id from bom where name = ?", (name,)).fetchone()
if not bid:
    sys.exit(f"BOM '{name}' not found in {db}")

rows = con.execute(
    'select no, parts_no, parts_name, "order", qty, material'
    " from bom_row where bom_id = ? order by sort_no",
    (bid[0],),
).fetchall()

with open(out, "w", encoding="utf-8", newline="") as f:
    f.write("No\tPartsNo\tPartsName\tOrder\tQty\tMaterial\n")
    for r in rows:
        f.write("\t".join("" if v is None else str(v) for v in r) + "\n")

print(f"wrote {out} ({len(rows)} rows from '{name}')")
