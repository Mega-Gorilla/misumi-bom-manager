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

function extract(res: LookupResponse): { row: Row | null; messages: string[] } {
  const d = res.price?.detailList?.[0];
  if (!d) return { row: null, messages: [] };
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
  const messages: string[] = (d.infoMessageList ?? [])
    .map((m: any) => m.message)
    .filter(Boolean);
  return { row, messages };
}

function App() {
  const [partNumber, setPartNumber] = useState("CBT3-8");
  const [loading, setLoading] = useState(false);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [row, setRow] = useState<Row | null>(null);
  const [messages, setMessages] = useState<string[]>([]);

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
    setMessages([]);
    try {
      const res = await invoke<LookupResponse>("lookup_part", { partNumber });
      if (!res.ok) {
        setError(res.error ?? "取得に失敗しました");
        return;
      }
      const { row, messages } = extract(res);
      if (!row) {
        setError("価格・出荷日が取得できませんでした");
        return;
      }
      setRow(row);
      setMessages(messages);
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
          {messages.length > 0 && (
            <ul className="messages">
              {messages.map((m, i) => (
                <li key={i}>{m}</li>
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
