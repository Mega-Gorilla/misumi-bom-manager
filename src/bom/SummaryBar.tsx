import { useMemo } from "react";
import type { BomDoc } from "../types/bom";
import { computeTotals } from "../lib/columns";

/** Currency-aware amount formatting: JPY (and empty) render as ¥ with no decimals;
 *  anything else shows the code as a prefix. */
function formatAmount(amount: number, currency: string): string {
  if (currency === "JPY" || currency === "") {
    return `¥${Math.round(amount).toLocaleString()}`;
  }
  return `${currency} ${amount.toLocaleString()}`;
}

/** BOM-wide summary bar shown under the grid: total amount, latest ship date, fetched /
 *  error counts. Recomputes from the live doc, so Qty / ORDER edits update it immediately. */
export function SummaryBar({ doc }: { doc: BomDoc }) {
  const t = useMemo(() => computeTotals(doc), [doc]);

  return (
    <div className="summary-bar">
      {t.activeRows === 0 ? (
        <span className="sb-muted">EC 未取得（MISUMI 一括取得で価格・出荷日を取得）</span>
      ) : (
        <>
          <span className="sb-item">
            <span className="sb-label">合計</span>
            <b className="sb-amount">{formatAmount(t.totalAmount, t.currency)}</b>
            <span className="sb-muted">（{t.pricedRows}件）</span>
          </span>
          <span className="sb-sep" />
          <span className="sb-item">
            <span className="sb-label">最遅出荷</span>
            <b>{t.latestShipDate || "—"}</b>
          </span>
          <span className="sb-sep" />
          <span className="sb-item">
            <span className="sb-label">取得済</span>
            <b>
              {t.pricedRows}/{t.activeRows}
            </b>
          </span>
          {t.errorRows > 0 && (
            <>
              <span className="sb-sep" />
              <span className="sb-item sb-error">エラー {t.errorRows}</span>
            </>
          )}
        </>
      )}
    </div>
  );
}
