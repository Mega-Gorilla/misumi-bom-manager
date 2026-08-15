import { useCallback, useEffect, useRef, useState } from "react";
import type { AgGridReact } from "ag-grid-react";
import "./App.css";
import type {
  BomDoc,
  BomRow,
  BomSummary,
  ColumnRole,
  SupplierQuote,
  WritePolicy,
} from "./types/bom";
import {
  newBom,
  newRow,
  nextNo,
  newSupplierColumn,
  SUPPLIER_FIELDS,
  partNoColumn,
  sourceColumn,
  orderNoColumns,
  DEFAULT_ORDER_NO_SEPARATOR,
} from "./types/bom";
import type { Workbook } from "./types/bom";
import { applyLinkedColumns, getCellValue, buildExportGrid } from "./lib/columns";
import type { LinkGridOptions } from "./lib/columns";
import { createWatchQueue } from "./lib/watchQueue";
import type { WatchQueue } from "./lib/watchQueue";
import * as api from "./api/bom";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import type {
  CalcState,
  EnvCheck,
  FormulaCell,
  LinkColumnMeta,
  LinkProbe,
  LinkStatus,
  LinkedBomView,
  PendingInfo,
  StructureVerdict,
  SyncStatus,
} from "./types/link";
import { calcUsable } from "./types/link";
import { BomEditor } from "./bom/BomEditor";
import { Toolbar } from "./bom/Toolbar";
import { BomList } from "./bom/BomList";
import { ColumnManager } from "./bom/ColumnManager";
import { ImportWizard } from "./bom/ImportWizard";
import { HistoryDrawer, type HistoryTarget } from "./bom/HistoryDrawer";
import { SummaryBar } from "./bom/SummaryBar";
import { LinkBanner } from "./bom/LinkBanner";
import { LinkWizard, type LinkWizardMode } from "./bom/LinkWizard";
import { LinkConfirmDialog } from "./bom/LinkConfirmDialog";
import { LinkConflictDialog } from "./bom/LinkConflictDialog";

// ORDER values that map to a supported EC provider (history/quotes exist only for these).
const SUPPORTED_EC = ["MISUMI"];

// The history drawer is a togglable panel, not a popup: once the user opens it we keep it
// open across BOM switches and app restarts (it follows the selected row while open). Persist
// just the open/closed preference so re-entering the editor restores the user's last choice.
const HISTORY_OPEN_KEY = "mbm.historyOpen";
function initialHistoryOpen(): boolean {
  try {
    return localStorage.getItem(HISTORY_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

function slug(s: string): string {
  return (
    s
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "_")
      .replace(/^_|_$/g, "") || "col"
  );
}

function uniqueKey(doc: BomDoc, base: string): string {
  const keys = new Set(doc.columns.map((c) => c.key));
  let key = `c_${base}`;
  let i = 1;
  while (keys.has(key)) key = `c_${base}_${i++}`;
  return key;
}

export default function App() {
  const [view, setView] = useState<"list" | "editor">("list");
  const [doc, setDoc] = useState<BomDoc | null>(null);
  const [list, setList] = useState<BomSummary[]>([]);
  const [status, setStatus] = useState("");
  const [showColumns, setShowColumns] = useState(false);
  const [columnsTab, setColumnsTab] = useState<"columns" | "ec">("columns");
  const [importSrc, setImportSrc] = useState<{ workbook: Workbook; fileName: string; path: string } | null>(null);
  const [historyOpen, setHistoryOpen] = useState(initialHistoryOpen);
  const [confirmBack, setConfirmBack] = useState(false);
  const [activeTarget, setActiveTarget] = useState<HistoryTarget>({ kind: "empty", reason: "no-row" });
  const [quickFilter, setQuickFilter] = useState("");
  const [quoting, setQuotingState] = useState(false);
  // EC 取得中かどうかは watch タスクの実行可否を決めるため、再レンダーを待たない
  // 同期 ref を併設する (setQuoting 直後の窓を閉じる — PR-7 レビュー)。
  const quotingRef = useRef(false);
  const setQuoting = (v: boolean) => {
    quotingRef.current = v;
    setQuotingState(v);
  };
  const [addingCart, setAddingCart] = useState(false);
  const [confirmCart, setConfirmCart] = useState<{
    items: api.CartItem[];
    count: number;
    totalQty: number;
    mode: "selection" | "all";
  } | null>(null);
  const [cartDone, setCartDone] = useState(false);
  const [loginPrompt, setLoginPrompt] = useState<null | { phase: "ask" | "waiting" | "failed" }>(
    null,
  );
  const pendingCartRef = useRef<{
    items: api.CartItem[];
    count: number;
    totalQty: number;
    mode: "selection" | "all";
  } | null>(null);
  // ---- Excel リンクモード (docs/plans/0018-excel-link-mode) ----
  // link != null = 開いている BOM はリンク BOM (グリッド読み取り専用・§4.8)。
  const [link, setLink] = useState<{
    verdict: StructureVerdict;
    calcState: CalcState;
    syncStatus: SyncStatus;
    env: EnvCheck;
    columnsMeta: LinkColumnMeta[];
    formulaCells: FormulaCell[];
    warnings: string[];
    pending?: PendingInfo;
    status?: LinkStatus;
  } | null>(null);
  const [linkWizard, setLinkWizard] = useState<{
    mode: LinkWizardMode;
    workbookPath: string;
    probe: LinkProbe;
  } | null>(null);
  const [linkConfirmOpen, setLinkConfirmOpen] = useState(false);
  const [linkConflictOpen, setLinkConflictOpen] = useState(false);
  const [linkBusy, setLinkBusyState] = useState(false);
  // React state は再レンダーまで更新されないため、同時実行ガードには同期的な ref を
  // 使う (watch 由来のイベントは同一ティックで連続到着し得る — PR-7 レビュー)。
  const linkBusyRef = useRef(false);
  const setLinkBusy = (v: boolean) => {
    linkBusyRef.current = v;
    setLinkBusyState(v);
  };
  const gridRef = useRef<AgGridReact<BomRow>>(null);
  // JSON snapshot of the doc as of the last load/save; back() compares against it to detect
  // unsaved changes. A freshly created (untouched) BOM counts as clean, like Notepad.
  const savedSnapRef = useRef("");

  const reloadList = useCallback(async () => {
    try {
      setList(await api.bomList());
    } catch (e) {
      setStatus(String(e));
    }
  }, []);

  useEffect(() => {
    reloadList();
  }, [reloadList]);

  // Remember the drawer's open/closed state so it persists across editor sessions & restarts.
  useEffect(() => {
    try {
      localStorage.setItem(HISTORY_OPEN_KEY, historyOpen ? "1" : "0");
    } catch {
      /* localStorage unavailable — non-fatal */
    }
  }, [historyOpen]);

  // LinkedBomView をエディタ状態へ反映する共通経路 (open/create/confirm/remap の戻り)。
  const adoptLinkView = useCallback((view: LinkedBomView) => {
    setDoc(view.doc);
    savedSnapRef.current = JSON.stringify(view.doc); // リンク BOM は常にクリーン (編集不可)
    setLink({
      verdict: view.verdict,
      calcState: view.calcState,
      syncStatus: view.syncStatus,
      env: view.env,
      columnsMeta: view.columnsMeta,
      formulaCells: view.formulaCells,
      warnings: view.warnings,
      pending: view.pending,
    });
    if (view.warnings.length > 0) setStatus(view.warnings[0]);
  }, []);

  const openBom = useCallback(async (id: string) => {
    try {
      const d = await api.bomLoad(id);
      if (!d) return;
      if (d.meta.linked) {
        // リンク BOM は excel_link_open が入口 (§2.3): 読込→構造検証→復元→合成。
        const view = await api.excelLinkOpen(id);
        adoptLinkView(view);
        setStatus("");
        setView("editor");
        return;
      }
      setLink(null);
      setDoc(d);
      savedSnapRef.current = JSON.stringify(d);
      setStatus("");
      setView("editor");
    } catch (e) {
      setStatus(String(e));
    }
  }, [adoptLinkView]);

  const createBom = useCallback(() => {
    const d = newBom();
    setLink(null);
    setDoc(d);
    savedSnapRef.current = JSON.stringify(d);
    setStatus("");
    setView("editor");
  }, []);

  // ---- リンク BOM の操作群 ----

  /** 軽量ステータスのポーリング (エディタ表示中のみ・5s)。反映待ち/競合/ロック予兆を追従。 */
  const linkedId = link ? doc?.id : undefined;
  useEffect(() => {
    if (!linkedId || view !== "editor") return;
    let alive = true;
    const tick = async () => {
      try {
        const st = await api.excelLinkStatus(linkedId);
        if (!alive) return;
        setLink((l) =>
          l
            ? {
                ...l,
                status: st,
                syncStatus: st.syncStatus,
                calcState: st.calcState,
                pending: st.pending,
              }
            : l,
        );
      } catch {
        /* ポーリング失敗は無視 (次周期で再試行) */
      }
    };
    void tick();
    const h = setInterval(() => void tick(), 5000);
    return () => {
      alive = false;
      clearInterval(h);
    };
  }, [linkedId, view]);

  // ファイル監視 (PR-7・§4.2.3): リンクエディタ表示中のみ有効 (既定 ON)。
  // 生イベントの判断は Rust 側 (親 dir 監視・デバウンス・~$ フィルタ) — ここは
  // 合成イベントを受けて既存導線を呼ぶだけ。ハンドラは ref 経由で最新を参照する。
  const watchRef = useRef<{
    busy: boolean;
    refresh: () => Promise<void>;
    apply: () => Promise<void>;
  }>({ busy: false, refresh: async () => {}, apply: async () => {} });

  // watch 由来の処理は1本のキューで直列化する (lib/watchQueue.ts に切り出し・単体
  // テストで固定): FIFO 厳守で Changed の再読込が完了してから ExcelClosed の
  // apply が走り、busy 中は自分のキュー位置を保持したまま待つ (再キューしない —
  // 後続に追い越されるため)。破棄は離脱 (cleanup) 時のみ (PR-7 レビュー)。
  const watchQueueRef = useRef<WatchQueue | null>(null);
  if (!watchQueueRef.current) {
    watchQueueRef.current = createWatchQueue({
      isBusy: () => linkBusyRef.current || quotingRef.current || watchRef.current.busy,
    });
  }
  const watchQueue = watchQueueRef.current;

  // watch の所有権トークン: StrictMode の setup→cleanup→setup や素早い再入場で、
  // 旧 effect の cleanup が新 effect の watch を止めてしまうのを防ぐ (PR-7 レビュー)。
  // 同じ BOM を新しい世代が所有していれば旧 cleanup は watch(false) を呼ばない
  // (別 BOM なら必ず止める — 監視の残留を作らない)。
  const watchGenRef = useRef(0);
  const watchOwnerRef = useRef<{ gen: number; bomId: string } | null>(null);

  useEffect(() => {
    if (!linkedId || view !== "editor") return;
    let alive = true;
    const gen = ++watchGenRef.current;
    watchOwnerRef.current = { gen, bomId: linkedId };
    const isAlive = () => alive;
    let unChanged: (() => void) | undefined;
    let unClosed: (() => void) | undefined;
    // listener を先に登録してから watch を有効化する (登録前に届いたイベントを
    // 取りこぼさない)。cleanup は初期化 Promise の完了後に走らせ、watch(false) が
    // watch(true) を追い越したり listener が残ったりしないようにする。
    const ready = (async () => {
      try {
        unChanged = await api.onExcelLinkChanged((p) => {
          if (!alive || p.bomId !== linkedId) return;
          // §9-7: 保存検知 → 自動再読込 (Safe のみ自動適用は open の既存経路)。
          watchQueue.enqueue(isAlive, () => watchRef.current.refresh());
        });
        if (!alive) return;
        unClosed = await api.onExcelLinkExcelClosed((p) => {
          if (!alive || p.bomId !== linkedId) return;
          // §9-5 完成: Excel が閉じた → 反映待ちがあれば自動再試行。先行する
          // Changed の再読込が終わってから、pending は DB へ問い合わせて判断する
          // (React state は再レンダー待ちで古い可能性がある)。
          watchQueue.enqueue(isAlive, async () => {
            try {
              const st = await api.excelLinkStatus(linkedId);
              if (!alive || !st.pending) return;
              await watchRef.current.apply();
            } catch {
              /* 次のイベント / 手動導線に委ねる (監視は利便性トリガ) */
            }
          });
        });
        if (!alive) return;
        await api.excelLinkWatch(linkedId, true);
      } catch (e) {
        if (alive) setStatus(String(e));
      }
    })();
    return () => {
      alive = false;
      void ready.then(() => {
        unChanged?.();
        unClosed?.();
        // 同じ BOM をより新しい世代が所有していれば、その watch を止めてしまうため
        // 呼ばない (バックエンドは bom_id キーの単一 handle なので、旧 cleanup の
        // watch(false) が新しい watch を消し得る)。
        const owner = watchOwnerRef.current;
        const superseded = !!owner && owner.gen !== gen && owner.bomId === linkedId;
        if (!superseded) void api.excelLinkWatch(linkedId, false).catch(() => {});
      });
    };
  }, [linkedId, view]);

  /** リンク先ワークブックの実体パス (「Excel で編集」「フォルダを開く」用)。 */
  const linkWorkbookPath = (): string | undefined =>
    link?.env.readTarget ?? link?.env.resolvedPath ?? doc?.meta.importedFrom ?? undefined;

  /** 未保存のメタ編集 (名前・数量倍率) を確定してから再読込系操作へ進む。
   *  adoptLinkView は DB 由来の doc で全置換するため、ここで保存しないと編集が
   *  消える (レビュー #2)。失敗時は false を返し、呼び出し側は操作を中止する。 */
  const flushLinkedMeta = async (): Promise<boolean> => {
    if (!link || !doc?.id) return true;
    if (JSON.stringify(doc) === savedSnapRef.current) return true;
    try {
      await api.bomUpdateMeta(doc.id, doc.meta);
      savedSnapRef.current = JSON.stringify(doc);
      return true;
    } catch (e) {
      setStatus(`設定の保存に失敗しました: ${e}`);
      return false;
    }
  };

  /** 「更新」= excel_link_open で再読込。 */
  const refreshLink = async () => {
    // 同期 ref で先にガードを立ててから await する (メタ保存中に2本目が
    // 走り込むのを防ぐ — PR-7 レビュー)。
    if (!doc?.id || linkBusyRef.current) return;
    setLinkBusy(true);
    setStatus("Excel から再読込しています…");
    try {
      if (!(await flushLinkedMeta())) return;
      const view = await api.excelLinkOpen(doc.id);
      adoptLinkView(view);
      setStatus(view.warnings[0] ?? "更新しました");
    } catch (e) {
      setStatus(String(e));
    } finally {
      setLinkBusy(false);
    }
  };

  /** 「Excel へ反映」= excel_link_apply。結果 kind ごとに UI を分岐 (§2.3)。 */
  const applyLink = async () => {
    if (!doc?.id || linkBusyRef.current) return;
    setLinkBusy(true);
    setStatus("Excel へ反映しています…");
    try {
      if (!(await flushLinkedMeta())) return;
      const out = await api.excelLinkApply(doc.id);
      const warn = out.warnings.length > 0 ? `（警告: ${out.warnings[0]}）` : "";
      if (out.kind === "applied") {
        setStatus(`Excel へ反映しました（第${out.generation}世代）${warn}`);
      } else if (out.kind === "pending") {
        setStatus(`反映待ちになりました（Excel が開いています。閉じてから再実行してください）${warn}`);
      } else if (out.kind === "conflict") {
        setStatus("外部の変更と競合しました。バックアップに外部版を保全しています");
        setLinkConflictOpen(true);
      } else {
        const msg: Record<string, string> = {
          env: "この保存場所には書き戻しできません（読み取り専用リンク）",
          structure: "構造の変更が検出されたため反映を中止しました。「更新」で確認してください",
          fingerprint_changed:
            "Excel 側が変更されています。自動では書き込みません — 「更新」で再読込してから再実行してください",
          unresolved_conflict: "未解決の競合があります。先に競合を解決してください",
          spill: "書き込み先がスピル範囲と交差しています",
          formula_cell: "書き込み先セルに数式があります",
          truncated: "5,000 行を超えるファイルには書き込めません",
          nothing_to_write: "書き込む EC 取得値がありません（先に MISUMI 一括取得を実行してください）",
        };
        setStatus(`反映できません: ${msg[out.reason] ?? out.reason}${warn}`);
        if (out.reason === "unresolved_conflict") setLinkConflictOpen(true);
      }
      // 反映後の状態 (Stale 遷移・pending 解消・競合) を一度の再読込で取り込む。
      const view = await api.excelLinkOpen(doc.id);
      adoptLinkView(view);
    } catch (e) {
      setStatus(String(e));
    } finally {
      setLinkBusy(false);
    }
  };

  // watch ハンドラが常に最新の状態・関数を見るための ref 更新 (毎レンダー)。
  // refresh/apply は Promise を返す — キューが完了を待って直列化する。
  watchRef.current = {
    busy: linkBusy || quoting || !!linkWizard,
    refresh: () => refreshLink(),
    apply: () => applyLink(),
  };

  /** 数量倍率の変更 (§4.8: リンク BOM でも編集可な DB 所有メタ)。即時永続化。 */
  const setLinkMultiplier = async (mult: number) => {
    if (!doc?.id || !Number.isFinite(mult) || mult <= 0) return;
    const next = { ...doc, meta: { ...doc.meta, qtyMultiplier: mult } };
    setDoc(next);
    try {
      await api.bomUpdateMeta(doc.id, next.meta);
      savedSnapRef.current = JSON.stringify(next);
      setStatus(`数量倍率を ×${mult} に変更しました`);
    } catch (e) {
      setStatus(String(e));
    }
  };

  /** 「Excel で編集」= ファイル起動のみ (ロックも受け渡しも無い・§9-6)。 */
  const editInExcel = async () => {
    const path = linkWorkbookPath();
    if (!path) {
      setStatus("ワークブックのパスを解決できません");
      return;
    }
    try {
      await openPath(path);
    } catch (e) {
      setStatus(String(e));
    }
  };

  const unlinkBom = async () => {
    if (!doc?.id) return;
    if (!(await flushLinkedMeta())) return;
    if (
      !window.confirm(
        "リンクを解除しますか？\n現在の表示内容は従来 BOM として残ります（Excel との同期は停止します）。",
      )
    )
      return;
    try {
      await api.excelLinkWatch(doc.id, false).catch(() => {});
      await api.excelLinkUnlink(doc.id);
      setStatus("リンクを解除しました");
      await openBom(doc.id); // 従来 BOM として開き直す
      await reloadList();
    } catch (e) {
      setStatus(String(e));
    }
  };

  /** Confirm 候補の確定 (部分確定可 — 残異常は返却 view の Confirm verdict に残る)。 */
  const confirmLinkCandidates = async (accepted: Parameters<typeof api.excelLinkConfirm>[1]["accepted"]) => {
    if (!doc?.id || link?.verdict.kind !== "confirm") return;
    if (!(await flushLinkedMeta())) return;
    setLinkBusy(true);
    try {
      const view = await api.excelLinkConfirm(doc.id, {
        structureFp: link.verdict.structureFp,
        accepted,
      });
      adoptLinkView(view);
      if (view.verdict.kind !== "confirm") setLinkConfirmOpen(false);
      setStatus(view.syncStatus === "linked" ? "変更を確定しました" : "残りの変更を確認してください");
    } catch (e) {
      setStatus(String(e));
    } finally {
      setLinkBusy(false);
    }
  };

  const resolveLinkConflict = async (backupId: number) => {
    if (!doc?.id) return;
    if (!(await flushLinkedMeta())) return;
    setLinkBusy(true);
    try {
      await api.excelLinkResolveConflict(doc.id, backupId);
      const view = await api.excelLinkOpen(doc.id);
      adoptLinkView(view);
      const st = await api.excelLinkStatus(doc.id);
      setLink((l) => (l ? { ...l, status: st } : l));
      if (st.conflicts.length === 0) {
        setLinkConflictOpen(false);
        setStatus("競合を解決しました（同期を再開します）");
      }
    } catch (e) {
      setStatus(String(e));
    } finally {
      setLinkBusy(false);
    }
  };

  /** リンク作成 (一覧から): ファイル選択 → probe → ウィザード。 */
  const startLinkCreate = async () => {
    try {
      const picked = await api.pickSpreadsheetToOpen();
      if (!picked) return;
      setStatus("ファイルを確認しています…");
      const probe = await api.excelLinkProbe(picked.path);
      setStatus("");
      setLinkWizard({
        mode: { kind: "create", name: picked.name },
        workbookPath: picked.path,
        probe,
      });
    } catch (e) {
      setStatus(String(e));
    }
  };

  /** 再マッピング (Broken 修復・要確認からの再指定)。既存契約の列を初期値に。 */
  const startLinkRemap = async () => {
    if (!(await flushLinkedMeta())) return;
    const path = linkWorkbookPath();
    if (!doc?.id || !path) {
      setStatus("ワークブックのパスを解決できません（リンクを解除して作り直してください）");
      return;
    }
    try {
      setStatus("ファイルを確認しています…");
      const probe = await api.excelLinkProbe(path);
      setStatus("");
      setLinkConfirmOpen(false);
      setLinkWizard({
        mode: { kind: "remap", bomId: doc.id, current: { columns: link?.columnsMeta ?? [] } },
        workbookPath: path,
        probe,
      });
    } catch (e) {
      setStatus(String(e));
    }
  };

  const linkWizardDone = async (view: LinkedBomView) => {
    setLinkWizard(null);
    adoptLinkView(view);
    setView("editor");
    setStatus(
      view.syncStatus === "linked"
        ? "Excel にリンクしました"
        : "リンクしました（確認が必要な項目があります）",
    );
    await reloadList();
  };

  const selectedRows = (): BomRow[] => gridRef.current?.api?.getSelectedRows() ?? [];

  const addRow = () =>
    setDoc((d) => (d ? { ...d, rows: [...d.rows, newRow(nextNo(d.rows))] } : d));

  const dupRows = () => {
    if (!doc) return;
    const sel = selectedRows();
    if (sel.length === 0) {
      setStatus("複製する行を選択してください");
      return;
    }
    // Duplicate values but assign fresh ids and sequential No. (No. must not duplicate).
    // Deep-copy nested data so the clone does not share `custom` (and later `supplier`)
    // objects with the source row — the custom valueSetter mutates row.custom in place.
    let n = nextNo(doc.rows);
    const clones = sel.map((r) => ({
      ...r,
      id: crypto.randomUUID(),
      no: n++,
      custom: { ...r.custom },
      supplier: r.supplier ? structuredClone(r.supplier) : undefined,
    }));
    setDoc({ ...doc, rows: [...doc.rows, ...clones] });
  };

  const renumberNo = () => {
    if (!doc) return;
    setDoc({ ...doc, rows: doc.rows.map((r, i) => ({ ...r, no: i + 1 })) });
    setStatus("No. を振り直しました");
  };

  const delRows = () => {
    const ids = new Set(selectedRows().map((r) => r.id));
    if (!doc || ids.size === 0) {
      setStatus("削除する行を選択してください");
      return;
    }
    setDoc({ ...doc, rows: doc.rows.filter((r) => !ids.has(r.id)) });
  };

  const addColumn = (label: string) => {
    if (!doc || !label.trim()) return;
    const key = uniqueKey(doc, slug(label));
    setDoc({
      ...doc,
      columns: [
        ...doc.columns,
        { key, label: label.trim(), kind: "custom", editable: true, width: 140 },
      ],
    });
  };

  const renameColumn = (key: string, label: string) => {
    if (!doc) return;
    setDoc({ ...doc, columns: doc.columns.map((c) => (c.key === key ? { ...c, label } : c)) });
  };

  const deleteColumn = (key: string) => {
    if (!doc) return;
    const col = doc.columns.find((c) => c.key === key);
    if (!col || col.kind === "core") return; // only core columns are protected
    // Deleting the current 型番列 changes the fetch identity — partNoColumn() falls back to
    // the default partsNo column, so previously fetched EC results no longer correspond to
    // the (now different) part number. Clear supplier from all rows (mirrors setColumnRole).
    // Deleting the 発注先列 only shifts the ORDER gate to the fallback column, which
    // supplierActive re-evaluates live, so supplier need not be cleared there.
    const removingPartNo = partNoColumn(doc)?.key === key;
    const columns = doc.columns.filter((c) => c.key !== key);
    const rows = removingPartNo
      ? doc.rows.map((r) => (r.supplier ? { ...r, supplier: undefined } : r))
      : doc.rows;
    setDoc({ ...doc, columns, rows });
  };

  // Designate which column plays a fetch role (型番列 / EC発注先列). Each role has at most
  // one column, so assigning it clears any previous holder. A role column is a fetch key,
  // never an EC fill target, so we also drop any EC link it may have carried.
  const setColumnRole = (role: ColumnRole, key: string) => {
    if (!doc) return;
    const prevPartKey = partNoColumn(doc)?.key;
    const columns = doc.columns.map((c) => {
      if (c.key === key) return { ...c, role, link: undefined };
      if (c.role === role) return { ...c, role: undefined };
      return c;
    });
    // Reassigning the 型番列 changes the fetch identity — previously fetched EC results no
    // longer correspond to the new part number, so clear supplier from all rows (re-fetch
    // resets them). Reassigning the 発注先列 doesn't change identity (supplierActive gates
    // it live), so leave supplier intact there.
    const rows =
      role === "partNo" && prevPartKey !== key
        ? doc.rows.map((r) => (r.supplier ? { ...r, supplier: undefined } : r))
        : doc.rows;
    setDoc({ ...doc, columns, rows });
  };

  // Map an EC field to a target column (field-anchored, per plan §7.3「フィールドを列に結ぶ」).
  // Each field binds at most one column; binding clears the field from any previous holder.
  // columnKey === null unbinds the field. Applied immediately to current supplier results.
  const setFieldLink = (field: string, columnKey: string | null, write: WritePolicy) => {
    if (!doc) return;
    const columns = doc.columns.map((c) => {
      if (c.link?.field === field && c.key !== columnKey) return { ...c, link: undefined };
      if (columnKey && c.key === columnKey) return { ...c, link: { field, write } };
      return c;
    });
    setDoc(applyLinkedColumns({ ...doc, columns }));
  };

  const addSupplierColumn = (field: string, label: string) => {
    if (!doc) return;
    if (doc.columns.some((c) => c.kind === "supplier" && c.link?.field === field)) return;
    const def = SUPPLIER_FIELDS.find((s) => s.field === field);
    const col = newSupplierColumn(field, label, def?.width);
    const keys = new Set(doc.columns.map((c) => c.key));
    let key = col.key;
    let i = 1;
    while (keys.has(key)) key = `${col.key}_${i++}`;
    setDoc({ ...doc, columns: [...doc.columns, { ...col, key }] });
  };

  // Fetch quotes for ORDER=MISUMI rows and apply them (cache-first; force re-fetch).
  const runQuote = async (force: boolean) => {
    if (!doc || quoting) return;
    const partCol = partNoColumn(doc);
    const srcCol = sourceColumn(doc);
    if (!partCol || !srcCol) {
      setStatus("型番列 / EC発注先列 が未設定です（列の管理で設定）");
      return;
    }
    const partOf = (r: BomRow) => String(getCellValue(r, partCol) ?? "").trim();
    const isMisumi = (r: BomRow) =>
      String(getCellValue(r, srcCol) ?? "")
        .trim()
        .toUpperCase() === "MISUMI";
    const targets = doc.rows.filter((r) => isMisumi(r) && partOf(r) !== "");
    if (targets.length === 0) {
      setStatus("EC発注先=MISUMI かつ 型番のある行がありません");
      return;
    }
    setQuoting(true);
    setStatus("取得を開始しています…");
    let unlisten: (() => void) | undefined;
    try {
      unlisten = await api.onQuoteProgress((p) =>
        setStatus(p.total > 0 ? `取得中 ${p.done}/${p.total}…` : "キャッシュから取得中…"),
      );
      const items = targets.map((r) => ({ partNo: partOf(r) }));
      if (link && doc.id) {
        // リンク BOM: 採用スナップショット (DB 正本) に取り込まれるので、再 open で
        // 合成済み表示に統一する (§2.3)。失敗行は「前回値・手動値の可能性あり」(§0)。
        // 未保存のメタ編集は再 open で消えるため先に確定 (レビュー #2)。
        if (!(await flushLinkedMeta())) return;
        const { results, generation, failed } = await api.quote("MISUMI", items, force, doc.id);
        const view = await api.excelLinkOpen(doc.id);
        adoptLinkView(view);
        const failNote = failed.length
          ? ` / 失敗 ${failed.length} 型番 — 前回値・手動値の可能性あり`
          : "";
        const genNote = generation != null ? `・第${generation}世代` : "";
        setStatus(`取得完了（${results.length} 件${failNote}${genNote}）`);
        return;
      }
      const { results } = await api.quote("MISUMI", items, force, doc.id);
      const byId = new Map<string, SupplierQuote>();
      // Store the raw quote only. Subtotal and the MOQ note are derived LIVE in the
      // supplier column getters from the row's current Qty/multiplier, so they stay
      // correct after the user edits Qty (no stale stamped values).
      targets.forEach((r, idx) => {
        const q = results[idx];
        if (q) byId.set(r.id, q);
      });
      // Apply fresh quotes to targets; clear stale supplier data from rows that are no
      // longer MISUMI targets (ORDER changed away / Parts No removed) so re-fetch resets them.
      setDoc((d) => {
        if (!d) return d;
        const rows = d.rows.map((r) =>
          byId.has(r.id)
            ? { ...r, supplier: byId.get(r.id) }
            : r.supplier
              ? { ...r, supplier: undefined }
              : r,
        );
        // Apply linked-column write policies (fillEmpty/overwrite) with the fresh results.
        return applyLinkedColumns({ ...d, rows });
      });
      const errs = results.filter((q) => q?.status === "error").length;
      setStatus(`取得完了（${results.length} 件${errs ? ` / エラー ${errs}` : ""}）`);
    } catch (e) {
      setStatus(String(e));
    } finally {
      unlisten?.();
      setQuoting(false);
    }
  };

  // Collect MISUMI cart lines. If rows are checked (selected), use ONLY those; otherwise all
  // rows. In both cases keep ORDER=MISUMI + 型番あり, merged by part number with qty summed as
  // Qty × 数量倍率 (min 1 per part).
  const collectCartItems = ():
    | { items: api.CartItem[]; count: number; totalQty: number; mode: "selection" | "all" }
    | null => {
    if (!doc) return null;
    const partCol = partNoColumn(doc);
    const srcCol = sourceColumn(doc);
    if (!partCol || !srcCol) {
      setStatus("型番列 / EC発注先列 が未設定です（列の管理で設定）");
      return null;
    }
    const orderNoCols = orderNoColumns(doc);
    const sep = doc.meta.orderNoSeparator ?? DEFAULT_ORDER_NO_SEPARATOR;
    const partOf = (r: BomRow) => String(getCellValue(r, partCol) ?? "").trim();
    // Join the assigned お客様注文番号1/2/3 slots (non-empty only) into a single value.
    const orderNoOf = (r: BomRow) =>
      orderNoCols
        .map((c) => String(getCellValue(r, c) ?? "").trim())
        .filter((v) => v !== "")
        .join(sep);
    const isMisumi = (r: BomRow) =>
      String(getCellValue(r, srcCol) ?? "")
        .trim()
        .toUpperCase() === "MISUMI";
    const mult = doc.meta.qtyMultiplier ?? 1;
    // Checked rows scope the cart add; with none checked, fall back to the whole BOM.
    const sel = selectedRows();
    const useSelection = sel.length > 0;
    const selIds = new Set(sel.map((r) => r.id));
    const scope = useSelection ? doc.rows.filter((r) => selIds.has(r.id)) : doc.rows;
    // Merge by 型番 + お客様注文番号: same part with a different order number stays a separate
    // cart line (each carries its own customerItemSubReference); same part+number sums qty.
    const merged = new Map<string, { part: string; orderNo: string; qty: number }>();
    for (const r of scope) {
      if (!isMisumi(r)) continue;
      const p = partOf(r);
      if (!p) continue;
      const orderNo = orderNoOf(r);
      const key = `${p}\x00${orderNo}`;
      const base = r.qty && r.qty > 0 ? r.qty : 1;
      const add = Math.max(1, Math.round(base * mult));
      const cur = merged.get(key);
      if (cur) cur.qty += add;
      else merged.set(key, { part: p, orderNo, qty: add });
    }
    if (merged.size === 0) {
      setStatus(
        useSelection
          ? "チェックした行に EC発注先=MISUMI かつ 型番のある行がありません"
          : "EC発注先=MISUMI かつ 型番のある行がありません",
      );
      return null;
    }
    const items: api.CartItem[] = Array.from(merged.values(), (m) => ({
      inputProductCode: m.part,
      qty: m.qty,
      ...(m.orderNo ? { customerItemSubReference: m.orderNo } : {}),
    }));
    const totalQty = items.reduce((s, it) => s + it.qty, 0);
    return { items, count: items.length, totalQty, mode: useSelection ? "selection" : "all" };
  };

  // Toolbar「カートに追加」: gather targets and open the confirmation dialog.
  const addToCart = () => {
    if (addingCart) return;
    const collected = collectCartItems();
    if (collected) setConfirmCart(collected);
  };

  // POST cart-detail/add via the bridge. If not logged in, remember the pending items and
  // raise the in-app login prompt (rather than silently popping the bridge window) so the
  // separate login window has clear in-app context. No order is placed — cart only.
  const runCartAdd = async (collected: {
    items: api.CartItem[];
    count: number;
    totalQty: number;
    mode: "selection" | "all";
  }) => {
    setAddingCart(true);
    setCartDone(false);
    setStatus("カートに追加中…");
    try {
      const res = await api.cartAdd("MISUMI", collected.items);
      if (!res.ok && (res.error === "NOT_LOGGED_IN" || res.error === "AUTH_EXPIRED")) {
        pendingCartRef.current = collected;
        setLoginPrompt({ phase: "ask" });
        setStatus("");
        return;
      }
      if (res.ok) {
        setStatus(
          `MISUMI カートに ${collected.count} 型番（合計 ${collected.totalQty} 個）を追加しました`,
        );
        setCartDone(true);
      } else {
        setStatus(`カート追加に失敗しました: ${res.error ?? "不明なエラー"}`);
      }
    } catch (e) {
      setStatus(String(e));
    } finally {
      setAddingCart(false);
    }
  };

  // Confirm dialog accepted → attempt the add.
  const doAddToCart = () => {
    const collected = confirmCart;
    setConfirmCart(null);
    if (!collected || addingCart) return;
    void runCartAdd(collected);
  };

  // Login prompt「ログイン画面を開く」/「再試行」: show the bridge for login, then on success
  // close the prompt and continue the pending cart add automatically.
  const startLogin = async () => {
    setLoginPrompt({ phase: "waiting" });
    try {
      const { loggedIn } = await api.misumiLogin();
      if (loggedIn) {
        setLoginPrompt(null);
        const collected = pendingCartRef.current;
        if (collected) void runCartAdd(collected);
      } else {
        setLoginPrompt({ phase: "failed" });
      }
    } catch (e) {
      setStatus(String(e));
      setLoginPrompt({ phase: "failed" });
    }
  };

  const undo = () => gridRef.current?.api?.undoCellEditing();
  const redo = () => gridRef.current?.api?.redoCellEditing();
  const autoSize = () => gridRef.current?.api?.autoSizeAllColumns();

  const moveColumn = (key: string, dir: -1 | 1) => {
    if (!doc) return;
    const i = doc.columns.findIndex((c) => c.key === key);
    const j = i + dir;
    if (i < 0 || j < 0 || j >= doc.columns.length) return;
    const cols = [...doc.columns];
    [cols[i], cols[j]] = [cols[j], cols[i]];
    setDoc({ ...doc, columns: cols });
  };

  const saveBom = async () => {
    if (!doc) return;
    // リンク BOM の列・行は Excel が正 (save_bom はバックエンドでも拒否される)。
    // 保存できるのはメタ (名前等) だけ。
    if (link) {
      if (!doc.id) return;
      try {
        await api.bomUpdateMeta(doc.id, doc.meta);
        savedSnapRef.current = JSON.stringify(doc);
        setStatus("設定を保存しました");
      } catch (e) {
        setStatus(String(e));
      }
      return;
    }
    try {
      const id = await api.bomSave(doc);
      const saved = { ...doc, id };
      setDoc(saved);
      savedSnapRef.current = JSON.stringify(saved);
      setStatus("保存しました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doBack = async () => {
    setConfirmBack(false);
    setView("list");
    setDoc(null);
    setLink(null);
    setLinkConfirmOpen(false);
    setLinkConflictOpen(false);
    setStatus("");
    await reloadList();
  };

  // Leaving the editor with unsaved changes prompts 保存/破棄/キャンセル (Windows convention).
  const back = () => {
    if (link) {
      // リンク BOM はグリッド編集不可 — 未保存はメタ (名前・倍率) のみ。保存を
      // await し、失敗したら一覧へ戻らずエラーを表示する (レビュー #2)。
      void (async () => {
        if (!(await flushLinkedMeta())) return;
        await doBack();
      })();
      return;
    }
    if (doc && JSON.stringify(doc) !== savedSnapRef.current) {
      setConfirmBack(true);
      return;
    }
    void doBack();
  };

  const saveAndBack = async () => {
    if (!doc) return;
    try {
      await api.bomSave(doc);
    } catch (e) {
      setConfirmBack(false);
      setStatus(String(e));
      return;
    }
    await doBack();
  };

  // Import: pick an Excel/CSV file, parse it to a grid, and open the mapping wizard.
  const doImport = async () => {
    try {
      const picked = await api.pickSpreadsheetToOpen();
      if (!picked) return;
      setStatus("ファイルを読み込み中…");
      const workbook = await api.readSpreadsheet(picked.path);
      setImportSrc({ workbook, fileName: picked.name, path: picked.path });
      setStatus("");
    } catch (e) {
      setStatus(String(e));
    }
  };

  // Wizard confirmed: persist the built BomDoc as a new BOM and open it. When the user opted
  // in, open 列管理 on the 列の構成 tab — imported BOMs have no EC columns yet, and that tab
  // is where "EC連携項目を列に追加" lives (the 取得・連携 tab only links to existing columns).
  const confirmImport = async (built: BomDoc, openEcSetup: boolean) => {
    setImportSrc(null);
    try {
      const id = await api.bomSave(built);
      await reloadList();
      await openBom(id);
      setStatus(`取込しました（${built.rows.length} 行）`);
      if (openEcSetup) {
        setColumnsTab("columns");
        setShowColumns(true);
      }
    } catch (e) {
      setStatus(String(e));
    }
  };

  const doExport = async () => {
    if (!doc) return;
    try {
      const { headers, rows } = buildExportGrid(doc);
      if (await api.exportSpreadsheet(doc, headers, rows)) setStatus("書き出しました");
    } catch (e) {
      setStatus(String(e));
    }
  };

  // Resolve what the history drawer should show for a row: only rows whose ORDER is an
  // EC-supported source (currently just MISUMI) have history; otherwise carry the reason so
  // the drawer can explain it (not-ec / no-part / no-row) rather than "まだありません".
  const historyTargetOf = (row: BomRow | null): HistoryTarget => {
    if (!row || !doc) return { kind: "empty", reason: "no-row" };
    const srcCol = sourceColumn(doc);
    const src = String((srcCol ? getCellValue(row, srcCol) : row.order) ?? "")
      .trim()
      .toUpperCase();
    if (!SUPPORTED_EC.includes(src)) return { kind: "empty", reason: "not-ec" };
    const partCol = partNoColumn(doc);
    const partNo = partCol ? String(getCellValue(row, partCol) ?? "").trim() : "";
    if (!partNo) return { kind: "empty", reason: "no-part" };
    return { kind: "row", partNo, supplier: src };
  };

  // The row the history button should act on. Prefer the user's SELECTION; only use the
  // grid's focused row when there is no selection, or when the focused row is itself part of
  // the selection. This avoids opening a different row's history than the one the user picked
  // (e.g. after a Ctrl / multi-row selection leaves focus on a non-selected row).
  const historyButtonRow = (): BomRow | null => {
    const gridApi = gridRef.current?.api;
    if (!gridApi) return null;
    const selected = gridApi.getSelectedRows();
    const fc = gridApi.getFocusedCell();
    const focusedRow = fc ? (gridApi.getDisplayedRowAtIndex(fc.rowIndex)?.data ?? null) : null;
    if (focusedRow && (selected.length === 0 || selected.some((r) => r.id === focusedRow.id))) {
      return focusedRow;
    }
    return selected[0] ?? focusedRow ?? null;
  };

  // Toolbar toggle for the history drawer. When opening, seed it with the selected/focused
  // row so it shows something immediately; while open it follows the active cell via
  // onActiveRowChange.
  const toggleHistory = () => {
    if (historyOpen) {
      setHistoryOpen(false);
      return;
    }
    setActiveTarget(historyTargetOf(historyButtonRow()));
    setHistoryOpen(true);
  };

  const doDelete = async (id: string) => {
    try {
      await api.bomDelete(id);
      await reloadList();
    } catch (e) {
      setStatus(String(e));
    }
  };

  return (
    <div className="app">
      {view === "list" || !doc ? (
        <BomList
          list={list}
          status={status}
          onOpen={openBom}
          onNew={createBom}
          onDelete={doDelete}
          onImport={doImport}
          onLinkExcel={() => void startLinkCreate()}
        />
      ) : (
        <>
          <Toolbar
            title={doc.meta.name ?? ""}
            status={status}
            quickFilter={quickFilter}
            onBack={back}
            onSave={saveBom}
            onAddRow={addRow}
            onDupRows={dupRows}
            onDelRows={delRows}
            onRenumber={renumberNo}
            onManageColumns={() => {
              setColumnsTab("columns");
              setShowColumns(true);
            }}
            onHistory={toggleHistory}
            historyOpen={historyOpen}
            onExport={doExport}
            onRename={(name) => setDoc({ ...doc, meta: { ...doc.meta, name } })}
            onQuickFilter={setQuickFilter}
            onUndo={undo}
            onRedo={redo}
            onAutoSize={autoSize}
            onQuote={runQuote}
            quoting={quoting}
            onAddToCart={addToCart}
            addingCart={addingCart}
            link={
              link
                ? {
                    onEditInExcel: () => void editInExcel(),
                    onRefresh: () => void refreshLink(),
                    onApply: () => void applyLink(),
                    onUnlink: () => void unlinkBom(),
                    refreshing: linkBusy,
                    applying: linkBusy,
                    calcUsable: calcUsable(link.calcState),
                  }
                : undefined
            }
          />
          {link && (
            <LinkBanner
              syncStatus={link.syncStatus}
              calcState={link.calcState}
              envVerdict={link.env.verdict}
              verdict={link.verdict}
              pending={link.pending}
              status={link.status}
              warnings={link.warnings}
              qtyMultiplier={doc.meta.qtyMultiplier ?? 1}
              onQtyMultiplier={(m) => void setLinkMultiplier(m)}
              onOpenConfirm={() => setLinkConfirmOpen(true)}
              onOpenConflict={() => setLinkConflictOpen(true)}
              onRemap={() => void startLinkRemap()}
              onApply={() => void applyLink()}
            />
          )}
          <BomEditor
            doc={doc}
            onChange={setDoc}
            gridRef={gridRef}
            quickFilter={quickFilter}
            onActiveRowChange={(row) => setActiveTarget(historyTargetOf(row))}
            link={
              link
                ? ({
                    formulaCells: link.formulaCells,
                    columnsMeta: link.columnsMeta,
                  } satisfies LinkGridOptions)
                : undefined
            }
          />
          {historyOpen && (
            <HistoryDrawer target={activeTarget} onClose={() => setHistoryOpen(false)} />
          )}
          <SummaryBar doc={doc} />
          {cartDone && (
            <div className="cart-toast">
              <span>MISUMI カートに追加しました。</span>
              <button className="primary" onClick={() => void api.misumiOpenCart()}>
                カートを開く
              </button>
              <button onClick={() => setCartDone(false)}>閉じる</button>
            </div>
          )}
          {confirmCart && (
            <div className="col-mgr-backdrop" onClick={() => setConfirmCart(null)}>
              <div className="confirm-dlg" onClick={(e) => e.stopPropagation()}>
                <h3>MISUMI カートに追加</h3>
                <p>
                  {confirmCart.mode === "selection"
                    ? `チェックした ${confirmCart.count} 型番`
                    : `ORDER=MISUMI の ${confirmCart.count} 型番`}
                  （合計 {confirmCart.totalQty} 個）を MISUMI のカートに追加します。
                  <br />
                  これは注文ではありません（カートに入るだけで、いつでも削除できます）。
                </p>
                <div className="confirm-actions">
                  <button className="primary" onClick={doAddToCart}>
                    カートに追加
                  </button>
                  <button onClick={() => setConfirmCart(null)}>キャンセル</button>
                </div>
              </div>
            </div>
          )}
          {loginPrompt && (
            <div
              className="col-mgr-backdrop"
              onClick={() => loginPrompt.phase !== "waiting" && setLoginPrompt(null)}
            >
              <div className="confirm-dlg" onClick={(e) => e.stopPropagation()}>
                <h3>MISUMI へのログインが必要です</h3>
                {loginPrompt.phase === "ask" && (
                  <>
                    <p>
                      カートに追加するには MISUMI へのログインが必要です。
                      <br />
                      「ログイン画面を開く」を押すと別ウィンドウが開きます。ログインが完了すると自動でカートに追加します。
                    </p>
                    <div className="confirm-actions">
                      <button className="primary" onClick={startLogin}>
                        ログイン画面を開く
                      </button>
                      <button
                        onClick={() => {
                          setLoginPrompt(null);
                          setStatus("キャンセルしました");
                        }}
                      >
                        キャンセル
                      </button>
                    </div>
                  </>
                )}
                {loginPrompt.phase === "waiting" && (
                  <p>
                    開いたウィンドウで MISUMI にログインしてください。
                    <br />
                    ログインを検知すると、自動でウィンドウを閉じてカートに追加します…
                  </p>
                )}
                {loginPrompt.phase === "failed" && (
                  <>
                    <p>ログインが確認できませんでした。もう一度お試しください。</p>
                    <div className="confirm-actions">
                      <button className="primary" onClick={startLogin}>
                        再試行
                      </button>
                      <button onClick={() => setLoginPrompt(null)}>閉じる</button>
                    </div>
                  </>
                )}
              </div>
            </div>
          )}
          {confirmBack && (
            <div className="col-mgr-backdrop" onClick={() => setConfirmBack(false)}>
              <div className="confirm-dlg" onClick={(e) => e.stopPropagation()}>
                <h3>変更が保存されていません</h3>
                <p>
                  「{doc.meta.name?.trim() || "無題の BOM"}」への変更を保存しますか？
                  <br />
                  保存せずに戻ると、編集内容と取得結果は失われます。
                </p>
                <div className="confirm-actions">
                  <button className="primary" onClick={saveAndBack}>
                    保存して戻る
                  </button>
                  <button onClick={doBack}>保存せずに戻る</button>
                  <button onClick={() => setConfirmBack(false)}>キャンセル</button>
                </div>
              </div>
            </div>
          )}
          {showColumns && (
            <ColumnManager
              columns={doc.columns}
              initialTab={columnsTab}
              orderNoSeparator={doc.meta.orderNoSeparator ?? DEFAULT_ORDER_NO_SEPARATOR}
              onAdd={addColumn}
              onAddSupplier={addSupplierColumn}
              onSetFieldLink={setFieldLink}
              onSetRole={setColumnRole}
              onSetOrderNoSeparator={(sep) =>
                setDoc({ ...doc, meta: { ...doc.meta, orderNoSeparator: sep } })
              }
              onRename={renameColumn}
              onDelete={deleteColumn}
              onMove={moveColumn}
              onClose={() => setShowColumns(false)}
            />
          )}
        </>
      )}
      {linkConfirmOpen && link?.verdict.kind === "confirm" && (
        <LinkConfirmDialog
          // 部分確定で verdict が変わったら選択状態ごと remount する (レビュー #1:
          // 旧 index が新候補配列へ適用される誤選択を防ぐ)。
          key={link.verdict.structureFp}
          verdict={link.verdict}
          busy={linkBusy}
          onConfirm={(accepted) => void confirmLinkCandidates(accepted)}
          onRemap={() => void startLinkRemap()}
          onClose={() => setLinkConfirmOpen(false)}
        />
      )}
      {linkConflictOpen && (
        <LinkConflictDialog
          conflicts={link?.status?.conflicts ?? []}
          busy={linkBusy}
          onResolve={(id) => void resolveLinkConflict(id)}
          onOpenFolder={(path) => void revealItemInDir(path).catch((e) => setStatus(String(e)))}
          onClose={() => setLinkConflictOpen(false)}
        />
      )}
      {linkWizard && (
        <LinkWizard
          mode={linkWizard.mode}
          workbookPath={linkWizard.workbookPath}
          probe={linkWizard.probe}
          onCancel={() => setLinkWizard(null)}
          onDone={(v) => void linkWizardDone(v)}
        />
      )}
      {importSrc && (
        <ImportWizard
          workbook={importSrc.workbook}
          fileName={importSrc.fileName}
          path={importSrc.path}
          onCancel={() => setImportSrc(null)}
          onConfirm={confirmImport}
        />
      )}
    </div>
  );
}
