import { useCallback, useEffect, useMemo, useRef, useState } from "react";
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
import { ackKey, buildColumnDefs, pendingSuggestion, setCellValue } from "../lib/columns";
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
  // Set when the chooser's "自分で編集" starts an edit, so the next commit on that cell is
  // treated as a deliberate reconciliation (ack ← fetched) rather than a plain edit.
  const pendingEditAckRef = useRef<{ rowId: string; colId: string; fetched: string } | null>(null);

  // Columns only need to rebuild when the column set changes.
  const columnDefs = useMemo(() => buildColumnDefs(doc), [doc.columns]);

  const onCellValueChanged = useCallback(
    (e: CellValueChangedEvent<BomRow>) => {
      // AG Grid has already mutated e.data via field / valueSetter; reflect it
      // back into the doc immutably so state stays the source of truth.
      // Editing the designated 型番列 (partNo role) invalidates the fetched EC result for
      // that row — resolve the role column dynamically, not the hardcoded "partsNo" key.
      const colId = e.column.getColId();
      const partKey = partNoColumn(doc)?.key;
      let base = { ...e.data };
      // If this commit is the chooser's "自分で編集", record it as reconciled (ack ← fetched)
      // so the difference highlight clears, matching the other two options. Plain edits leave
      // any existing ack untouched (a re-fetch that changes the value re-flags it).
      const pend = pendingEditAckRef.current;
      if (pend && pend.rowId === e.data.id && pend.colId === colId) {
        base = { ...base, custom: { ...base.custom, [ackKey(colId)]: pend.fetched } };
        pendingEditAckRef.current = null;
      }
      const updated = colId === partKey ? { ...base, supplier: undefined } : base;
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
      pendingEditAckRef.current = null; // reset any stale edit-myself intent
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

  // Apply the selected option on OK.
  //  - adopt : write the MISUMI value through the grid (valueSetter → onCellValueChanged);
  //            current becomes fetched so the highlight clears naturally.
  //  - edit  : open the inline editor.
  //  - keep  : keep the current value but mark the difference reconciled (ack = fetched) so
  //            the highlight clears until a future fetch changes the MISUMI value.
  const applyChoice = (action: "adopt" | "keep" | "edit" = choice) => {
    if (!chooser || !api) return;
    const c = chooser;
    setChooser(null);
    if (action === "adopt") {
      api.getRowNode(c.rowId)?.setDataValue(c.colId, c.fetched);
    } else if (action === "edit") {
      // Remember this cell so its next commit is recorded as reconciled (highlight clears).
      pendingEditAckRef.current = { rowId: c.rowId, colId: c.colId, fetched: c.fetched.trim() };
      api.startEditingCell({ rowIndex: c.rowIndex, colKey: c.colId });
    } else {
      const key = ackKey(c.colId);
      const rows = doc.rows.map((r) =>
        r.id === c.rowId ? { ...r, custom: { ...r.custom, [key]: c.fetched.trim() } } : r,
      );
      onChange({ ...doc, rows });
      // The ack isn't a displayed value, so nudge AG Grid to re-evaluate cellClassRules.
      requestAnimationFrame(() => api.refreshCells({ force: true }));
    }
  };

  // Reconcile one column across the given rows in a single pass (used by the Alt+1/2/3
  // shortcuts, incl. bulk over selected rows). "adopt" writes the fetched value; "keep"
  // records the ack so the highlight clears. Rows without a pending suggestion are skipped.
  const reconcileRows = (rowIds: string[], colId: string, action: "adopt" | "keep") => {
    const col = doc.columns.find((c) => c.key === colId);
    if (!col) return;
    const ids = new Set(rowIds);
    let changed = false;
    const rows = doc.rows.map((r) => {
      if (!ids.has(r.id)) return r;
      const pend = pendingSuggestion(doc, r, col);
      if (!pend) return r;
      changed = true;
      return action === "adopt"
        ? setCellValue(r, col, pend.fetched)
        : { ...r, custom: { ...r.custom, [ackKey(colId)]: pend.fetched.trim() } };
    });
    if (!changed) return;
    onChange({ ...doc, rows });
    requestAnimationFrame(() => api?.refreshCells({ force: true }));
  };

  // Keyboard shortcuts:
  //  - chooser open : 1/2/3 confirm the option, Enter confirms the selection, Esc closes.
  //  - grid focused : Alt+1/2/3 reconcile the focused cell directly (no popover). If rows are
  //                   selected, Alt+1/2 apply to the focused column across all of them (bulk).
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (chooser) {
        if (ev.key === "Escape") {
          ev.preventDefault();
          setChooser(null);
        } else if (ev.key === "Enter") {
          ev.preventDefault();
          applyChoice();
        } else if (ev.key === "1" || ev.key === "2" || ev.key === "3") {
          ev.preventDefault();
          applyChoice(ev.key === "1" ? "adopt" : ev.key === "2" ? "keep" : "edit");
        }
        return;
      }
      if (!api || !ev.altKey || ev.ctrlKey || ev.metaKey) return;
      if (ev.key !== "1" && ev.key !== "2" && ev.key !== "3") return;
      if (api.getEditingCells().length > 0) return;
      const fc = api.getFocusedCell();
      if (!fc || fc.rowPinned) return;
      const node = api.getDisplayedRowAtIndex(fc.rowIndex);
      if (!node?.data) return;
      const colId = fc.column.getColId();
      const col = doc.columns.find((c) => c.key === colId);
      if (!col) return;
      ev.preventDefault();
      if (ev.key === "3") {
        const pend = pendingSuggestion(doc, node.data, col);
        if (!pend) return;
        pendingEditAckRef.current = { rowId: node.data.id, colId, fetched: pend.fetched.trim() };
        api.startEditingCell({ rowIndex: fc.rowIndex, colKey: colId });
        return;
      }
      // Bulk over the selection only when the focused row is part of a multi-row selection;
      // otherwise act on just the focused row (so arrow-moving off the selection is intuitive).
      const selIds = api.getSelectedRows().map((r) => r.id);
      const rowIds =
        selIds.length > 1 && selIds.includes(node.data.id) ? selIds : [node.data.id];
      reconcileRows(rowIds, colId, ev.key === "1" ? "adopt" : "keep");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api, doc, chooser, choice]);

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
        rowSelection={{ mode: "multiRow", enableClickSelection: true }}
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
                MISUMI値を採用 <kbd className="sugg-kbd">Alt+1</kbd>
              </label>
              <label>
                <input type="radio" checked={choice === "keep"} onChange={() => setChoice("keep")} />
                現在の値を採用 <kbd className="sugg-kbd">Alt+2</kbd>
              </label>
              <label>
                <input type="radio" checked={choice === "edit"} onChange={() => setChoice("edit")} />
                自分で編集 <kbd className="sugg-kbd">Alt+3</kbd>
              </label>
            </div>
            <div className="sugg-hint">
              Enter で確定 / Esc で閉じる ・ グリッド上で Alt+1/2/3 は直接反映（複数行選択で一括）
            </div>
            <div className="sugg-actions">
              <button onClick={() => setChooser(null)}>キャンセル</button>
              <button className="primary" onClick={() => applyChoice()}>
                OK
              </button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
