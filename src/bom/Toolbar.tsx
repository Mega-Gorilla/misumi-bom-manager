import {
  ArrowLeft,
  Plus,
  Copy,
  Trash2,
  ListOrdered,
  Undo2,
  Redo2,
  Columns3,
  MoveHorizontal,
  Save,
  Download,
  Search,
  DownloadCloud,
  RefreshCw,
} from "lucide-react";

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
  onQuote: (force: boolean) => void;
  quoting: boolean;
}

const ICON = 16;

export function Toolbar(p: Props) {
  return (
    <div className="toolbar">
      <button className="icon-btn" onClick={p.onBack} title="一覧へ戻る">
        <ArrowLeft size={ICON} />
      </button>
      <input
        className="bom-name"
        value={p.title}
        onChange={(e) => p.onRename(e.currentTarget.value)}
        placeholder="BOM 名"
      />
      <div className="search-box">
        <Search size={15} />
        <input
          className="quick-filter"
          value={p.quickFilter}
          onChange={(e) => p.onQuickFilter(e.currentTarget.value)}
          placeholder="検索"
        />
      </div>

      <span className="sep" />
      <button onClick={p.onAddRow}>
        <Plus size={ICON} /> 行追加
      </button>
      <button onClick={p.onDupRows}>
        <Copy size={ICON} /> 複製
      </button>
      <button onClick={p.onDelRows}>
        <Trash2 size={ICON} /> 行削除
      </button>
      <button onClick={p.onRenumber} title="No. を 1 から振り直す">
        <ListOrdered size={ICON} /> No.振り直し
      </button>

      <span className="sep" />
      <button className="icon-btn" onClick={p.onUndo} title="元に戻す (Ctrl+Z)">
        <Undo2 size={ICON} />
      </button>
      <button className="icon-btn" onClick={p.onRedo} title="やり直し (Ctrl+Y)">
        <Redo2 size={ICON} />
      </button>

      <span className="sep" />
      <button onClick={p.onManageColumns}>
        <Columns3 size={ICON} /> 列管理
      </button>
      <button className="icon-btn" onClick={p.onAutoSize} title="列幅を内容に合わせる">
        <MoveHorizontal size={ICON} />
      </button>

      <span className="sep" />
      <button
        onClick={() => p.onQuote(false)}
        disabled={p.quoting}
        title="ORDER=MISUMI の行を取得（本日取得済みはキャッシュを使用。日付が変わった型番・未取得のみ再取得）"
      >
        <DownloadCloud size={ICON} /> MISUMI 一括取得
      </button>
      <button
        className="icon-btn"
        onClick={() => p.onQuote(true)}
        disabled={p.quoting}
        title="最新化（キャッシュを無視して全件を再取得）"
      >
        <RefreshCw size={ICON} />
      </button>

      <span className="sep" />
      <button className="primary" onClick={p.onSave}>
        <Save size={ICON} /> 保存
      </button>
      <button onClick={p.onExport} title="JSON で書き出し">
        <Download size={ICON} /> JSON
      </button>

      {p.status && <span className="status-msg">{p.status}</span>}
    </div>
  );
}
