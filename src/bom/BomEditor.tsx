import { useCallback, useMemo, useRef, useState } from "react";
import type { Ref } from "react";
import { AgGridReact } from "ag-grid-react";
import type {
  CellDoubleClickedEvent,
  CellValueChangedEvent,
  GridApi,
  GridReadyEvent,
  RowDragEndEvent,
} from "ag-grid-community";
import type { BomDoc, BomRow } from "../types/bom";
import { partNoColumn } from "../types/bom";
import { buildColumnDefs, pendingSuggestion } from "../lib/columns";
import { bomTheme } from "../lib/agTheme";
import { FillHandle } from "./FillHandle";

interface Props {
  doc: BomDoc;
  onChange: (d: BomDoc) => void;
  gridRef: Ref<AgGridReact<BomRow>>;
  quickFilter: string;
}

export function BomEditor({ doc, onChange, gridRef, quickFilter }: Props) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const [api, setApi] = useState<GridApi<BomRow> | null>(null);
  // Open when a linked cell with a pending EC suggestion is double-clicked (adopt / keep / edit).
  const [chooser, setChooser] = useState<{
    rowId: string;
    colId: string;
    rowIndex: number;
    left: number;
    top: number;
    label: string;
    current: string;
    fetched: string;
  } | null>(null);
  const [choice, setChoice] = useState<"adopt" | "keep" | "edit">("adopt");

  // Columns only need to rebuild when the column set changes.
  const columnDefs = useMemo(() => buildColumnDefs(doc), [doc.columns]);

  const onCellValueChanged = useCallback(
    (e: CellValueChangedEvent<BomRow>) => {
      // AG Grid has already mutated e.data via field / valueSetter; reflect it
      // back into the doc immutably so state stays the source of truth.
      // Editing the designated 型番列 (partNo role) invalidates the fetched EC result for
      // that row — resolve the role column dynamically, not the hardcoded "partsNo" key.
      const partKey = partNoColumn(doc)?.key;
      const updated =
        e.column.getColId() === partKey ? { ...e.data, supplier: undefined } : { ...e.data };
      const rows = doc.rows.map((r) => (r.id === e.data.id ? updated : r));
      onChange({ ...doc, rows });
    },
    [doc, onChange],
  );

  // Double-clicking a linked cell whose value differs from the fetched EC value opens a
  // small chooser (adopt MISUMI value / keep current / edit myself) instead of the inline
  // editor. Cells with no pending suggestion edit normally.
  const onCellDoubleClicked = useCallback(
    (e: CellDoubleClickedEvent<BomRow>) => {
      if (!e.data || e.rowIndex == null) return;
      const colId = e.column.getColId();
      const col = doc.columns.find((c) => c.key === colId);
      if (!col) return;
      const pend = pendingSuggestion(doc, e.data, col);
      if (!pend) return; // no conflict → let the default editor open
      e.api.stopEditing(true); // cancel the edit the double-click just started
      const root = wrapRef.current;
      const esc = typeof CSS !== "undefined" && CSS.escape ? CSS.escape(colId) : colId;
      const cellEl = root?.querySelector<HTMLElement>(
        `.ag-row[row-index="${e.rowIndex}"] .ag-cell[col-id="${esc}"]`,
      );
      if (!root || !cellEl) return;
      const r = cellEl.getBoundingClientRect();
      const c = root.getBoundingClientRect();
      // Clamp so the (wide) popover stays inside the grid area.
      const POP_W = 420;
      const left = Math.max(0, Math.min(r.left - c.left, c.width - POP_W));
      setChoice("adopt");
      setChooser({
        rowId: e.data.id,
        colId,
        rowIndex: e.rowIndex,
        left,
        top: r.bottom - c.top,
        label: col.label,
        current: pend.current,
        fetched: pend.fetched,
      });
    },
    [doc],
  );

  // Apply the selected option on OK. Adopt writes through the grid (valueSetter →
  // onCellValueChanged) so the doc stays authoritative and the highlight clears; edit opens
  // the inline editor; keep just closes.
  const applyChoice = () => {
    if (!chooser || !api) return;
    const c = chooser;
    setChooser(null);
    if (choice === "adopt") api.getRowNode(c.rowId)?.setDataValue(c.colId, c.fetched);
    else if (choice === "edit") api.startEditingCell({ rowIndex: c.rowIndex, colKey: c.colId });
    // keep: no change
  };

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
    <div className="grid-wrap" ref={wrapRef}>
      <AgGridReact<BomRow>
        ref={gridRef}
        theme={bomTheme}
        rowData={doc.rows}
        columnDefs={columnDefs}
        getRowId={(p) => p.data.id}
        context={{ qtyMultiplier: doc.meta.qtyMultiplier ?? 1 }}
        rowSelection={{ mode: "multiRow" }}
        onGridReady={(e: GridReadyEvent<BomRow>) => setApi(e.api)}
        onCellValueChanged={onCellValueChanged}
        onCellDoubleClicked={onCellDoubleClicked}
        rowDragManaged
        onRowDragEnd={onRowDragEnd}
        quickFilterText={quickFilter}
        undoRedoCellEditing
        undoRedoCellEditingLimit={50}
        defaultColDef={{ resizable: true, sortable: false, filter: true, minWidth: 80 }}
        stopEditingWhenCellsLoseFocus
      />
      {api && <FillHandle api={api} container={wrapRef} doc={doc} onChange={onChange} />}
      {chooser && (
        <>
          <div className="sugg-backdrop" onClick={() => setChooser(null)} />
          <div className="sugg-chooser" style={{ left: chooser.left, top: chooser.top }}>
            <div className="sugg-title">提案の反映</div>
            <div className="sugg-sub">列: {chooser.label}</div>
            <div className="sugg-values">
              <div className="sugg-row">
                <span>現在の値</span>
                <b>{chooser.current || "（空欄）"}</b>
              </div>
              <div className="sugg-row">
                <span>MISUMI値</span>
                <b>{chooser.fetched}</b>
              </div>
            </div>
            <div className="sugg-options">
              <label>
                <input type="radio" checked={choice === "adopt"} onChange={() => setChoice("adopt")} />
                MISUMI値を採用
              </label>
              <label>
                <input type="radio" checked={choice === "keep"} onChange={() => setChoice("keep")} />
                今の値のまま
              </label>
              <label>
                <input type="radio" checked={choice === "edit"} onChange={() => setChoice("edit")} />
                自分で編集
              </label>
            </div>
            <div className="sugg-actions">
              <button onClick={() => setChooser(null)}>キャンセル</button>
              <button className="primary" onClick={applyChoice}>
                OK
              </button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
