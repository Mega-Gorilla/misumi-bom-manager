// Custom Excel-style fill handle for AG Grid Community (the native one is Enterprise).
// Shows a small square at the bottom-right of the focused editable cell; dragging it
// vertically copies that cell's value into the dragged-over rows (same column).
// Phase 1: copy the source value (no numeric series yet). See memory grid-library-decision.

import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";
import type { GridApi } from "ag-grid-community";
import type { BomDoc, BomRow, ColumnDef } from "../types/bom";
import { getCellValue, setCellValue } from "../lib/columns";

interface Props {
  api: GridApi<BomRow>;
  container: RefObject<HTMLDivElement | null>;
  doc: BomDoc;
  onChange: (d: BomDoc) => void;
}

interface Pos {
  left: number;
  top: number;
}
interface Rect extends Pos {
  width: number;
  height: number;
}

const REPOSITION_EVENTS = [
  "cellFocused",
  "bodyScroll",
  "viewportChanged",
  "columnResized",
  "displayedColumnsChanged",
  "columnMoved",
  "columnPinned",
  "modelUpdated",
];

function escId(id: string): string {
  return typeof CSS !== "undefined" && CSS.escape ? CSS.escape(id) : id;
}

export function FillHandle({ api, container, doc, onChange }: Props) {
  const [handle, setHandle] = useState<Pos | null>(null);
  const [preview, setPreview] = useState<Rect | null>(null);

  const docRef = useRef(doc);
  docRef.current = doc;
  const editingRef = useRef(false);
  const dragRef = useRef<{ col: ColumnDef; srcIndex: number; colId: string; value: unknown } | null>(
    null,
  );

  // Loosely-typed event (un)subscribe — AG Grid's generic event union is awkward here.
  const on = (t: string, l: () => void) => api.addEventListener(t as never, l as never);
  const off = (t: string, l: () => void) => api.removeEventListener(t as never, l as never);

  function focusedEditable(): { rowIndex: number; colId: string; col: ColumnDef } | null {
    const fc = api.getFocusedCell();
    if (!fc) return null;
    const colId = fc.column.getColId();
    const col = docRef.current.columns.find((c) => c.key === colId);
    if (!col || col.kind === "supplier" || !col.editable) return null;
    return { rowIndex: fc.rowIndex, colId, col };
  }

  function cellEl(rowIndex: number, colId: string): HTMLElement | null {
    const root = container.current;
    if (!root) return null;
    return root.querySelector<HTMLElement>(
      `.ag-row[row-index="${rowIndex}"] .ag-cell[col-id="${escId(colId)}"]`,
    );
  }

  function reposition() {
    if (dragRef.current || editingRef.current) return;
    const f = focusedEditable();
    const root = container.current;
    const el = f && cellEl(f.rowIndex, f.colId);
    if (!f || !el || !root) {
      setHandle(null);
      return;
    }
    const r = el.getBoundingClientRect();
    const c = root.getBoundingClientRect();
    setHandle({ left: r.right - c.left, top: r.bottom - c.top });
  }

  function rowIndexAt(x: number, y: number): number | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const rowEl = el?.closest<HTMLElement>(".ag-row");
    const idx = rowEl?.getAttribute("row-index");
    return idx == null ? null : Number(idx);
  }

  function updatePreview(targetIndex: number) {
    const d = dragRef.current;
    const root = container.current;
    if (!d || !root) return;
    const min = Math.min(d.srcIndex, targetIndex);
    const max = Math.max(d.srcIndex, targetIndex);
    const refEl = cellEl(d.srcIndex, d.colId);
    const topEl = cellEl(min, d.colId) ?? refEl;
    const botEl = cellEl(max, d.colId) ?? refEl;
    if (!refEl || !topEl || !botEl) return;
    const c = root.getBoundingClientRect();
    const rr = refEl.getBoundingClientRect();
    const tr = topEl.getBoundingClientRect();
    const br = botEl.getBoundingClientRect();
    setPreview({
      left: rr.left - c.left,
      top: tr.top - c.top,
      width: rr.width,
      height: br.bottom - tr.top,
    });
  }

  function applyFill(
    d: { col: ColumnDef; srcIndex: number; colId: string; value: unknown },
    target: number,
  ) {
    const min = Math.min(d.srcIndex, target);
    const max = Math.max(d.srcIndex, target);
    if (min === max) return;
    const ids = new Set<string>();
    for (let i = min; i <= max; i++) {
      if (i === d.srcIndex) continue;
      const n = api.getDisplayedRowAtIndex(i);
      if (n?.data) ids.add(n.data.id);
    }
    if (!ids.size) return;
    const cur = docRef.current;
    const col = cur.columns.find((c) => c.key === d.colId) ?? d.col;
    const rows = cur.rows.map((r) => (ids.has(r.id) ? setCellValue(r, col, d.value) : r));
    onChange({ ...cur, rows });
  }

  function onMove(ev: MouseEvent) {
    if (!dragRef.current) return;
    const target = rowIndexAt(ev.clientX, ev.clientY);
    if (target == null) return;
    api.ensureIndexVisible(target);
    updatePreview(target);
  }

  function onUp(ev: MouseEvent) {
    const d = dragRef.current;
    window.removeEventListener("mousemove", onMove);
    window.removeEventListener("mouseup", onUp);
    document.body.classList.remove("fill-dragging");
    dragRef.current = null;
    setPreview(null);
    if (d) applyFill(d, rowIndexAt(ev.clientX, ev.clientY) ?? d.srcIndex);
    reposition();
  }

  function onHandleDown(e: React.MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    const f = focusedEditable();
    if (!f) return;
    const node = api.getDisplayedRowAtIndex(f.rowIndex);
    if (!node?.data) return;
    dragRef.current = {
      col: f.col,
      srcIndex: f.rowIndex,
      colId: f.colId,
      value: getCellValue(node.data, f.col),
    };
    document.body.classList.add("fill-dragging");
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    updatePreview(f.rowIndex);
  }

  // Subscribe to grid events that move/resize cells; reposition (rAF-coalesced).
  useEffect(() => {
    let raf = 0;
    const schedule = () => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        reposition();
      });
    };
    const onEditStart = () => {
      editingRef.current = true;
      setHandle(null);
    };
    const onEditStop = () => {
      editingRef.current = false;
      reposition();
    };
    REPOSITION_EVENTS.forEach((e) => on(e, schedule));
    on("cellEditingStarted", onEditStart);
    on("cellEditingStopped", onEditStop);
    window.addEventListener("resize", schedule);
    reposition();
    return () => {
      if (raf) cancelAnimationFrame(raf);
      REPOSITION_EVENTS.forEach((e) => off(e, schedule));
      off("cellEditingStarted", onEditStart);
      off("cellEditingStopped", onEditStop);
      window.removeEventListener("resize", schedule);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  // Reposition when the doc (columns/rows) changes layout.
  useEffect(() => {
    reposition();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [doc]);

  return (
    <>
      {handle && (
        <div
          className="fill-handle"
          style={{ left: handle.left, top: handle.top }}
          onMouseDown={onHandleDown}
          title="ドラッグで下/上のセルへ値をコピー"
        />
      )}
      {preview && (
        <div
          className="fill-preview"
          style={{
            left: preview.left,
            top: preview.top,
            width: preview.width,
            height: preview.height,
          }}
        />
      )}
    </>
  );
}
