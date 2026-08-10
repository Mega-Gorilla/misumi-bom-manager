// リンク BOM の常設ステータスバー (implementation.md §4 の DoD)。
// sync/calc バッジ・反映待ち・競合/要確認/破綻の導線・環境降格・同時編集非対応の
// 常設インジケータ・stale/missing の業務利用不可警告 (§9-9) をまとめて表示する。

import { useEffect, useState } from "react";
import { AlertTriangle, FolderOpen, Info, Link2, RefreshCw, Users } from "lucide-react";
import type {
  CalcState,
  EnvVerdict,
  LinkStatus,
  PendingInfo,
  StructureVerdict,
  SyncStatus,
} from "../types/link";
import { CALC_STATE_LABEL, SYNC_STATUS_LABEL, calcUsable } from "../types/link";

/** implementation.md §4 の文言案 (同時編集非対応の運用警告)。 */
export const CONCURRENT_EDIT_WARNING =
  "このファイルの同時編集には対応していません。複数の PC・Web で同時に編集すると、" +
  "あとから同期された変更が本体になり、他方はクラウドの版履歴のみに残ります" +
  "（アプリは検出できない場合があります）。編集は1人ずつ行ってください。";

/** implementation.md §4 の文言案 (競合検出時の復元導線)+Sheets 誤検出の説明。 */
export const CONFLICT_GUIDANCE =
  "外部の変更と競合しました。外部の版はバックアップに保全されています。" +
  "クラウド同期をお使いの場合は、Web の版履歴からも過去の版を復元できます。" +
  "（Google スプレッドシート等で開いた場合も変更として検出されることがあります — 安全側の動作です）";

interface Props {
  syncStatus: SyncStatus;
  calcState: CalcState;
  envVerdict: EnvVerdict;
  verdict: StructureVerdict;
  pending?: PendingInfo;
  /** ポーリング中の軽量ステータス (無ければ view 由来の値のみで表示)。 */
  status?: LinkStatus;
  warnings: string[];
  /** §4.8: リンク BOM でも編集可な DB 所有メタ (小計・カート数量に効く)。 */
  qtyMultiplier: number;
  onQtyMultiplier: (mult: number) => void;
  onOpenConfirm: () => void;
  onOpenConflict: () => void;
  onRemap: () => void;
  onApply: () => void;
}

const SYNC_CLASS: Record<SyncStatus, string> = {
  linked: "lb-ok",
  needs_review: "lb-warn",
  broken: "lb-error",
  conflict: "lb-error",
};

export function LinkBanner(p: Props) {
  const usable = calcUsable(p.calcState);
  const excelOpenHint = p.status?.excelLockHint ?? false;
  const recovery = p.status?.recoveryPending ?? false;
  const untransferred = p.status?.untransferred ?? 0;
  // 倍率はローカル編集 → blur/Enter で確定 (毎キーストローク保存を避ける)。
  const [multText, setMultText] = useState(String(p.qtyMultiplier));
  useEffect(() => setMultText(String(p.qtyMultiplier)), [p.qtyMultiplier]);
  const commitMult = () => {
    const m = Number(multText);
    if (Number.isFinite(m) && m > 0 && m !== p.qtyMultiplier) p.onQtyMultiplier(m);
    else setMultText(String(p.qtyMultiplier));
  };
  return (
    <div className="link-banner">
      <span className="lb-badge lb-link" title="Excel リンク BOM（編集は Excel に集約）">
        <Link2 size={13} /> リンク
      </span>
      <span className={`lb-badge ${SYNC_CLASS[p.syncStatus]}`}>
        {SYNC_STATUS_LABEL[p.syncStatus]}
      </span>
      <span
        className={`lb-badge ${usable ? "lb-muted" : "lb-warn"}`}
        title={
          p.calcState === "unverified"
            ? "数式値は Excel が保存した時点の計算結果です（業務利用可・未検証）"
            : p.calcState === "stale"
              ? "アプリが書き込んだ後、Excel での再計算・保存を確認できていません"
              : p.calcState === "missing"
                ? "数式セルに計算値がありません（Excel で開いて保存してください）"
                : "Excel 保存後の再読込で計算結果を確認済みです"
        }
      >
        数式: {CALC_STATE_LABEL[p.calcState]}
      </span>
      {p.envVerdict === "no_writeback" && (
        <span className="lb-badge lb-warn" title="環境判定により書き戻しは無効です（読み取りは可能）">
          書き戻し不可
        </span>
      )}
      {excelOpenHint && (
        <span
          className="lb-badge lb-muted"
          title="~$ ファイルを検出しました。Excel がこのファイルを開いている可能性があります（目安であり確定ではありません）"
        >
          Excel 使用中?
        </span>
      )}
      {untransferred > 0 && (
        <span
          className="lb-badge lb-warn"
          title="ワークブックと同じフォルダに退避したままのバックアップがあります。次回の「更新」「Excel へ反映」で自動的にアプリ領域へ移送を再試行します"
        >
          未移送バックアップ {untransferred}
        </span>
      )}
      <label className="lb-mult" title="数量倍率（小計・カート投入数量に掛かります。リンク BOM でも編集できる設定です）">
        倍率 ×
        <input
          value={multText}
          onChange={(e) => setMultText(e.currentTarget.value)}
          onBlur={commitMult}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") setMultText(String(p.qtyMultiplier));
          }}
        />
      </label>

      <span className="lb-msgs">
        {!usable && (
          <span className="lb-msg lb-msg-warn">
            <AlertTriangle size={13} /> 未再計算 — 数式値は業務利用できません（EC
            取得・カート投入は停止中）。Excel で開いて保存 → 「更新」で解消します
          </span>
        )}
        {p.pending && (
          <span className="lb-msg">
            反映待ち（第{p.pending.requestedGeneration}世代
            {p.pending.blockedReason === "file_open" ? "・Excel が開いています" : ""}）
            — Excel を閉じてから
            <button className="link" onClick={p.onApply}>
              <RefreshCw size={12} /> 再実行
            </button>
          </span>
        )}
        {p.syncStatus === "conflict" && (
          <span className="lb-msg lb-msg-error">
            <AlertTriangle size={13} /> 外部の変更と競合しました
            <button className="link" onClick={p.onOpenConflict}>
              <FolderOpen size={12} /> 確認・解決
            </button>
          </span>
        )}
        {p.syncStatus === "needs_review" && p.verdict.kind === "confirm" && (
          <span className="lb-msg lb-msg-warn">
            構造の変更を検出しました（同期停止中）
            <button className="link" onClick={p.onOpenConfirm}>
              確認して確定
            </button>
          </span>
        )}
        {p.syncStatus === "broken" && (
          <span className="lb-msg lb-msg-error">
            構造が破綻しています（取り込み・書き込み停止中）
            <button className="link" onClick={p.onRemap}>
              再マッピング
            </button>
          </span>
        )}
        {recovery && (
          <span className="lb-msg lb-msg-warn">
            中断された書き込みの復旧が未完了です（「更新」で再試行されます）
          </span>
        )}
        {p.warnings.length > 0 && (
          <span className="lb-msg lb-msg-warn" title={p.warnings.join("\n")}>
            <Info size={13} /> 警告 {p.warnings.length} 件
          </span>
        )}
      </span>

      <span
        className="lb-badge lb-muted lb-concurrent"
        title={CONCURRENT_EDIT_WARNING}
      >
        <Users size={13} /> 同時編集非対応
      </span>
    </div>
  );
}
