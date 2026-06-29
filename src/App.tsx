import { useCallback, useEffect, useRef, useState } from "react";
import type { AgGridReact } from "ag-grid-react";
import "./App.css";
import type { BomDoc, BomRow, BomSummary, SupplierQuote } from "./types/bom";
import { newBom, newRow, nextNo, newSupplierColumn, SUPPLIER_FIELDS } from "./types/bom";
import * as api from "./api/bom";
import { BomEditor } from "./bom/BomEditor";
import { Toolbar } from "./bom/Toolbar";
import { BomList } from "./bom/BomList";
import { ColumnManager } from "./bom/ColumnManager";

function slug(s: string): string {
  return (
    s
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "_")
      .replace(/^_|_$/g, "") || "col"
  );
}

function uniqueKey(doc: BomDoc, base: string): string {
  const keys = new Set(doc.columns.map((c) => c.key));
  let key = `c_${base}`;
  let i = 1;
  while (keys.has(key)) key = `c_${base}_${i++}`;
  return key;
}

export default function App() {
  const [view, setView] = useState<"list" | "editor">("list");
  const [doc, setDoc] = useState<BomDoc | null>(null);
  const [list, setList] = useState<BomSummary[]>([]);
  const [status, setStatus] = useState("");
  const [showColumns, setShowColumns] = useState(false);
  const [quickFilter, setQuickFilter] = useState("");
  const [quoting, setQuoting] = useState(false);
  const gridRef = useRef<AgGridReact<BomRow>>(null);

  const reloadList = useCallback(async () => {
    try {
      setList(await api.bomList());
    } catch (e) {
      setStatus(String(e));
    }
  }, []);

  useEffect(() => {
    reloadList();
  }, [reloadList]);

  const openBom = useCallback(async (id: string) => {
    try {
      const d = await api.bomLoad(id);
      if (d) {
        setDoc(d);
        setStatus("");
        setView("editor");
      }
    } catch (e) {
      setStatus(String(e));
    }
  }, []);

  const createBom = useCallback(() => {
    setDoc(newBom());
    setStatus("");
    setView("editor");
  }, []);

  const selectedRows = (): BomRow[] => gridRef.current?.api?.getSelectedRows() ?? [];

  const addRow = () =>
    setDoc((d) => (d ? { ...d, rows: [...d.rows, newRow(nextNo(d.rows))] } : d));

  const dupRows = () => {
    if (!doc) return;
    const sel = selectedRows();
    if (sel.length === 0) {
      setStatus("複製する行を選択してください");
      return;
    }
    // Duplicate values but assign fresh ids and sequential No. (No. must not duplicate).
    // Deep-copy nested data so the clone does not share `custom` (and later `supplier`)
    // objects with the source row — the custom valueSetter mutates row.custom in place.
    let n = nextNo(doc.rows);
    const clones = sel.map((r) => ({
      ...r,
      id: crypto.randomUUID(),
      no: n++,
      custom: { ...r.custom },
      supplier: r.supplier ? structuredClone(r.supplier) : undefined,
    }));
    setDoc({ ...doc, rows: [...doc.rows, ...clones] });
  };

  const renumberNo = () => {
    if (!doc) return;
    setDoc({ ...doc, rows: doc.rows.map((r, i) => ({ ...r, no: i + 1 })) });
    setStatus("No. を振り直しました");
  };

  const delRows = () => {
    const ids = new Set(selectedRows().map((r) => r.id));
    if (!doc || ids.size === 0) {
      setStatus("削除する行を選択してください");
      return;
    }
    setDoc({ ...doc, rows: doc.rows.filter((r) => !ids.has(r.id)) });
  };

  const addColumn = (label: string) => {
    if (!doc || !label.trim()) return;
    const key = uniqueKey(doc, slug(label));
    setDoc({
      ...doc,
      columns: [
        ...doc.columns,
        { key, label: label.trim(), kind: "custom", editable: true, width: 140 },
      ],
    });
  };

  const renameColumn = (key: string, label: string) => {
    if (!doc) return;
    setDoc({ ...doc, columns: doc.columns.map((c) => (c.key === key ? { ...c, label } : c)) });
  };

  const deleteColumn = (key: string) => {
    if (!doc) return;
    const col = doc.columns.find((c) => c.key === key);
    if (!col || col.kind === "core") return; // only core columns are protected
    setDoc({ ...doc, columns: doc.columns.filter((c) => c.key !== key) });
  };

  const addSupplierColumn = (field: string, label: string) => {
    if (!doc) return;
    if (doc.columns.some((c) => c.kind === "supplier" && c.link?.field === field)) return;
    const def = SUPPLIER_FIELDS.find((s) => s.field === field);
    const col = newSupplierColumn(field, label, def?.width);
    const keys = new Set(doc.columns.map((c) => c.key));
    let key = col.key;
    let i = 1;
    while (keys.has(key)) key = `${col.key}_${i++}`;
    setDoc({ ...doc, columns: [...doc.columns, { ...col, key }] });
  };

  // Fetch quotes for ORDER=MISUMI rows and apply them (cache-first; force re-fetch).
  const runQuote = async (force: boolean) => {
    if (!doc || quoting) return;
    const targets = doc.rows.filter(
      (r) => (r.order ?? "").toUpperCase() === "MISUMI" && (r.partsNo ?? "").trim() !== "",
    );
    if (targets.length === 0) {
      setStatus("ORDER=MISUMI かつ Parts No のある行がありません");
      return;
    }
    setQuoting(true);
    setStatus("取得を開始しています…");
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await api.onQuoteProgress((p) =>
        setStatus(p.total > 0 ? `取得中 ${p.done}/${p.total}…` : "キャッシュから取得中…"),
      );
      const items = targets.map((r) => ({ partNo: r.partsNo!.trim() }));
      const quotes = await api.quote("MISUMI", items, force);
      const mult = doc.meta.qtyMultiplier || 1;
      const byId = new Map<string, SupplierQuote>();
      targets.forEach((r, idx) => {
        const q = quotes[idx];
        if (!q) return;
        const qty = r.qty ?? 1;
        const unit = Number(q.quote?.unitPrice);
        if (q.quote && Number.isFinite(unit)) {
          q.quote.subtotal = unit * qty * mult;
        }
        // MOQ is qty-independent (product minSoQty); judge it against THIS row's Qty
        // rather than the representative qty=1 fetch, so the per-row signal is accurate.
        const moq = q.quote?.moq;
        if (moq != null && qty < moq) {
          q.warnings = [...(q.warnings ?? []), `最小発注数 ${moq}（現在 ${qty}）`];
        }
        byId.set(r.id, q);
      });
      setDoc((d) =>
        d
          ? {
              ...d,
              rows: d.rows.map((r) => (byId.has(r.id) ? { ...r, supplier: byId.get(r.id) } : r)),
            }
          : d,
      );
      const errs = quotes.filter((q) => q?.status === "error").length;
      setStatus(`取得完了（${quotes.length} 件${errs ? ` / エラー ${errs}` : ""}）`);
    } catch (e) {
      setStatus(String(e));
    } finally {
      unlisten?.();
      setQuoting(false);
    }
  };

  const undo = () => gridRef.current?.api?.undoCellEditing();
  const redo = () => gridRef.current?.api?.redoCellEditing();
  const autoSize = () => gridRef.current?.api?.autoSizeAllColumns();

  const moveColumn = (key: string, dir: -1 | 1) => {
    if (!doc) return;
    const i = doc.columns.findIndex((c) => c.key === key);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= doc.columns.length) return;
    const cols = [...doc.columns];
    [cols[i], cols[j]] = [cols[j], cols[i]];
    setDoc({ ...doc, columns: cols });
  };

  const saveBom = async () => {
    if (!doc) return;
    try {
      const id = await api.bomSave(doc);
      setDoc({ ...doc, id });
      setStatus("保存しました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  const back = async () => {
    setView("list");
    setDoc(null);
    setStatus("");
    await reloadList();
  };

  const doImport = async () => {
    try {
      const id = await api.importJson();
      if (id) {
        await reloadList();
        await openBom(id);
        setStatus("インポートしました");
      }
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doExport = async () => {
    if (!doc) return;
    try {
      if (await api.exportJson(doc)) setStatus("エクスポートしました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doDelete = async (id: string) => {
    try {
      await api.bomDelete(id);
      await reloadList();
    } catch (e) {
      setStatus(String(e));
    }
  };

  return (
    <div className="app">
      {view === "list" || !doc ? (
        <BomList
          list={list}
          status={status}
          onOpen={openBom}
          onNew={createBom}
          onDelete={doDelete}
          onImport={doImport}
        />
      ) : (
        <>
          <Toolbar
            title={doc.meta.name ?? ""}
            status={status}
            quickFilter={quickFilter}
            onBack={back}
            onSave={saveBom}
            onAddRow={addRow}
            onDupRows={dupRows}
            onDelRows={delRows}
            onRenumber={renumberNo}
            onManageColumns={() => setShowColumns(true)}
            onExport={doExport}
            onRename={(name) => setDoc({ ...doc, meta: { ...doc.meta, name } })}
            onQuickFilter={setQuickFilter}
            onUndo={undo}
            onRedo={redo}
            onAutoSize={autoSize}
            onQuote={runQuote}
            quoting={quoting}
          />
          <BomEditor doc={doc} onChange={setDoc} gridRef={gridRef} quickFilter={quickFilter} />
          {showColumns && (
            <ColumnManager
              columns={doc.columns}
              onAdd={addColumn}
              onAddSupplier={addSupplierColumn}
              onRename={renameColumn}
              onDelete={deleteColumn}
              onMove={moveColumn}
              onClose={() => setShowColumns(false)}
            />
          )}
        </>
      )}
    </div>
  );
}
