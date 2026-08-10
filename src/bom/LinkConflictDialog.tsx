// 競合解決ダイアログ (§4.2.2: 外部版はバックアップに保全・自動復元しない)。
// 「フォルダを開く」はフロントの opener 責務 (implementation.md §2.3)。解決の記録
// (mark_resolved) だけが §1.3 の同期停止を解除する。

import { FolderOpen } from "lucide-react";
import type { ConflictInfo } from "../types/link";
import { CONFLICT_GUIDANCE } from "./LinkBanner";

interface Props {
  conflicts: ConflictInfo[];
  busy: boolean;
  onResolve: (backupId: number) => void;
  onOpenFolder: (backupPath: string) => void;
  onClose: () => void;
}

export function LinkConflictDialog(p: Props) {
  return (
    <div className="col-mgr-backdrop" onClick={p.onClose}>
      <div className="confirm-dlg link-conflict" onClick={(e) => e.stopPropagation()}>
        <h3>外部の変更と競合しました</h3>
        <p className="col-note">{CONFLICT_GUIDANCE}</p>
        {p.conflicts.length === 0 && (
          <p className="col-note">未解決の競合はありません。</p>
        )}
        <div className="lc-candidates">
          {p.conflicts.map((c) => (
            <div key={c.backupId} className="lk-conflict-row">
              <div className="lk-conflict-info">
                <div>{c.createdAt} の反映で退避した外部版</div>
                <div className="lk-conflict-path" title={c.backupPath}>
                  {c.backupPath}
                  {!c.transferred && "（ワークブックと同じフォルダに未移送のまま残っています）"}
                </div>
              </div>
              <button onClick={() => p.onOpenFolder(c.backupPath)} title="バックアップの場所を開く">
                <FolderOpen size={14} /> フォルダを開く
              </button>
              <button
                className="primary"
                disabled={p.busy}
                onClick={() => p.onResolve(c.backupId)}
                title="内容を確認済みとして競合を解決し、同期を再開します"
              >
                解決済みにする
              </button>
            </div>
          ))}
        </div>
        <div className="confirm-actions">
          <button onClick={p.onClose} disabled={p.busy}>
            閉じる
          </button>
        </div>
      </div>
    </div>
  );
}
