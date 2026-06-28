interface Props {
  title: string;
  status: string;
  onBack: () => void;
  onSave: () => void;
  onAddRow: () => void;
  onDupRows: () => void;
  onDelRows: () => void;
  onAddCol: () => void;
  onRenameCol: () => void;
  onDelCol: () => void;
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
      <span className="sep" />
      <button onClick={p.onAddCol}>列追加</button>
      <button onClick={p.onRenameCol}>列改名</button>
      <button onClick={p.onDelCol}>列削除</button>
      <span className="sep" />
      <button className="primary" onClick={p.onSave}>
        保存
      </button>
      <button onClick={p.onExport}>JSON書出</button>
      {p.status && <span className="status-msg">{p.status}</span>}
    </div>
  );
}
