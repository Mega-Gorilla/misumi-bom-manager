import { useMemo, useState } from "react";
import { FilePlus2, Upload, Trash2, FileSpreadsheet, Search } from "lucide-react";
import type { BomSummary } from "../types/bom";

interface Props {
  list: BomSummary[];
  status: string;
  onOpen: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => void;
  onImport: () => void;
}

type SortKey = "updated" | "name" | "rows";

export function BomList(p: Props) {
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<SortKey>("updated");

  // Search (name, case-insensitive) + sort. Client-side — fine for the expected
  // handful-to-dozens of BOMs; move to a SQL WHERE/ORDER BY if the library grows large.
  const shown = useMemo(() => {
    const q = query.trim().toLowerCase();
    const filtered = q
      ? p.list.filter((b) => (b.name ?? "").toLowerCase().includes(q))
      : p.list;
    const sorted = [...filtered].sort((a, b) => {
      if (sort === "name") return (a.name ?? "").localeCompare(b.name ?? "", "ja");
      if (sort === "rows") return b.rowCount - a.rowCount;
      return (b.updatedAt ?? "").localeCompare(a.updatedAt ?? ""); // updated, newest first
    });
    return sorted;
  }, [p.list, query, sort]);

  return (
    <div className="list-view">
      <header className="list-head">
        <h1>BOM 一覧</h1>
        <div className="actions">
          <button className="primary" onClick={p.onNew}>
            <FilePlus2 size={16} /> 新規 BOM
          </button>
          <button onClick={p.onImport} title="Excel / CSV から取込">
            <Upload size={16} /> 取込 (Excel/CSV)
          </button>
        </div>
      </header>

      {p.list.length > 0 && (
        <div className="list-tools">
          <div className="search-box">
            <Search size={15} />
            <input
              className="quick-filter"
              value={query}
              onChange={(e) => setQuery(e.currentTarget.value)}
              placeholder="名前で検索"
            />
          </div>
          <label className="list-sort">
            並べ替え
            <select value={sort} onChange={(e) => setSort(e.currentTarget.value as SortKey)}>
              <option value="updated">更新日（新しい順）</option>
              <option value="name">名前（昇順）</option>
              <option value="rows">行数（多い順）</option>
            </select>
          </label>
          <span className="list-count">
            {shown.length === p.list.length
              ? `${p.list.length} 件`
              : `${shown.length} 件 / 全 ${p.list.length} 件`}
          </span>
        </div>
      )}

      {p.status && <div className="status-msg">{p.status}</div>}

      {p.list.length === 0 ? (
        <p className="empty">
          BOM がありません。「新規 BOM」または「取込 (Excel/CSV)」で作成してください。
        </p>
      ) : shown.length === 0 ? (
        <p className="empty">「{query}」に一致する BOM はありません。</p>
      ) : (
        <table className="bom-table">
          <thead>
            <tr>
              <th>名前</th>
              <th className="num">行数</th>
              <th>更新</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {shown.map((b) => (
              <tr key={b.id}>
                <td>
                  <button className="link" onClick={() => p.onOpen(b.id)}>
                    <FileSpreadsheet size={15} />
                    {b.name || "(無題)"}
                  </button>
                </td>
                <td className="num">{b.rowCount}</td>
                <td className="muted">{(b.updatedAt ?? "").slice(0, 16)}</td>
                <td>
                  <button
                    className="icon-btn danger"
                    title="削除"
                    onClick={() => {
                      if (confirm("この BOM を削除しますか？")) p.onDelete(b.id);
                    }}
                  >
                    <Trash2 size={15} />
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
