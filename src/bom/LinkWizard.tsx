// Excel リンク作成/再マッピングウィザード (implementation.md §2.3 probe/create/remap)。
// ImportWizard と同じ単一画面構成: シート選択 → ヘッダ行/データ開始行 → 列マッピング。
// リンク版の追加要素: 列ごとの所有権 (ユーザー列 / EC 書き戻し列 / EC 提案列 / 読み飛ばし)、
// 環境判定の表示 (NoWriteback は読み取り専用リンク)、完了前の同時編集非対応警告 (§3.12)。

import { useEffect, useMemo, useState } from "react";
import type { ColumnRole } from "../types/bom";
import { CORE_COLUMNS } from "../types/bom";
import type {
  LinkColumnConfig,
  LinkColumnMeta,
  LinkProbe,
  LinkedBomView,
} from "../types/link";
import * as api from "../api/bom";
import { CONCURRENT_EDIT_WARNING } from "./LinkBanner";

/** 割当の種別。ec = アプリ所有 (writeback)、suggest = ユーザー所有+アプリ内提案表示。 */
type AssignKind = "skip" | "core" | "custom" | "ec" | "suggest";
interface ColAssign {
  kind: AssignKind;
  /** kind=core のときの core キー。 */
  coreKey?: string;
  /** kind=ec / suggest のときの EC 項目 (dotted path・計算フィールド含む)。 */
  field?: string;
}

/** EC 書き戻し列に選べる項目 (§2.3: dotted path+計算フィールド2種)。 */
const EC_FIELDS: { field: string; label: string }[] = [
  { field: "quote.unitPrice", label: "EC 単価(税別)" },
  { field: "quote.unitPriceTax", label: "EC 単価(税込)" },
  { field: "quote.shipDate", label: "EC 出荷日" },
  { field: "quote.stock", label: "EC 即納在庫数" },
  { field: "quote.moq", label: "EC 最小数量" },
  { field: "quote.subtotal", label: "EC 小計（単価×数量×倍率）" },
  { field: "quote.moqNote", label: "EC MOQ 注意（未満で警告文）" },
  { field: "product.name", label: "EC 品名" },
];

// ImportWizard と同系のヘッダ推定 (リンク版はローカル保持 — 依存を作らない)。
const GUESS: Record<string, string> = {
  "no": "no", "no.": "no", "番号": "no",
  "partsno": "partsNo", "parts no": "partsNo", "型番": "partsNo", "品番": "partsNo",
  "partsname": "partsName", "parts name": "partsName", "品名": "partsName", "名称": "partsName",
  "order": "order", "発注": "order", "発注先": "order", "手配": "order",
  "qty": "qty", "数量": "qty", "個数": "qty",
  "material": "material", "材質": "material", "材料": "material",
};
const normalize = (s: string) => s.trim().toLowerCase().replace(/[\s　]+/g, " ");
const guessCore = (label: string): string | undefined => GUESS[normalize(label)];

const slug = (field: string) => "ec_" + field.replace(/[^a-zA-Z0-9]+/g, "_").toLowerCase();

const roleOf = (coreKey: string): ColumnRole | undefined =>
  coreKey === "partsNo" ? "partNo" : coreKey === "order" ? "source" : undefined;

export interface LinkWizardMode {
  kind: "create" | "remap";
  /** create: 既存 BOM の昇格時のみ。remap: 対象 BOM (必須)。 */
  bomId?: string;
  /** create の新規 BOM 名の初期値。 */
  name?: string;
  /** remap: 既存契約の列初期値 (シート/ヘッダ行は probe の推定を使う —
   *  LinkedBomView は契約のヘッダ座標を持たないため)。 */
  current?: { columns: LinkColumnMeta[] };
}

interface Props {
  mode: LinkWizardMode;
  /** create のとき選択済みのワークブックパス。 */
  workbookPath: string;
  probe: LinkProbe;
  onCancel: () => void;
  onDone: (view: LinkedBomView) => void;
}

export function LinkWizard({ mode, workbookPath, probe, onCancel, onDone }: Props) {
  const isRemap = mode.kind === "remap";
  const [name, setName] = useState(mode.name ?? "");
  const [sheetIndex, setSheetIndex] = useState(0);
  const sheet = probe.sheets[sheetIndex];
  const [headerRow, setHeaderRow] = useState(
    () => sheet?.suggestedHeaderRow ?? sheet?.startRow ?? 1,
  );
  const [dataStartRow, setDataStartRow] = useState(
    () => (sheet?.suggestedHeaderRow ?? sheet?.startRow ?? 1) + 1,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  // プレビュー上のヘッダ行 (絶対行 → preview index)。範囲外なら null。
  const headerIdx = sheet ? headerRow - sheet.startRow : -1;
  const headers: string[] = useMemo(() => {
    if (!sheet || headerIdx < 0 || headerIdx >= sheet.preview.length) return [];
    return sheet.preview[headerIdx] ?? [];
  }, [sheet, headerIdx]);
  const colCount = useMemo(
    () => Math.max(headers.length, ...(sheet?.preview.map((r) => r.length) ?? [0])),
    [headers, sheet],
  );

  // 列割当 (シート/ヘッダ行が変わるたびに再推定。remap 初回は既存契約から)。
  const [assigns, setAssigns] = useState<ColAssign[]>([]);
  useEffect(() => {
    const next: ColAssign[] = [];
    const usedCore = new Set<string>();
    for (let c = 0; c < colCount; c++) {
      const label = headers[c] ?? "";
      const meta = isRemap
        ? mode.current?.columns.find((m) => m.excelCol === c)
        : undefined;
      if (meta) {
        if (meta.ownership === "app") {
          next.push({ kind: "ec", field: meta.sourceField });
        } else if (meta.projection === "suggest") {
          next.push({ kind: "suggest", field: meta.sourceField });
        } else if (CORE_COLUMNS.some((cc) => cc.key === meta.appKey)) {
          next.push({ kind: "core", coreKey: meta.appKey });
          usedCore.add(meta.appKey);
        } else {
          next.push({ kind: "custom" });
        }
        continue;
      }
      const g = label ? guessCore(label) : undefined;
      if (g && !usedCore.has(g)) {
        usedCore.add(g);
        next.push({ kind: "core", coreKey: g });
      } else if (label.trim()) {
        next.push({ kind: "custom" });
      } else {
        next.push({ kind: "skip" });
      }
    }
    setAssigns(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sheetIndex, headerRow, colCount]);

  const setAssign = (c: number, a: ColAssign) =>
    setAssigns((prev) => prev.map((x, i) => (i === c ? a : x)));

  const takenCore = (c: number, key: string) =>
    assigns.some((a, i) => i !== c && a.kind === "core" && a.coreKey === key);

  const partAssigned = assigns.some((a) => a.kind === "core" && a.coreKey === "partsNo");

  function buildColumns(): LinkColumnConfig[] {
    const out: LinkColumnConfig[] = [];
    for (let c = 0; c < colCount; c++) {
      const a = assigns[c];
      const label = (headers[c] ?? "").trim();
      if (!a || a.kind === "skip") {
        // 空ヘッダの読み飛ばしは契約に載せない (相対検出の対象はラベル付きのみ)。
        if (label) {
          out.push({ excelCol: c, headerLabel: label, ownership: "skipped" });
        }
        continue;
      }
      if (a.kind === "core" && a.coreKey) {
        out.push({
          excelCol: c,
          headerLabel: label || undefined,
          appKey: a.coreKey,
          ownership: "user",
          required: a.coreKey === "partsNo",
          role: roleOf(a.coreKey),
        });
      } else if (a.kind === "custom") {
        out.push({
          excelCol: c,
          headerLabel: label || undefined,
          appKey: `u_${c}`,
          ownership: "user",
        });
      } else if (a.kind === "ec" && a.field) {
        out.push({
          excelCol: c,
          headerLabel: label || undefined,
          appKey: slug(a.field),
          ownership: "app",
          sourceField: a.field,
          projection: "writeback",
        });
      } else if (a.kind === "suggest" && a.field) {
        out.push({
          excelCol: c,
          headerLabel: label || undefined,
          appKey: `u_${c}`,
          ownership: "user",
          sourceField: a.field,
          projection: "suggest",
        });
      }
    }
    return out;
  }

  async function submit() {
    if (!sheet) return;
    setBusy(true);
    setError("");
    try {
      const columns = buildColumns();
      const view = isRemap
        ? await api.excelLinkRemap(mode.bomId!, {
            sheetName: sheet.name,
            headerRow,
            dataStartRow,
            columns,
          })
        : await api.excelLinkCreate({
            bomId: mode.bomId,
            name: mode.bomId ? undefined : name || undefined,
            workbookPath,
            sheetName: sheet.name,
            headerRow,
            dataStartRow,
            columns,
          });
      onDone(view);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  }

  const canSubmit = !busy && !!sheet && partAssigned && dataStartRow > headerRow;
  const previewRows = sheet?.preview.slice(0, 20) ?? [];

  return (
    <div className="col-mgr-backdrop">
      <div className="col-mgr import-wizard link-wizard" onClick={(e) => e.stopPropagation()}>
        <div className="col-mgr-head">
          <h3>{isRemap ? "再マッピング（リンクの修復）" : "Excel にリンク"}</h3>
          <button className="link" onClick={onCancel} disabled={busy}>
            閉じる
          </button>
        </div>

        {!isRemap && !mode.bomId && (
          <div className="iw-name-row">
            <label>BOM 名</label>
            <input value={name} onChange={(e) => setName(e.currentTarget.value)} placeholder="BOM 名" />
          </div>
        )}
        <div className="iw-name-row">
          <label>ファイル</label>
          <span className="lk-path" title={workbookPath}>
            {workbookPath}
          </span>
        </div>

        {probe.env.verdict === "no_writeback" && (
          <p className="lk-env-warn">
            この保存場所には書き戻しできません（{probe.env.reason ?? "環境判定"}）。
            読み取り専用リンクとして作成されます。
          </p>
        )}
        {probe.warnings.map((w, i) => (
          <p key={i} className="lk-env-warn">{w}</p>
        ))}

        <div className="iw-controls">
          {probe.sheets.length > 1 && (
            <label>
              シート
              <select
                value={sheetIndex}
                onChange={(e) => {
                  const i = Number(e.currentTarget.value);
                  setSheetIndex(i);
                  const sh = probe.sheets[i];
                  const hr = sh.suggestedHeaderRow ?? sh.startRow;
                  setHeaderRow(hr);
                  setDataStartRow(hr + 1);
                }}
              >
                {probe.sheets.map((s, i) => (
                  <option key={s.name} value={i}>
                    {s.name}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label>
            ヘッダ行
            <input
              type="number"
              min={1}
              value={headerRow}
              onChange={(e) => {
                const v = Number(e.currentTarget.value) || 1;
                setHeaderRow(v);
                if (dataStartRow <= v) setDataStartRow(v + 1);
              }}
            />
          </label>
          <label>
            データ開始行
            <input
              type="number"
              min={headerRow + 1}
              value={dataStartRow}
              onChange={(e) => setDataStartRow(Number(e.currentTarget.value) || headerRow + 1)}
            />
          </label>
          {sheet?.truncated && <span className="lk-env-warn">5,000 行を超えています（書き戻し不可）</span>}
        </div>

        <div className="iw-preview">
          <table className="col-table iw-table">
            <thead>
              <tr>
                <th className="lk-rownum">行</th>
                {Array.from({ length: colCount }, (_, c) => (
                  <th key={c}>
                    <div className="lk-col-head">
                      <span className="lk-col-label" title={headers[c] ?? ""}>
                        {headers[c]?.trim() || "（空）"}
                      </span>
                      <select
                        value={
                          assigns[c]?.kind === "core"
                            ? `core:${assigns[c].coreKey}`
                            : assigns[c]?.kind === "ec"
                              ? `ec:${assigns[c].field}`
                              : assigns[c]?.kind === "suggest"
                                ? `suggest:${assigns[c].field}`
                                : (assigns[c]?.kind ?? "skip")
                        }
                        onChange={(e) => {
                          const v = e.currentTarget.value;
                          if (v === "skip" || v === "custom") setAssign(c, { kind: v });
                          else if (v.startsWith("core:"))
                            setAssign(c, { kind: "core", coreKey: v.slice(5) });
                          else if (v.startsWith("ec:"))
                            setAssign(c, { kind: "ec", field: v.slice(3) });
                          else if (v.startsWith("suggest:"))
                            setAssign(c, { kind: "suggest", field: v.slice(8) });
                        }}
                      >
                        <option value="skip">—（読み飛ばし）</option>
                        {CORE_COLUMNS.map((cc) => (
                          <option
                            key={cc.key}
                            value={`core:${cc.key}`}
                            disabled={takenCore(c, cc.key)}
                          >
                            {cc.label}
                            {takenCore(c, cc.key) ? "（割当済）" : ""}
                          </option>
                        ))}
                        <option value="custom">ユーザー列として取り込む</option>
                        <optgroup label="EC 自動更新列（Excel へ書き戻し）">
                          {EC_FIELDS.map((f) => (
                            <option key={f.field} value={`ec:${f.field}`}>
                              {f.label}
                            </option>
                          ))}
                        </optgroup>
                        <optgroup label="EC 提案（アプリ内表示のみ・書き戻さない）">
                          {EC_FIELDS.filter((f) => !f.field.startsWith("quote.moqNote")).map((f) => (
                            <option key={f.field} value={`suggest:${f.field}`}>
                              {f.label}
                            </option>
                          ))}
                        </optgroup>
                      </select>
                    </div>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {previewRows.map((row, r) => {
                const abs = (sheet?.startRow ?? 1) + r;
                const cls = abs === headerRow ? "iw-hdr" : abs < dataStartRow ? "iw-skip" : "";
                return (
                  <tr key={r} className={cls}>
                    <td className="lk-rownum">{abs}</td>
                    {Array.from({ length: colCount }, (_, c) => (
                      <td key={c}>{row[c] ?? ""}</td>
                    ))}
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {!partAssigned && (
          <p className="lk-env-warn">型番列（Parts No）の割当が必要です。</p>
        )}
        {error && <p className="lk-error">{error}</p>}

        <p className="lk-concurrent-note">{CONCURRENT_EDIT_WARNING}</p>

        <div className="iw-actions">
          <button onClick={onCancel} disabled={busy}>
            キャンセル
          </button>
          <button className="primary" onClick={submit} disabled={!canSubmit}>
            {busy ? "処理中…" : isRemap ? "再マッピングを適用" : "リンクを作成"}
          </button>
        </div>
      </div>
    </div>
  );
}
