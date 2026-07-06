import { useEffect, useMemo, useState } from "react";
import { X } from "lucide-react";
import { priceHistory, type PriceHistoryEntry } from "../api/bom";

interface Props {
  supplier: string;
  partNo: string;
  onClose: () => void;
}

/** Minimal currency symbol (JPY→¥); other codes fall back to a code prefix. */
function sym(currency?: string | null): string {
  return !currency || currency === "JPY" ? "¥" : `${currency} `;
}

function priceNum(e: PriceHistoryEntry): number {
  const n = Number(e.unitPrice);
  return Number.isFinite(n) ? n : NaN;
}

/** Self-drawn SVG sparkline of unit price over time (oldest→newest, left→right).
 *  Skipped by the caller when there are fewer than 2 numeric points. */
function Sparkline({ values }: { values: number[] }) {
  const W = 280;
  const H = 48;
  const P = 5;
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const dx = values.length > 1 ? (W - 2 * P) / (values.length - 1) : 0;
  const pts = values.map((v, i) => {
    const x = P + i * dx;
    const y = P + (1 - (v - min) / span) * (H - 2 * P);
    return [x, y] as const;
  });
  const path = pts.map((p, i) => `${i === 0 ? "M" : "L"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(" ");
  return (
    <svg className="ph-spark" viewBox={`0 0 ${W} ${H}`} width={W} height={H} preserveAspectRatio="none">
      <path d={path} className="ph-spark-line" fill="none" />
      {pts.map((p, i) => (
        <circle key={i} cx={p[0]} cy={p[1]} r={i === pts.length - 1 ? 3 : 2} className="ph-spark-dot" />
      ))}
    </svg>
  );
}

/** Price/delivery history for one part number (all-BOM, cross-cache). Opened from the
 *  toolbar for the selected/focused row. Shows a unit-price sparkline + a per-fetch table
 *  with delta-vs-previous and ship date. */
export function PriceHistory({ supplier, partNo, onClose }: Props) {
  const [entries, setEntries] = useState<PriceHistoryEntry[] | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    let alive = true;
    priceHistory(supplier, partNo)
      .then((e) => alive && setEntries(e))
      .catch((err) => alive && setError(String(err)));
    return () => {
      alive = false;
    };
  }, [supplier, partNo]);

  // Numeric price series oldest→newest for the sparkline + min/max/latest summary.
  const stats = useMemo(() => {
    if (!entries || entries.length === 0) return null;
    const nums = entries.map(priceNum).filter((n) => Number.isFinite(n));
    const chrono = [...entries].reverse().map(priceNum).filter((n) => Number.isFinite(n));
    const latest = entries.find((e) => Number.isFinite(priceNum(e)));
    return {
      chrono,
      min: nums.length ? Math.min(...nums) : NaN,
      max: nums.length ? Math.max(...nums) : NaN,
      currency: latest?.currency ?? entries[0].currency,
      latest: latest ? priceNum(latest) : NaN,
    };
  }, [entries]);

  return (
    <>
      <div className="col-mgr-backdrop" onClick={onClose} />
      <div className="col-mgr ph-modal">
        <div className="col-mgr-head">
          <h2>
            価格履歴: {partNo}
            <span className="ph-supplier">（{supplier}）</span>
          </h2>
          <button className="icon-btn" onClick={onClose} title="閉じる">
            <X size={18} />
          </button>
        </div>

        {error ? (
          <p className="ph-empty">読み込みに失敗しました: {error}</p>
        ) : !entries ? (
          <p className="ph-empty">読み込み中…</p>
        ) : entries.length === 0 ? (
          <p className="ph-empty">
            この型番の履歴はまだありません。「MISUMI 一括取得」で価格を取得すると記録されます。
          </p>
        ) : (
          <div className="ph-body">
            {stats && stats.chrono.length >= 2 && (
              <div className="ph-trend">
                <Sparkline values={stats.chrono} />
                <div className="ph-summary">
                  <span>
                    最新 <b>{Number.isFinite(stats.latest) ? `${sym(stats.currency)}${stats.latest.toLocaleString()}` : "—"}</b>
                  </span>
                  <span>
                    最安 <b>{`${sym(stats.currency)}${stats.min.toLocaleString()}`}</b>
                  </span>
                  <span>
                    最高 <b>{`${sym(stats.currency)}${stats.max.toLocaleString()}`}</b>
                  </span>
                </div>
              </div>
            )}

            <div className="ph-table-wrap">
              <table className="ph-table">
                <thead>
                  <tr>
                    <th>取得日時</th>
                    <th className="num">単価</th>
                    <th className="num">前回比</th>
                    <th>出荷日</th>
                  </tr>
                </thead>
                <tbody>
                  {entries.map((e, i) => {
                    const cur = priceNum(e);
                    // Rows are newest-first, so the "previous" fetch is the next row down.
                    const prev = i + 1 < entries.length ? priceNum(entries[i + 1]) : NaN;
                    const diff = Number.isFinite(cur) && Number.isFinite(prev) ? cur - prev : NaN;
                    const cls = !Number.isFinite(diff) ? "" : diff > 0 ? "ph-up" : diff < 0 ? "ph-down" : "";
                    return (
                      <tr key={i}>
                        <td className="ph-muted">{(e.fetchedAt ?? "").slice(0, 16)}</td>
                        <td className="num">
                          {Number.isFinite(cur) ? `${sym(e.currency)}${cur.toLocaleString()}` : (e.unitPrice ?? "—")}
                        </td>
                        <td className={`num ${cls}`}>
                          {!Number.isFinite(diff)
                            ? "—"
                            : diff === 0
                              ? "±0"
                              : `${diff > 0 ? "+" : "−"}${sym(e.currency)}${Math.abs(diff).toLocaleString()}`}
                        </td>
                        <td>{e.shipDate ?? "—"}</td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        )}
      </div>
    </>
  );
}
