import { useCallback, useEffect, useRef, useState } from "react";
import type { AgGridReact } from "ag-grid-react";
import "./App.css";
import type { BomDoc, BomRow, BomSummary } from "./types/bom";
import { newBom, newRow } from "./types/bom";
import * as api from "./api/bom";
import { BomEditor } from "./bom/BomEditor";
import { Toolbar } from "./bom/Toolbar";
import { BomList } from "./bom/BomList";

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
    setDoc((d) => (d ? { ...d, rows: [...d.rows, newRow(d.rows.length + 1)] } : d));

  const dupRows = () => {
    const sel = selectedRows();
    if (!doc || sel.length === 0) {
      setStatus("複製する行を選択してください");
      return;
    }
    const clones = sel.map((r) => ({ ...r, id: crypto.randomUUID() }));
    setDoc({ ...doc, rows: [...doc.rows, ...clones] });
  };

  const delRows = () => {
    const ids = new Set(selectedRows().map((r) => r.id));
    if (!doc || ids.size === 0) {
      setStatus("削除する行を選択してください");
      return;
    }
    setDoc({ ...doc, rows: doc.rows.filter((r) => !ids.has(r.id)) });
  };

  const addColumn = () => {
    if (!doc) return;
    const label = window.prompt("追加する列名");
    if (!label) return;
    const key = uniqueKey(doc, slug(label));
    setDoc({
      ...doc,
      columns: [...doc.columns, { key, label, kind: "custom", editable: true, width: 140 }],
    });
  };

  const renameColumn = () => {
    if (!doc) return;
    const key = window.prompt(`改名する列のキー\n(${doc.columns.map((c) => c.key).join(", ")})`);
    if (!key) return;
    if (!doc.columns.some((c) => c.key === key)) {
      setStatus(`列が見つかりません: ${key}`);
      return;
    }
    const label = window.prompt("新しい列名");
    if (!label) return;
    setDoc({ ...doc, columns: doc.columns.map((c) => (c.key === key ? { ...c, label } : c)) });
  };

  const deleteColumn = () => {
    if (!doc) return;
    const custom = doc.columns.filter((c) => c.kind === "custom").map((c) => c.key);
    if (custom.length === 0) {
      setStatus("削除できる任意列がありません（core 列は削除不可）");
      return;
    }
    const key = window.prompt(`削除する任意列のキー\n(${custom.join(", ")})`);
    if (!key) return;
    if (!custom.includes(key)) {
      setStatus("core 列は削除できません（任意列のみ削除可）");
      return;
    }
    setDoc({ ...doc, columns: doc.columns.filter((c) => c.key !== key) });
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
            onBack={back}
            onSave={saveBom}
            onAddRow={addRow}
            onDupRows={dupRows}
            onDelRows={delRows}
            onAddCol={addColumn}
            onRenameCol={renameColumn}
            onDelCol={deleteColumn}
            onExport={doExport}
            onRename={(name) => setDoc({ ...doc, meta: { ...doc.meta, name } })}
          />
          <BomEditor doc={doc} onChange={setDoc} gridRef={gridRef} />
        </>
      )}
    </div>
  );
}
