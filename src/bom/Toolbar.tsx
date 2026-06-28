interface Props {
  title: string;
  status: string;
  onBack: () => void;
  onSave: () => void;
  onAddRow: () => void;
  onDupRows: () => void;
  onDelRows: () => void;
  onRenumber: () => void;
  onManageColumns: () => void;
  onExport: () => void;
  onRename: (name: string) => void;
}

export function Toolbar(p: Props) {
  return (
    <div className="toolbar">
      <button onClick={p.onBack}>← 一覧</button>
      <input
        className="bom-name"
        value={p.title}
        onChange={(e) => p.onRename(e.currentTarget.value)}
        placeholder="BOM 名"
      />
      <span className="sep" />
      <button onClick={p.onAddRow}>行追加</button>
      <button onClick={p.onDupRows}>複製</button>
      <button onClick={p.onDelRows}>行削除</button>
      <button onClick={p.onRenumber}>No.振り直し</button>
      <span className="sep" />
      <button onClick={p.onManageColumns}>列管理</button>
      <span className="sep" />
      <button className="primary" onClick={p.onSave}>
        保存
      </button>
      <button onClick={p.onExport}>JSON書出</button>
      {p.status && <span className="status-msg">{p.status}</span>}
    </div>
  );
}
