import { useCallback, useEffect, useRef, useState } from "react";
import type { AgGridReact } from "ag-grid-react";
import "./App.css";
import type {
  BomDoc,
  BomRow,
  BomSummary,
  ColumnRole,
  SupplierQuote,
  WritePolicy,
} from "./types/bom";
import {
  newBom,
  newRow,
  nextNo,
  newSupplierColumn,
  SUPPLIER_FIELDS,
  partNoColumn,
  sourceColumn,
} from "./types/bom";
import type { Workbook } from "./types/bom";
import { applyLinkedColumns, getCellValue, buildExportGrid } from "./lib/columns";
import * as api from "./api/bom";
import { BomEditor } from "./bom/BomEditor";
import { Toolbar } from "./bom/Toolbar";
import { BomList } from "./bom/BomList";
import { ColumnManager } from "./bom/ColumnManager";
import { ImportWizard } from "./bom/ImportWizard";
import { HistoryDrawer, type HistoryTarget } from "./bom/HistoryDrawer";
import { SummaryBar } from "./bom/SummaryBar";

// ORDER values that map to a supported EC provider (history/quotes exist only for these).
const SUPPORTED_EC = ["MISUMI"];

// The history drawer is a togglable panel, not a popup: once the user opens it we keep it
// open across BOM switches and app restarts (it follows the selected row while open). Persist
// just the open/closed preference so re-entering the editor restores the user's last choice.
const HISTORY_OPEN_KEY = "mbm.historyOpen";
function initialHistoryOpen(): boolean {
  try {
    return localStorage.getItem(HISTORY_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

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
  const [columnsTab, setColumnsTab] = useState<"columns" | "ec">("columns");
  const [importSrc, setImportSrc] = useState<{ workbook: Workbook; fileName: string; path: string } | null>(null);
  const [historyOpen, setHistoryOpen] = useState(initialHistoryOpen);
  const [confirmBack, setConfirmBack] = useState(false);
  const [activeTarget, setActiveTarget] = useState<HistoryTarget>({ kind: "empty", reason: "no-row" });
  const [quickFilter, setQuickFilter] = useState("");
  const [quoting, setQuoting] = useState(false);
  const gridRef = useRef<AgGridReact<BomRow>>(null);
  // JSON snapshot of the doc as of the last load/save; back() compares against it to detect
  // unsaved changes. A freshly created (untouched) BOM counts as clean, like Notepad.
  const savedSnapRef = useRef("");

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

  // Remember the drawer's open/closed state so it persists across editor sessions & restarts.
  useEffect(() => {
    try {
      localStorage.setItem(HISTORY_OPEN_KEY, historyOpen ? "1" : "0");
    } catch {
      /* localStorage unavailable — non-fatal */
    }
  }, [historyOpen]);

  const openBom = useCallback(async (id: string) => {
    try {
      const d = await api.bomLoad(id);
      if (d) {
        setDoc(d);
        savedSnapRef.current = JSON.stringify(d);
        setStatus("");
        setView("editor");
      }
    } catch (e) {
      setStatus(String(e));
    }
  }, []);

  const createBom = useCallback(() => {
    const d = newBom();
    setDoc(d);
    savedSnapRef.current = JSON.stringify(d);
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
    // Deleting the current 型番列 changes the fetch identity — partNoColumn() falls back to
    // the default partsNo column, so previously fetched EC results no longer correspond to
    // the (now different) part number. Clear supplier from all rows (mirrors setColumnRole).
    // Deleting the 発注先列 only shifts the ORDER gate to the fallback column, which
    // supplierActive re-evaluates live, so supplier need not be cleared there.
    const removingPartNo = partNoColumn(doc)?.key === key;
    const columns = doc.columns.filter((c) => c.key !== key);
    const rows = removingPartNo
      ? doc.rows.map((r) => (r.supplier ? { ...r, supplier: undefined } : r))
      : doc.rows;
    setDoc({ ...doc, columns, rows });
  };

  // Designate which column plays a fetch role (型番列 / EC発注先列). Each role has at most
  // one column, so assigning it clears any previous holder. A role column is a fetch key,
  // never an EC fill target, so we also drop any EC link it may have carried.
  const setColumnRole = (role: ColumnRole, key: string) => {
    if (!doc) return;
    const prevPartKey = partNoColumn(doc)?.key;
    const columns = doc.columns.map((c) => {
      if (c.key === key) return { ...c, role, link: undefined };
      if (c.role === role) return { ...c, role: undefined };
      return c;
    });
    // Reassigning the 型番列 changes the fetch identity — previously fetched EC results no
    // longer correspond to the new part number, so clear supplier from all rows (re-fetch
    // resets them). Reassigning the 発注先列 doesn't change identity (supplierActive gates
    // it live), so leave supplier intact there.
    const rows =
      role === "partNo" && prevPartKey !== key
        ? doc.rows.map((r) => (r.supplier ? { ...r, supplier: undefined } : r))
        : doc.rows;
    setDoc({ ...doc, columns, rows });
  };

  // Map an EC field to a target column (field-anchored, per plan §7.3「フィールドを列に結ぶ」).
  // Each field binds at most one column; binding clears the field from any previous holder.
  // columnKey === null unbinds the field. Applied immediately to current supplier results.
  const setFieldLink = (field: string, columnKey: string | null, write: WritePolicy) => {
    if (!doc) return;
    const columns = doc.columns.map((c) => {
      if (c.link?.field === field && c.key !== columnKey) return { ...c, link: undefined };
      if (columnKey && c.key === columnKey) return { ...c, link: { field, write } };
      return c;
    });
    setDoc(applyLinkedColumns({ ...doc, columns }));
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
    const partCol = partNoColumn(doc);
    const srcCol = sourceColumn(doc);
    if (!partCol || !srcCol) {
      setStatus("型番列 / EC発注先列 が未設定です（列の管理で設定）");
      return;
    }
    const partOf = (r: BomRow) => String(getCellValue(r, partCol) ?? "").trim();
    const isMisumi = (r: BomRow) =>
      String(getCellValue(r, srcCol) ?? "")
        .trim()
        .toUpperCase() === "MISUMI";
    const targets = doc.rows.filter((r) => isMisumi(r) && partOf(r) !== "");
    if (targets.length === 0) {
      setStatus("EC発注先=MISUMI かつ 型番のある行がありません");
      return;
    }
    setQuoting(true);
    setStatus("取得を開始しています…");
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await api.onQuoteProgress((p) =>
        setStatus(p.total > 0 ? `取得中 ${p.done}/${p.total}…` : "キャッシュから取得中…"),
      );
      const items = targets.map((r) => ({ partNo: partOf(r) }));
      const quotes = await api.quote("MISUMI", items, force);
      const byId = new Map<string, SupplierQuote>();
      // Store the raw quote only. Subtotal and the MOQ note are derived LIVE in the
      // supplier column getters from the row's current Qty/multiplier, so they stay
      // correct after the user edits Qty (no stale stamped values).
      targets.forEach((r, idx) => {
        const q = quotes[idx];
        if (q) byId.set(r.id, q);
      });
      // Apply fresh quotes to targets; clear stale supplier data from rows that are no
      // longer MISUMI targets (ORDER changed away / Parts No removed) so re-fetch resets them.
      setDoc((d) => {
        if (!d) return d;
        const rows = d.rows.map((r) =>
          byId.has(r.id)
            ? { ...r, supplier: byId.get(r.id) }
            : r.supplier
              ? { ...r, supplier: undefined }
              : r,
        );
        // Apply linked-column write policies (fillEmpty/overwrite) with the fresh results.
        return applyLinkedColumns({ ...d, rows });
      });
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
      const saved = { ...doc, id };
      setDoc(saved);
      savedSnapRef.current = JSON.stringify(saved);
      setStatus("保存しました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doBack = async () => {
    setConfirmBack(false);
    setView("list");
    setDoc(null);
    setStatus("");
    await reloadList();
  };

  // Leaving the editor with unsaved changes prompts 保存/破棄/キャンセル (Windows convention).
  const back = () => {
    if (doc && JSON.stringify(doc) !== savedSnapRef.current) {
      setConfirmBack(true);
      return;
    }
    void doBack();
  };

  const saveAndBack = async () => {
    if (!doc) return;
    try {
      await api.bomSave(doc);
    } catch (e) {
      setConfirmBack(false);
      setStatus(String(e));
      return;
    }
    await doBack();
  };

  // Import: pick an Excel/CSV file, parse it to a grid, and open the mapping wizard.
  const doImport = async () => {
    try {
      const picked = await api.pickSpreadsheetToOpen();
      if (!picked) return;
      setStatus("ファイルを読み込み中…");
      const workbook = await api.readSpreadsheet(picked.path);
      setImportSrc({ workbook, fileName: picked.name, path: picked.path });
      setStatus("");
    } catch (e) {
      setStatus(String(e));
    }
  };

  // Wizard confirmed: persist the built BomDoc as a new BOM and open it. When the user opted
  // in, open 列管理 on the 列の構成 tab — imported BOMs have no EC columns yet, and that tab
  // is where "EC連携項目を列に追加" lives (the 取得・連携 tab only links to existing columns).
  const confirmImport = async (built: BomDoc, openEcSetup: boolean) => {
    setImportSrc(null);
    try {
      const id = await api.bomSave(built);
      await reloadList();
      await openBom(id);
      setStatus(`取込しました（${built.rows.length} 行）`);
      if (openEcSetup) {
        setColumnsTab("columns");
        setShowColumns(true);
      }
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doExport = async () => {
    if (!doc) return;
    try {
      const { headers, rows } = buildExportGrid(doc);
      if (await api.exportSpreadsheet(doc, headers, rows)) setStatus("書き出しました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  // Resolve what the history drawer should show for a row: only rows whose ORDER is an
  // EC-supported source (currently just MISUMI) have history; otherwise carry the reason so
  // the drawer can explain it (not-ec / no-part / no-row) rather than "まだありません".
  const historyTargetOf = (row: BomRow | null): HistoryTarget => {
    if (!row || !doc) return { kind: "empty", reason: "no-row" };
    const srcCol = sourceColumn(doc);
    const src = String((srcCol ? getCellValue(row, srcCol) : row.order) ?? "")
      .trim()
      .toUpperCase();
    if (!SUPPORTED_EC.includes(src)) return { kind: "empty", reason: "not-ec" };
    const partCol = partNoColumn(doc);
    const partNo = partCol ? String(getCellValue(row, partCol) ?? "").trim() : "";
    if (!partNo) return { kind: "empty", reason: "no-part" };
    return { kind: "row", partNo, supplier: src };
  };

  // The row the history button should act on. Prefer the user's SELECTION; only use the
  // grid's focused row when there is no selection, or when the focused row is itself part of
  // the selection. This avoids opening a different row's history than the one the user picked
  // (e.g. after a Ctrl / multi-row selection leaves focus on a non-selected row).
  const historyButtonRow = (): BomRow | null => {
    const gridApi = gridRef.current?.api;
    if (!gridApi) return null;
    const selected = gridApi.getSelectedRows();
    const fc = gridApi.getFocusedCell();
    const focusedRow = fc ? (gridApi.getDisplayedRowAtIndex(fc.rowIndex)?.data ?? null) : null;
    if (focusedRow && (selected.length === 0 || selected.some((r) => r.id === focusedRow.id))) {
      return focusedRow;
    }
    return selected[0] ?? focusedRow ?? null;
  };

  // Toolbar toggle for the history drawer. When opening, seed it with the selected/focused
  // row so it shows something immediately; while open it follows the active cell via
  // onActiveRowChange.
  const toggleHistory = () => {
    if (historyOpen) {
      setHistoryOpen(false);
      return;
    }
    setActiveTarget(historyTargetOf(historyButtonRow()));
    setHistoryOpen(true);
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
            onManageColumns={() => {
              setColumnsTab("columns");
              setShowColumns(true);
            }}
            onHistory={toggleHistory}
            historyOpen={historyOpen}
            onExport={doExport}
            onRename={(name) => setDoc({ ...doc, meta: { ...doc.meta, name } })}
            onQuickFilter={setQuickFilter}
            onUndo={undo}
            onRedo={redo}
            onAutoSize={autoSize}
            onQuote={runQuote}
            quoting={quoting}
          />
          <BomEditor
            doc={doc}
            onChange={setDoc}
            gridRef={gridRef}
            quickFilter={quickFilter}
            onActiveRowChange={(row) => setActiveTarget(historyTargetOf(row))}
          />
          {historyOpen && (
            <HistoryDrawer target={activeTarget} onClose={() => setHistoryOpen(false)} />
          )}
          <SummaryBar doc={doc} />
          {confirmBack && (
            <div className="col-mgr-backdrop" onClick={() => setConfirmBack(false)}>
              <div className="confirm-dlg" onClick={(e) => e.stopPropagation()}>
                <h3>変更が保存されていません</h3>
                <p>
                  「{doc.meta.name?.trim() || "無題の BOM"}」への変更を保存しますか？
                  <br />
                  保存せずに戻ると、編集内容と取得結果は失われます。
                </p>
                <div className="confirm-actions">
                  <button className="primary" onClick={saveAndBack}>
                    保存して戻る
                  </button>
                  <button onClick={doBack}>保存せずに戻る</button>
                  <button onClick={() => setConfirmBack(false)}>キャンセル</button>
                </div>
              </div>
            </div>
          )}
          {showColumns && (
            <ColumnManager
              columns={doc.columns}
              initialTab={columnsTab}
              onAdd={addColumn}
              onAddSupplier={addSupplierColumn}
              onSetFieldLink={setFieldLink}
              onSetRole={setColumnRole}
              onRename={renameColumn}
              onDelete={deleteColumn}
              onMove={moveColumn}
              onClose={() => setShowColumns(false)}
            />
          )}
        </>
      )}
      {importSrc && (
        <ImportWizard
          workbook={importSrc.workbook}
          fileName={importSrc.fileName}
          path={importSrc.path}
          onCancel={() => setImportSrc(null)}
          onConfirm={confirmImport}
        />
      )}
    </div>
  );
}
