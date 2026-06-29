import { useCallback, useMemo } from "react";
import type { Ref } from "react";
import { AgGridReact } from "ag-grid-react";
import type { CellValueChangedEvent, RowDragEndEvent } from "ag-grid-community";
import type { BomDoc, BomRow } from "../types/bom";
import { buildColumnDefs } from "../lib/columns";
import { bomTheme } from "../lib/agTheme";

interface Props {
  doc: BomDoc;
  onChange: (d: BomDoc) => void;
  gridRef: Ref<AgGridReact<BomRow>>;
  quickFilter: string;
}

export function BomEditor({ doc, onChange, gridRef, quickFilter }: Props) {
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

  // Managed row drag: read the grid's new order back into the doc.
  const onRowDragEnd = useCallback(
    (e: RowDragEndEvent<BomRow>) => {
      const rows: BomRow[] = [];
      e.api.forEachNode((n) => {
        if (n.data) rows.push(n.data);
      });
      onChange({ ...doc, rows });
    },
    [doc, onChange],
  );

  return (
    <div className="grid-wrap">
      <AgGridReact<BomRow>
        ref={gridRef}
        theme={bomTheme}
        rowData={doc.rows}
        columnDefs={columnDefs}
        getRowId={(p) => p.data.id}
        rowSelection={{ mode: "multiRow" }}
        onCellValueChanged={onCellValueChanged}
        rowDragManaged
        onRowDragEnd={onRowDragEnd}
        quickFilterText={quickFilter}
        undoRedoCellEditing
        undoRedoCellEditingLimit={50}
        defaultColDef={{ resizable: true, sortable: false, filter: true, minWidth: 80 }}
        singleClickEdit
        stopEditingWhenCellsLoseFocus
      />
    </div>
  );
}
