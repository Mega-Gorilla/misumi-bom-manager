import { useState } from "react";
import { X, ChevronUp, ChevronDown, Trash2, Plus } from "lucide-react";
import type { ColumnDef, WritePolicy } from "../types/bom";
import { SUPPLIER_FIELDS, WRITE_POLICIES } from "../types/bom";

interface Props {
  columns: ColumnDef[];
  onAdd: (label: string) => void;
  onAddSupplier: (field: string, label: string) => void;
  onSetLink: (key: string, field: string | null, write: WritePolicy) => void;
  onRename: (key: string, label: string) => void;
  onDelete: (key: string) => void;
  onMove: (key: string, dir: -1 | 1) => void;
  onClose: () => void;
}

// Supplier data fields that can drive an existing editable column (write policy).
const LINKABLE = SUPPLIER_FIELDS.filter((f) => f.linkable);

// User-facing labels for the column kind (the raw "core/custom/supplier" is unclear).
const KIND_LABEL: Record<string, string> = {
  core: "基本",
  custom: "任意",
  supplier: "取得データ",
};
const KIND_TITLE: Record<string, string> = {
  core: "基本列（BOM の標準項目）",
  custom: "任意列（ユーザー追加）",
  supplier: "取得列（MISUMI 取得データ・読取専用）",
};

// Editable columns that may be linked to a MISUMI field. ORDER is excluded (it is the
// supplier dispatch / gate / dropdown key, not a fill target). Parts No is allowed so
// it can be normalized to MISUMI's canonical part number.
function canLink(c: ColumnDef): boolean {
  return c.kind !== "supplier" && c.editable && c.key !== "order";
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

        <table className="col-table">
          <thead>
            <tr>
              <th>列名</th>
              <th>種別</th>
              <th>EC連携</th>
              <th>書込</th>
              <th className="col-th-actions">並べ替え / 削除</th>
            </tr>
          </thead>
          <tbody>
            {p.columns.map((c, i) => (
              <tr key={c.key}>
                <td>
                  <input
                    className="col-label"
                    value={c.label}
                    onChange={(e) => p.onRename(c.key, e.currentTarget.value)}
                  />
                </td>
                <td>
                  <span className={`kind kind-${c.kind}`} title={KIND_TITLE[c.kind] ?? c.kind}>
                    {KIND_LABEL[c.kind] ?? c.kind}
                  </span>
                </td>
                <td>
                  {canLink(c) ? (
                    <select
                      value={c.link?.field ?? ""}
                      onChange={(e) =>
                        p.onSetLink(
                          c.key,
                          e.currentTarget.value || null,
                          c.link?.write ?? "fillEmpty",
                        )
                      }
                    >
                      <option value="">連携なし</option>
                      {LINKABLE.map((f) => (
                        <option key={f.field} value={f.field}>
                          {f.label}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="dash">—</span>
                  )}
                </td>
                <td>
                  {canLink(c) && c.link ? (
                    <select
                      value={c.link.write}
                      onChange={(e) =>
                        p.onSetLink(c.key, c.link!.field, e.currentTarget.value as WritePolicy)
                      }
                      title="書込ポリシー"
                    >
                      {WRITE_POLICIES.map((w) => (
                        <option key={w.value} value={w.value}>
                          {w.label}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="dash">—</span>
                  )}
                </td>
                <td>
                  <div className="col-actions">
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
                      disabled={c.kind === "core"}
                      title={c.kind === "core" ? "core 列は削除不可" : "削除"}
                      onClick={() => p.onDelete(c.key)}
                    >
                      <Trash2 size={15} />
                    </button>
                  </div>
                </td>
              </tr>
            ))}
          </tbody>
        </table>

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

        <div className="supplier-fields">
          <div className="supplier-fields-head">EC連携項目を列に追加</div>
          <div className="supplier-fields-list">
            {SUPPLIER_FIELDS.map((f) => {
              const added = p.columns.some(
                (c) => c.kind === "supplier" && c.link?.field === f.field,
              );
              return (
                <button
                  key={f.field}
                  disabled={added}
                  title={added ? "追加済み" : `${f.field} を列として追加`}
                  onClick={() => p.onAddSupplier(f.field, f.label)}
                >
                  <Plus size={13} /> {f.label}
                </button>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
