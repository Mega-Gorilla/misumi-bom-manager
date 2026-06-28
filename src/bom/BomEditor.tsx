import { useCallback, useMemo } from "react";
import type { Ref } from "react";
import { AgGridReact } from "ag-grid-react";
import { themeQuartz } from "ag-grid-community";
import type { CellValueChangedEvent } from "ag-grid-community";
import type { BomDoc, BomRow } from "../types/bom";
import { buildColumnDefs } from "../lib/columns";

interface Props {
  doc: BomDoc;
  onChange: (d: BomDoc) => void;
  gridRef: Ref<AgGridReact<BomRow>>;
}

export function BomEditor({ doc, onChange, gridRef }: Props) {
  // Columns only need to rebuild when the column set changes.
  const columnDefs = useMemo(() => buildColumnDefs(doc), [doc.columns]);

  const onCellValueChanged = useCallback(
    (e: CellValueChangedEvent<BomRow>) => {
      // AG Grid has already mutated e.data via field / valueSetter; reflect it
      // back into the doc immutably so state stays the source of truth.
      const rows = doc.rows.map((r) => (r.id === e.data.id ? { ...e.data } : r));
      onChange({ ...doc, rows });
    },
    [doc, onChange],
  );

  return (
    <div className="grid-wrap">
      <AgGridReact<BomRow>
        ref={gridRef}
        theme={themeQuartz}
        rowData={doc.rows}
        columnDefs={columnDefs}
        getRowId={(p) => p.data.id}
        rowSelection={{ mode: "multiRow" }}
        onCellValueChanged={onCellValueChanged}
        defaultColDef={{ resizable: true, sortable: false, minWidth: 80 }}
        singleClickEdit
        stopEditingWhenCellsLoseFocus
      />
    </div>
  );
}
