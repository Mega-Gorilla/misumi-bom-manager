// Thin invoke wrappers for the BOM backend commands + Excel/CSV import/export.
// File IO goes through backend commands (spreadsheet_read/spreadsheet_write); the
// dialog plugin only picks the path. BomDoc construction / column mapping is done on
// the frontend (import wizard) so the backend stays a thin file<->grid converter.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { BomDoc, BomMeta, BomSummary, SupplierQuote, Workbook } from "../types/bom";
import type {
  ApplyOutcome,
  ConflictAction,
  LinkConfirmRequest,
  LinkCreateConfig,
  LinkProbe,
  LinkRemapRequest,
  LinkStatus,
  LinkedBomView,
} from "../types/link";

export const bomList = (): Promise<BomSummary[]> => invoke("bom_list");
export const bomLoad = (id: string): Promise<BomDoc | null> => invoke("bom_load", { id });
export const bomSave = (doc: BomDoc): Promise<string> => invoke("bom_save", { doc });
export const bomDelete = (id: string): Promise<void> => invoke("bom_delete", { id });
/** Meta-only save (名前 / 数量倍率 / 注文番号区切り) — リンク BOM が使える唯一の保存経路。 */
export const bomUpdateMeta = (id: string, meta: BomMeta): Promise<void> =>
  invoke("bom_update_meta", { id, meta });

// ---- Excel リンクモード (docs/plans/0018-excel-link-mode) ----

/** リンクウィザードの下見: シート一覧・ヘッダ候補・環境判定 (読み取りのみ)。 */
export const excelLinkProbe = (path: string): Promise<LinkProbe> =>
  invoke("excel_link_probe", { path });
/** リンク作成 (config.bomId 指定で既存 BOM の昇格)。 */
export const excelLinkCreate = (config: LinkCreateConfig): Promise<LinkedBomView> =>
  invoke("excel_link_create", { config });
/** 読込+構造検証+復元規則+合成。初回表示・「更新」の共通入口。 */
export const excelLinkOpen = (bomId: string): Promise<LinkedBomView> =>
  invoke("excel_link_open", { bomId });
/** リンク解除 (スナップショットを残し従来 BOM 化)。 */
export const excelLinkUnlink = (bomId: string): Promise<void> =>
  invoke("excel_link_unlink", { bomId });
/** 「Excel へ反映」(§4.2.2 手順1〜9)。手動再試行も同コマンド。 */
export const excelLinkApply = (bomId: string): Promise<ApplyOutcome> =>
  invoke("excel_link_apply", { bomId });
/** 軽量ステータス (DB 射影+~$ 存在チェック1回) — ポーリング可。 */
export const excelLinkStatus = (bomId: string): Promise<LinkStatus> =>
  invoke("excel_link_status", { bomId });
/** Confirm 判定の候補確定 (structureFp echo back・部分確定可)。 */
export const excelLinkConfirm = (
  bomId: string,
  resolution: LinkConfirmRequest,
): Promise<LinkedBomView> => invoke("excel_link_confirm", { bomId, resolution });
/** 競合解決の記録 (§1.3 同期停止の解除)。フォルダを開くのはフロント (opener)。 */
export const excelLinkResolveConflict = (
  bomId: string,
  backupId: number,
  action: ConflictAction = "resolved",
): Promise<void> => invoke("excel_link_resolve_conflict", { bomId, backupId, action });
/** 再マッピング (Broken 修復) — 契約全置換・state/世代/EC スナップショット保持。 */
export const excelLinkRemap = (
  bomId: string,
  request: LinkRemapRequest,
): Promise<LinkedBomView> => invoke("excel_link_remap", { bomId, request });
/** ファイル監視の開始/停止 (PR-7・§4.2.3)。検知は下記イベントで通知される。 */
export const excelLinkWatch = (bomId: string, enable: boolean): Promise<void> =>
  invoke("excel_link_watch", { bomId, enable });

/** watch の合成イベント payload。 */
export interface LinkWatchPayload {
  bomId: string;
}
/** ワークブック変化 (Excel 保存・外部書込・同期到着) — 受けたら excelLinkOpen で再読込。 */
export const onExcelLinkChanged = (
  cb: (p: LinkWatchPayload) => void,
): Promise<UnlistenFn> =>
  listen<LinkWatchPayload>("excel-link:changed", (e) => cb(e.payload));
/** `~$` オーナーファイル消滅 = Excel が閉じた — 反映待ちがあれば excelLinkApply を再試行 (§9-5)。 */
export const onExcelLinkExcelClosed = (
  cb: (p: LinkWatchPayload) => void,
): Promise<UnlistenFn> =>
  listen<LinkWatchPayload>("excel-link:excel-closed", (e) => cb(e.payload));

/** One price/delivery observation for a (supplier, part number), newest first. */
export interface PriceHistoryEntry {
  fetchedAt: string;
  unitPrice?: string | null;
  currency?: string | null;
  shipDate?: string | null;
  /** Immediate-shippable stock at fetch time (recorded from schema V3 on; null before). */
  stock?: number | null;
}

/** Read the append-only price/delivery history for a part number (all-BOM, cross-cache). */
export const priceHistory = (
  supplier: string,
  partNo: string,
  limit = 60,
): Promise<PriceHistoryEntry[]> => invoke("price_history", { supplier, partNo, limit });

/** A picked spreadsheet file: its absolute path + base name (for the new BOM title). */
export interface PickedFile {
  path: string;
  name: string;
}

/** Pick an Excel/CSV file to import. Returns path + base name, or null if cancelled. */
export async function pickSpreadsheetToOpen(): Promise<PickedFile | null> {
  const path = await open({
    multiple: false,
    filters: [{ name: "Excel / CSV", extensions: ["xlsx", "xls", "csv"] }],
  });
  if (typeof path !== "string") return null;
  const base = path.split(/[\\/]/).pop() ?? path;
  const name = base.replace(/\.[^.]+$/, "");
  return { path, name };
}

/** Parse the picked file into a flat string grid (one entry per sheet). */
export const readSpreadsheet = (path: string): Promise<Workbook> =>
  invoke("spreadsheet_read", { path });

/** Pick a destination and export the grid as .xlsx or .csv (by chosen extension).
 *  Returns false if cancelled. */
export async function exportSpreadsheet(
  doc: BomDoc,
  headers: string[],
  rows: string[][],
): Promise<boolean> {
  const path = await save({
    defaultPath: `${doc.meta.name ?? "bom"}.xlsx`,
    filters: [
      { name: "Excel", extensions: ["xlsx"] },
      { name: "CSV", extensions: ["csv"] },
    ],
  });
  if (!path) return false;
  await invoke("spreadsheet_write", { path, headers, rows });
  return true;
}

// The quote cache is qty-agnostic (representative qty=1), so items carry only the
// part number. Subtotal and the per-row MOQ check use the row's Qty on the frontend.
export interface QuoteItem {
  partNo: string;
}

export interface QuoteProgress {
  supplier: string;
  done: number;
  total: number;
}

/** One failed part number of a quote run (unique parts, not per-row). */
export interface QuoteFailure {
  partsNo: string;
  message: string;
}

/** Outcome of a quote run. `generation` is set only when `bomId` names a LINKED
 *  BOM and its adopted snapshot actually changed (Excel link mode, PR-4). */
export interface QuoteOutcome {
  results: SupplierQuote[];
  generation?: number;
  failed: QuoteFailure[];
}

/** Fetch supplier quotes for items (cache-first; misses fetched in chunks).
 *  `results` has one quote per input item (aligned by index). Pass the BOM id so a
 *  linked BOM adopts the run into its snapshot; conventional BOMs are unaffected. */
export const quote = (
  supplier: string,
  items: QuoteItem[],
  force = false,
  bomId?: string,
): Promise<QuoteOutcome> => invoke("quote", { supplier, items, force, bomId });

/** Subscribe to backend `quote-progress` events. Await the returned fn to stop. */
export const onQuoteProgress = (cb: (p: QuoteProgress) => void): Promise<UnlistenFn> =>
  listen<QuoteProgress>("quote-progress", (e) => cb(e.payload));

// ---- MISUMI cart (add BOM rows to the logged-in cart via the bridge WebView) ----

/** One line to add to the cart. brandCode is optional (backend resolves it via suggest).
 *  customerItemSubReference is the お客様注文番号 (single field per line; omitted when empty). */
export interface CartItem {
  inputProductCode: string;
  qty: number;
  brandCode?: string;
  customerItemSubReference?: string;
}

/** Result of a cart-add attempt. `error` carries "NOT_LOGGED_IN" / "AUTH_EXPIRED" so the
 *  caller can trigger a login, or a MISUMI error message otherwise. */
export interface CartAddResult {
  ok: boolean;
  error?: string;
  status?: number;
  result?: unknown;
}

/** Add items to the supplier's cart (currently MISUMI). Requires a logged-in bridge. */
export const cartAdd = (supplier: string, items: CartItem[]): Promise<CartAddResult> =>
  invoke("cart_add", { supplier, items });

/** Current MISUMI auth state. `captured` = a Bearer is available (cart-add is callable). */
export const misumiAuthStatus = (): Promise<{ loggedIn: boolean; captured: boolean }> =>
  invoke("misumi_auth_status");

/** Show the bridge for the user to log in; resolves once logged in (or times out). */
export const misumiLogin = (): Promise<{ loggedIn: boolean }> => invoke("misumi_login");

/** Show the authenticated bridge navigated to the MISUMI cart/order page. */
export const misumiOpenCart = (): Promise<void> => invoke("misumi_open_cart");
