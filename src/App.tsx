import { useEffect, useState, type FormEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type LookupResponse = {
  ok: boolean;
  error?: string;
  suggest?: any;
  price?: any;
};

type Row = {
  partNumber: string;
  brandName: string;
  productName: string;
  unitPrice: string;
  unitPriceTax: string;
  shipDate: string;
  stock: string;
  plant: string;
};

type Messages = { errors: string[]; warnings: string[]; infos: string[] };

function collect(list: any): string[] {
  return (Array.isArray(list) ? list : [])
    .map((m: any) => m?.message)
    .filter(Boolean);
}

// 価格 API は全体と行（detailList[]）の双方に error/warning/info メッセージを返す。
// 行単位の errorMessageList（例: 最小注文数エラー）を取りこぼさないよう両方をまとめる。
function gatherMessages(res: LookupResponse): Messages {
  const price = res.price;
  const d = price?.detailList?.[0];
  return {
    errors: [...collect(price?.errorMessageList), ...collect(d?.errorMessageList)],
    warnings: [...collect(price?.warningMessageList), ...collect(d?.warningMessageList)],
    infos: [...collect(price?.infoMessageList), ...collect(d?.infoMessageList)],
  };
}

function extract(res: LookupResponse): { row: Row | null; msgs: Messages } {
  const msgs = gatherMessages(res);
  const d = res.price?.detailList?.[0];
  if (!d) return { row: null, msgs };
  const row: Row = {
    partNumber: d.product?.inputProductCode ?? res.suggest?.partNumber ?? "",
    brandName: d.product?.brandName ?? res.suggest?.brandName ?? "",
    productName: d.product?.productName ?? "",
    unitPrice: d.salesPrice?.salesUnitPrice ?? "-",
    unitPriceTax: d.salesPrice?.salesUnitPriceIncludingTax ?? "-",
    shipDate: d.leadTime?.vsd ?? "-",
    stock: String(d.trade?.immediateShippableQty ?? "-"),
    plant: d.trade?.shippingPlantNameNative ?? "",
  };
  return { row, msgs };
}

function App() {
  const [partNumber, setPartNumber] = useState("CBT3-8");
  const [loading, setLoading] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [row, setRow] = useState<Row | null>(null);
  const [msgs, setMsgs] = useState<Messages>({ errors: [], warnings: [], infos: [] });

  // Poll the bridge readiness so the user knows when lookups can run.
  useEffect(() => {
    let timer: number | undefined;
    let cancelled = false;
    const poll = async () => {
      try {
        const r = await invoke<boolean>("bridge_ready");
        if (cancelled) return;
        setReady(r);
        if (!r) timer = window.setTimeout(poll, 800);
      } catch {
        if (!cancelled) timer = window.setTimeout(poll, 800);
      }
    };
    poll();
    return () => {
      cancelled = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  async function search(e?: FormEvent) {
    e?.preventDefault();
    setLoading(true);
    setError(null);
    setRow(null);
    setMsgs({ errors: [], warnings: [], infos: [] });
    try {
      const res = await invoke<LookupResponse>("lookup_part", { partNumber });
      if (!res.ok) {
        setError(res.error ?? "取得に失敗しました");
        return;
      }
      const { row, msgs } = extract(res);
      setMsgs(msgs);
      if (!row) {
        // 価格行が無い場合でも、行/全体エラーがあれば必ず表面化する
        setError(msgs.errors[0] ?? "価格・出荷日が取得できませんでした");
        return;
      }
      setRow(row);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  return (
    <main className="container">
      <header>
        <h1>MISUMI 単価・出荷日チェック</h1>
        <span className={ready ? "badge ok" : "badge wait"}>
          {ready ? "ブリッジ準備完了" : "初期化中…"}
        </span>
      </header>

      <form className="searchbar" onSubmit={search}>
        <input
          value={partNumber}
          onChange={(e) => setPartNumber(e.currentTarget.value)}
          placeholder="型番を入力（例: CBT3-8）"
          spellCheck={false}
          autoFocus
        />
        <button type="submit" disabled={loading || !ready}>
          {loading ? "取得中…" : "検索"}
        </button>
      </form>

      {error && <div className="error">⚠ {error}</div>}

      {row && (
        <section className="card">
          <div className="card-head">
            <span className="pn">{row.partNumber}</span>
            {row.brandName && <span className="brand">{row.brandName}</span>}
          </div>
          {row.productName && <div className="pname">{row.productName}</div>}
          <div className="grid">
            <div className="field">
              <label>単価（税別）</label>
              <strong>¥{row.unitPrice}</strong>
            </div>
            <div className="field">
              <label>単価（税込）</label>
              <strong>¥{row.unitPriceTax}</strong>
            </div>
            <div className="field">
              <label>出荷日</label>
              <strong>{row.shipDate}</strong>
            </div>
            <div className="field">
              <label>即納可能数</label>
              <strong>{row.stock}</strong>
            </div>
            {row.plant && (
              <div className="field wide">
                <label>出荷元</label>
                <strong>{row.plant}</strong>
              </div>
            )}
          </div>
          {msgs.errors.length > 0 && (
            <ul className="messages errors">
              {msgs.errors.map((m, i) => (
                <li key={`e${i}`}>⚠ {m}</li>
              ))}
            </ul>
          )}
          {msgs.warnings.length > 0 && (
            <ul className="messages warnings">
              {msgs.warnings.map((m, i) => (
                <li key={`w${i}`}>{m}</li>
              ))}
            </ul>
          )}
          {msgs.infos.length > 0 && (
            <ul className="messages infos">
              {msgs.infos.map((m, i) => (
                <li key={`i${i}`}>{m}</li>
              ))}
            </ul>
          )}
        </section>
      )}

      <p className="note">
        ※ MISUMI 内部APIを利用。価格・出荷日は未ログイン時の標準値で、時点により変動します。
      </p>
    </main>
  );
}

export default App;
