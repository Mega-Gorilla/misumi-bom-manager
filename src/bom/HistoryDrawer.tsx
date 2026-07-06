import { useEffect, useMemo, useState, type MouseEvent as ReactMouseEvent } from "react";
import { X, TrendingUp, Truck, Package } from "lucide-react";
import { priceHistory, type PriceHistoryEntry } from "../api/bom";

const HEIGHT_KEY = "mbm.historyHeight";
const MIN_H = 140;
const DEFAULT_H = 300;
/** Persisted drawer height, clamped to the current viewport. */
function initialHeight(): number {
  const s = Number(localStorage.getItem(HEIGHT_KEY));
  const h = Number.isFinite(s) && s >= MIN_H ? s : DEFAULT_H;
  return Math.min(h, Math.round(window.innerHeight * 0.85));
}

/** What the drawer should show for the active row. `empty` carries WHY there is nothing to
 *  fetch, so the drawer can explain it instead of implying history just hasn't accrued yet:
 *   - no-row : no row is selected
 *   - not-ec : the row's ORDER isn't an EC-supported source (history only exists for those)
 *   - no-part: the row has no part number */
export type HistoryTarget =
  | { kind: "row"; partNo: string; supplier: string }
  | { kind: "empty"; reason: "no-row" | "not-ec" | "no-part" };

interface Props {
  target: HistoryTarget;
  onClose: () => void;
}

type Tab = "price" | "lead" | "stock";

/** Minimal currency symbol (JPY→¥); other codes fall back to a code prefix. */
function sym(currency?: string | null): string {
  return !currency || currency === "JPY" ? "¥" : `${currency} `;
}

function priceNum(e: PriceHistoryEntry): number {
  const n = Number(e.unitPrice);
  return Number.isFinite(n) ? n : NaN;
}

/** Parse a "YYYY-MM-DD[ ...]" (or with slashes) prefix to a UTC day timestamp (TZ-safe). */
function ymd(s?: string | null): number {
  if (!s) return NaN;
  const [y, m, d] = s.slice(0, 10).split(/[-/]/).map(Number);
  return y && m && d ? Date.UTC(y, m - 1, d) : NaN;
}

/** Promised lead time in days = ship date − fetch date. NaN if either is unparseable. */
function leadDays(e: PriceHistoryEntry): number {
  const ship = ymd(e.shipDate);
  const fetched = ymd(e.fetchedAt);
  return Number.isFinite(ship) && Number.isFinite(fetched)
    ? Math.round((ship - fetched) / 86_400_000)
    : NaN;
}

function stockNum(e: PriceHistoryEntry): number {
  return typeof e.stock === "number" ? e.stock : NaN;
}

/** Self-drawn SVG sparkline of a numeric series (oldest→newest, left→right). */
function Sparkline({ values }: { values: number[] }) {
  const W = 300;
  const H = 46;
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

/** A simple summary strip (label + bold value pairs). */
function Summary({ items }: { items: { label: string; value: string }[] }) {
  return (
    <div className="ph-summary">
      {items.map((it) => (
        <span key={it.label}>
          {it.label} <b>{it.value}</b>
        </span>
      ))}
    </div>
  );
}

/** Bottom docked history drawer (rises up from the bottom). Follows the selected row and
 *  shows price / lead-time / stock trends for its part number across all BOMs. Replaces the
 *  earlier modal so the grid stays interactive while the history is consulted. */
export function HistoryDrawer({ target, onClose }: Props) {
  const [entries, setEntries] = useState<PriceHistoryEntry[] | null>(null);
  const [error, setError] = useState("");
  const [tab, setTab] = useState<Tab>("price");
  const [height, setHeight] = useState(initialHeight);

  // Drag the top edge to resize the drawer. Dragging up (smaller clientY) grows it. Clamp to
  // [MIN_H, 85% viewport]; persist the final height so it survives reopen / restart.
  const startResize = (e: ReactMouseEvent) => {
    e.preventDefault();
    const startY = e.clientY;
    const startH = height;
    const maxH = Math.round(window.innerHeight * 0.85);
    let latest = startH;
    document.body.classList.add("hd-resizing");
    const onMove = (ev: globalThis.MouseEvent) => {
      latest = Math.min(Math.max(startH + (startY - ev.clientY), MIN_H), maxH);
      setHeight(latest);
    };
    const onUp = () => {
      document.body.classList.remove("hd-resizing");
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      localStorage.setItem(HEIGHT_KEY, String(latest));
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const partNo = target.kind === "row" ? target.partNo : null;
  const supplier = target.kind === "row" ? target.supplier : "";

  useEffect(() => {
    if (!partNo) {
      setEntries(null);
      setError("");
      return;
    }
    let alive = true;
    setEntries(null);
    setError("");
    priceHistory(supplier, partNo)
      .then((e) => alive && setEntries(e))
      .catch((err) => alive && setError(String(err)));
    return () => {
      alive = false;
    };
  }, [supplier, partNo]);

  // Chronological (oldest→newest) numeric series for the active tab's sparkline.
  const chrono = useMemo(() => (entries ? [...entries].reverse() : []), [entries]);

  return (
    <div className="history-drawer" style={{ height }}>
      <div className="hd-resize" onMouseDown={startResize} title="ドラッグで高さを変更" />
      <div className="hd-head">
        <div className="hd-title">
          <span className="hd-heading">履歴</span>
          {partNo ? (
            <>
              <span className="hd-part">{partNo}</span>
              <span className="hd-supplier">（{supplier}）</span>
            </>
          ) : (
            <span className="hd-supplier">
              {target.kind === "empty" && target.reason === "not-ec"
                ? "EC 対応の発注先ではありません"
                : "行を選択すると表示します"}
            </span>
          )}
        </div>
        <div className="hd-tabs">
          <button className={tab === "price" ? "active" : ""} onClick={() => setTab("price")}>
            <TrendingUp size={14} /> 価格
          </button>
          <button className={tab === "lead" ? "active" : ""} onClick={() => setTab("lead")}>
            <Truck size={14} /> 納期
          </button>
          <button className={tab === "stock" ? "active" : ""} onClick={() => setTab("stock")}>
            <Package size={14} /> 在庫
          </button>
        </div>
        <button className="icon-btn" onClick={onClose} title="閉じる">
          <X size={18} />
        </button>
      </div>

      <div className="hd-body">
        {target.kind === "empty" ? (
          <p className="ph-empty">
            {target.reason === "not-ec"
              ? "この行の発注先（ORDER）は EC 対応ではないため、履歴はありません。履歴は EC 対応の発注先（現在は MISUMI）の行でのみ記録されます。"
              : target.reason === "no-part"
                ? "この行には型番がありません。型番を入力して「MISUMI 一括取得」すると記録されます。"
                : "グリッドで行を選択してください。"}
          </p>
        ) : error ? (
          <p className="ph-empty">読み込みに失敗しました: {error}</p>
        ) : !entries ? (
          <p className="ph-empty">読み込み中…</p>
        ) : entries.length === 0 ? (
          <p className="ph-empty">
            この型番の履歴はまだありません。「MISUMI 一括取得」で取得すると記録されます。
          </p>
        ) : tab === "price" ? (
          <PriceTab entries={entries} chrono={chrono} />
        ) : tab === "lead" ? (
          <LeadTab entries={entries} chrono={chrono} />
        ) : (
          <StockTab entries={entries} chrono={chrono} />
        )}
      </div>
    </div>
  );
}

function PriceTab({ entries, chrono }: { entries: PriceHistoryEntry[]; chrono: PriceHistoryEntry[] }) {
  const series = chrono.map(priceNum).filter(Number.isFinite);
  const cur = entries.find((e) => Number.isFinite(priceNum(e)));
  return (
    <>
      {series.length >= 2 && (
        <div className="ph-trend">
          <Sparkline values={series} />
          <Summary
            items={[
              { label: "最新", value: cur ? `${sym(cur.currency)}${priceNum(cur).toLocaleString()}` : "—" },
              { label: "最安", value: `${sym(cur?.currency)}${Math.min(...series).toLocaleString()}` },
              { label: "最高", value: `${sym(cur?.currency)}${Math.max(...series).toLocaleString()}` },
            ]}
          />
        </div>
      )}
      <table className="ph-table">
        <thead>
          <tr>
            <th>取得日時</th>
            <th className="num">単価</th>
            <th className="num">前回差</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((e, i) => {
            const c = priceNum(e);
            const prev = i + 1 < entries.length ? priceNum(entries[i + 1]) : NaN;
            const diff = Number.isFinite(c) && Number.isFinite(prev) ? c - prev : NaN;
            const cls = !Number.isFinite(diff) ? "" : diff > 0 ? "ph-up" : diff < 0 ? "ph-down" : "";
            return (
              <tr key={i}>
                <td className="ph-muted">{(e.fetchedAt ?? "").slice(0, 16)}</td>
                <td className="num">{Number.isFinite(c) ? `${sym(e.currency)}${c.toLocaleString()}` : (e.unitPrice ?? "—")}</td>
                <td className={`num ${cls}`}>
                  {!Number.isFinite(diff)
                    ? "—"
                    : diff === 0
                      ? "±0"
                      : `${diff > 0 ? "+" : "−"}${sym(e.currency)}${Math.abs(diff).toLocaleString()}`}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </>
  );
}

function LeadTab({ entries, chrono }: { entries: PriceHistoryEntry[]; chrono: PriceHistoryEntry[] }) {
  const series = chrono.map(leadDays).filter(Number.isFinite);
  const cur = entries.find((e) => e.shipDate);
  return (
    <>
      {series.length >= 2 && (
        <div className="ph-trend">
          <Sparkline values={series} />
          <Summary
            items={[
              { label: "最新出荷日", value: cur?.shipDate ?? "—" },
              { label: "最新リード", value: cur && Number.isFinite(leadDays(cur)) ? `${leadDays(cur)}日` : "—" },
              { label: "最短/最長", value: `${Math.min(...series)}/${Math.max(...series)}日` },
            ]}
          />
        </div>
      )}
      <table className="ph-table">
        <thead>
          <tr>
            <th>取得日時</th>
            <th>出荷日</th>
            <th className="num">リード日数</th>
            <th className="num">前回差</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((e, i) => {
            const ld = leadDays(e);
            // Compare LEAD TIME, not the raw ship-date calendar move. The ship date drifts by
            // the days elapsed between fetches, so its raw delta overstates the change
            // (出荷日差 = リード変化 + 取得日の経過日数). Lead-time change is fetch-timing-neutral.
            // Longer lead = 納期が延びた (worse→red), shorter = 早まった (green).
            const prevLd = i + 1 < entries.length ? leadDays(entries[i + 1]) : NaN;
            const dLead = Number.isFinite(ld) && Number.isFinite(prevLd) ? ld - prevLd : NaN;
            const cls = !Number.isFinite(dLead) ? "" : dLead > 0 ? "ph-up" : dLead < 0 ? "ph-down" : "";
            return (
              <tr key={i}>
                <td className="ph-muted">{(e.fetchedAt ?? "").slice(0, 16)}</td>
                <td>{e.shipDate ?? "—"}</td>
                <td className="num">{Number.isFinite(ld) ? `${ld}日` : "—"}</td>
                <td className={`num ${cls}`}>
                  {!Number.isFinite(dLead) ? "—" : dLead === 0 ? "±0" : `${dLead > 0 ? "+" : "−"}${Math.abs(dLead)}日`}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </>
  );
}

function StockTab({ entries, chrono }: { entries: PriceHistoryEntry[]; chrono: PriceHistoryEntry[] }) {
  const series = chrono.map(stockNum).filter(Number.isFinite);
  const cur = entries.find((e) => Number.isFinite(stockNum(e)));
  const anyStock = entries.some((e) => Number.isFinite(stockNum(e)));
  return (
    <>
      {!anyStock && (
        <p className="ph-note">
          在庫の記録はこのバージョン導入後の取得から蓄積されます（過去の取得分には在庫がありません）。
        </p>
      )}
      {series.length >= 2 && (
        <div className="ph-trend">
          <Sparkline values={series} />
          <Summary
            items={[
              { label: "現在", value: cur ? `${stockNum(cur).toLocaleString()}` : "—" },
              { label: "最少", value: `${Math.min(...series).toLocaleString()}` },
              { label: "最多", value: `${Math.max(...series).toLocaleString()}` },
            ]}
          />
        </div>
      )}
      <table className="ph-table">
        <thead>
          <tr>
            <th>取得日時</th>
            <th className="num">即納在庫数</th>
            <th className="num">前回差</th>
          </tr>
        </thead>
        <tbody>
          {entries.map((e, i) => {
            const s = stockNum(e);
            const prev = i + 1 < entries.length ? stockNum(entries[i + 1]) : NaN;
            const diff = Number.isFinite(s) && Number.isFinite(prev) ? s - prev : NaN;
            // More stock = better (green), less = red.
            const cls = !Number.isFinite(diff) ? "" : diff > 0 ? "ph-down" : diff < 0 ? "ph-up" : "";
            return (
              <tr key={i}>
                <td className="ph-muted">{(e.fetchedAt ?? "").slice(0, 16)}</td>
                <td className="num">{Number.isFinite(s) ? s.toLocaleString() : "—"}</td>
                <td className={`num ${cls}`}>
                  {!Number.isFinite(diff) ? "—" : diff === 0 ? "±0" : `${diff > 0 ? "+" : "−"}${Math.abs(diff).toLocaleString()}`}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </>
  );
}
