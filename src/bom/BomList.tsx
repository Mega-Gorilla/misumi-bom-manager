import type { BomSummary } from "../types/bom";

interface Props {
  list: BomSummary[];
  status: string;
  onOpen: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => void;
  onImport: () => void;
}

export function BomList(p: Props) {
  return (
    <div className="list-view">
      <header className="list-head">
        <h1>BOM 一覧</h1>
        <div className="actions">
          <button className="primary" onClick={p.onNew}>
            新規 BOM
          </button>
          <button onClick={p.onImport}>JSON 取込</button>
        </div>
      </header>

      {p.status && <div className="status-msg">{p.status}</div>}

      {p.list.length === 0 ? (
        <p className="empty">
          BOM がありません。「新規 BOM」または「JSON 取込」で作成してください。
        </p>
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
            {p.list.map((b) => (
              <tr key={b.id}>
                <td>
                  <button className="link" onClick={() => p.onOpen(b.id)}>
                    {b.name || "(無題)"}
                  </button>
                </td>
                <td className="num">{b.rowCount}</td>
                <td className="muted">{b.updatedAt ?? ""}</td>
                <td>
                  <button
                    className="danger"
                    onClick={() => {
                      if (confirm("この BOM を削除しますか？")) p.onDelete(b.id);
                    }}
                  >
                    削除
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
