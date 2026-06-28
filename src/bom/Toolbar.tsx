interface Props {
  title: string;
  status: string;
  quickFilter: string;
  onBack: () => void;
  onSave: () => void;
  onAddRow: () => void;
  onDupRows: () => void;
  onDelRows: () => void;
  onRenumber: () => void;
  onManageColumns: () => void;
  onExport: () => void;
  onRename: (name: string) => void;
  onQuickFilter: (text: string) => void;
  onUndo: () => void;
  onRedo: () => void;
  onAutoSize: () => void;
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
      <input
        className="quick-filter"
        value={p.quickFilter}
        onChange={(e) => p.onQuickFilter(e.currentTarget.value)}
        placeholder="🔍 検索"
      />
      <span className="sep" />
      <button onClick={p.onAddRow}>行追加</button>
      <button onClick={p.onDupRows}>複製</button>
      <button onClick={p.onDelRows}>行削除</button>
      <button onClick={p.onRenumber}>No.振り直し</button>
      <span className="sep" />
      <button onClick={p.onUndo} title="元に戻す (Ctrl+Z)">↶</button>
      <button onClick={p.onRedo} title="やり直し (Ctrl+Y)">↷</button>
      <span className="sep" />
      <button onClick={p.onManageColumns}>列管理</button>
      <button onClick={p.onAutoSize} title="列幅を内容に合わせる">列幅自動</button>
      <span className="sep" />
      <button className="primary" onClick={p.onSave}>
        保存
      </button>
      <button onClick={p.onExport}>JSON書出</button>
      {p.status && <span className="status-msg">{p.status}</span>}
    </div>
  );
}
