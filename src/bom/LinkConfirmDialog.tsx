// Confirm 判定の候補確定ダイアログ (§1.3 候補確定 → linked)。
// 同一 excel_col への排他候補 (KeepSkipped/ImportSkippedAsUser/SkipColumn) はラジオで
// 1つだけ選ばせる (バックエンドも同時指定を拒否する — PR-5 の選択整合ガード)。
// 部分確定可: 確定しなかった異常は返却 view の新しい Confirm verdict として残る。

import { useMemo, useState } from "react";
import type { LinkResolutionCandidate, StructureVerdict } from "../types/link";

type ConfirmVerdict = Extract<StructureVerdict, { kind: "confirm" }>;

function candidateLabel(c: LinkResolutionCandidate): string {
  switch (c.kind) {
    case "adoptSheetRename":
      return `シート名の変更を採用: 「${c.newSheet}」`;
    case "adoptHeaderRowMove":
      return `ヘッダ行の移動を採用: ${c.newHeaderRow} 行目（データ開始 ${c.newDataStartRow} 行目）`;
    case "adoptColumnMove":
      return `列「${c.appKey}」の移動を採用: ${colName(c.newExcelCol)} 列へ`;
    case "adoptRename":
      return `列「${c.appKey}」の名称変更を採用: 「${c.newLabel}」`;
    case "dropOptionalColumn":
      return `任意列「${c.appKey}」を割当から外す（列が見つかりません）`;
    case "importSkippedAsUser":
      return `「${c.label}」をユーザー列として取り込む`;
    case "keepSkipped":
      return c.newLabel
        ? `読み飛ばしのまま（新しい名前「${c.newLabel}」を記録）`
        : "読み飛ばしのまま";
    case "skipColumn":
      return "読み飛ばす（取り込まない）";
  }
}

function colName(col0: number): string {
  let n = col0;
  let s = "";
  do {
    s = String.fromCharCode(65 + (n % 26)) + s;
    n = Math.floor(n / 26) - 1;
  } while (n >= 0);
  return s;
}

/** 排他グループのキー (同一 excel_col の skipped 系候補)。それ以外は単独扱い。 */
function exclusiveKey(c: LinkResolutionCandidate): string | null {
  if (c.kind === "importSkippedAsUser" || c.kind === "keepSkipped" || c.kind === "skipColumn") {
    return `col:${c.excelCol}`;
  }
  return null;
}

interface Props {
  verdict: ConfirmVerdict;
  busy: boolean;
  onConfirm: (accepted: LinkResolutionCandidate[]) => void;
  onRemap: () => void;
  onClose: () => void;
}

export function LinkConfirmDialog(p: Props) {
  // 単独候補 index の選択集合 (既定 ON) と、排他グループの選択 (既定: 未選択)。
  const groups = useMemo(() => {
    const singles: number[] = [];
    const exclusive = new Map<string, number[]>();
    p.verdict.candidates.forEach((c, i) => {
      const key = exclusiveKey(c);
      if (key === null) singles.push(i);
      else exclusive.set(key, [...(exclusive.get(key) ?? []), i]);
    });
    return { singles, exclusive: [...exclusive.entries()] };
  }, [p.verdict]);
  const [checked, setChecked] = useState<Set<number>>(() => new Set(groups.singles));
  const [radio, setRadio] = useState<Map<string, number>>(new Map());

  const accepted = useMemo(() => {
    const idx = [...checked, ...radio.values()].sort((a, b) => a - b);
    return idx.map((i) => p.verdict.candidates[i]);
  }, [checked, radio, p.verdict]);

  return (
    <div className="col-mgr-backdrop" onClick={p.onClose}>
      <div className="confirm-dlg link-confirm" onClick={(e) => e.stopPropagation()}>
        <h3>構造の変更を確認</h3>
        <p className="col-note">
          Excel 側の構造変更を検出しました。採用する変更を選んで確定してください
          （確定するまで同期は停止し、Excel へは書き込みません）。
        </p>
        <ul className="lc-reasons">
          {p.verdict.reasons.map((r, i) => (
            <li key={i}>{r}</li>
          ))}
        </ul>

        <div className="lc-candidates">
          {groups.singles.map((i) => (
            <label key={i} className="lc-item">
              <input
                type="checkbox"
                checked={checked.has(i)}
                onChange={(e) => {
                  const next = new Set(checked);
                  if (e.currentTarget.checked) next.add(i);
                  else next.delete(i);
                  setChecked(next);
                }}
              />
              {candidateLabel(p.verdict.candidates[i])}
            </label>
          ))}
          {groups.exclusive.map(([key, idxs]) => (
            <fieldset key={key} className="lc-group">
              <legend>
                列 {colName((p.verdict.candidates[idxs[0]] as { excelCol: number }).excelCol)}{" "}
                の扱い（どれか1つ）
              </legend>
              <label className="lc-item">
                <input
                  type="radio"
                  name={key}
                  checked={!radio.has(key)}
                  onChange={() => {
                    const next = new Map(radio);
                    next.delete(key);
                    setRadio(next);
                  }}
                />
                あとで決める
              </label>
              {idxs.map((i) => (
                <label key={i} className="lc-item">
                  <input
                    type="radio"
                    name={key}
                    checked={radio.get(key) === i}
                    onChange={() => setRadio(new Map(radio).set(key, i))}
                  />
                  {candidateLabel(p.verdict.candidates[i])}
                </label>
              ))}
            </fieldset>
          ))}
        </div>

        <div className="confirm-actions">
          <button className="link" onClick={p.onRemap} disabled={p.busy}>
            再マッピングで対応
          </button>
          <span className="spacer" />
          <button onClick={p.onClose} disabled={p.busy}>
            キャンセル
          </button>
          <button
            className="primary"
            disabled={p.busy || accepted.length === 0}
            onClick={() => p.onConfirm(accepted)}
          >
            選択した変更を確定
          </button>
        </div>
      </div>
    </div>
  );
}
