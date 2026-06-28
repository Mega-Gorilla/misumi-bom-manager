import { useState } from "react";
import { X, ChevronUp, ChevronDown, Trash2, Plus } from "lucide-react";
import type { ColumnDef } from "../types/bom";

interface Props {
  columns: ColumnDef[];
  onAdd: (label: string) => void;
  onRename: (key: string, label: string) => void;
  onDelete: (key: string) => void;
  onMove: (key: string, dir: -1 | 1) => void;
  onClose: () => void;
}

export function ColumnManager(p: Props) {
  const [newLabel, setNewLabel] = useState("");

  const add = () => {
    const v = newLabel.trim();
    if (!v) return;
    p.onAdd(v);
    setNewLabel("");
  };

  return (
    <div className="col-mgr-backdrop" onClick={p.onClose}>
      <div className="col-mgr" onClick={(e) => e.stopPropagation()}>
        <div className="col-mgr-head">
          <strong>列の管理</strong>
          <button className="icon-btn" onClick={p.onClose} title="閉じる">
            <X size={16} />
          </button>
        </div>

        <ul className="col-list">
          {p.columns.map((c, i) => (
            <li key={c.key}>
              <input
                className="col-label"
                value={c.label}
                onChange={(e) => p.onRename(c.key, e.currentTarget.value)}
              />
              <span className={`kind kind-${c.kind}`}>{c.kind}</span>
              <button
                className="icon-btn"
                disabled={i === 0}
                onClick={() => p.onMove(c.key, -1)}
                title="上へ"
              >
                <ChevronUp size={15} />
              </button>
              <button
                className="icon-btn"
                disabled={i === p.columns.length - 1}
                onClick={() => p.onMove(c.key, 1)}
                title="下へ"
              >
                <ChevronDown size={15} />
              </button>
              <button
                className="icon-btn danger"
                disabled={c.kind !== "custom"}
                title={c.kind !== "custom" ? "core / supplier 列は削除不可" : "削除"}
                onClick={() => p.onDelete(c.key)}
              >
                <Trash2 size={15} />
              </button>
            </li>
          ))}
        </ul>

        <div className="col-add">
          <input
            placeholder="新しい列名（任意列）"
            value={newLabel}
            onChange={(e) => setNewLabel(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") add();
            }}
          />
          <button className="primary" disabled={!newLabel.trim()} onClick={add}>
            <Plus size={15} /> 列を追加
          </button>
        </div>
      </div>
    </div>
  );
}
