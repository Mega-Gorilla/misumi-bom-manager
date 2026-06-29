// Thin invoke wrappers for the BOM backend commands + JSON import/export.
// File IO goes through backend commands (bom_import/bom_export); the dialog
// plugin only picks the path. See plan PR-B notes.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { BomDoc, BomSummary, SupplierQuote } from "../types/bom";

export const bomList = (): Promise<BomSummary[]> => invoke("bom_list");
export const bomLoad = (id: string): Promise<BomDoc | null> => invoke("bom_load", { id });
export const bomSave = (doc: BomDoc): Promise<string> => invoke("bom_save", { doc });
export const bomDelete = (id: string): Promise<void> => invoke("bom_delete", { id });

/** Pick a JSON file and import it (backend assigns missing row IDs). Returns the new BOM id, or null if cancelled. */
export async function importJson(): Promise<string | null> {
  const path = await open({
    multiple: false,
    filters: [{ name: "BOM JSON", extensions: ["json"] }],
  });
  if (typeof path !== "string") return null;
  return invoke<string>("bom_import", { path });
}

/** Pick a destination and export the BOM as JSON. Returns false if cancelled. */
export async function exportJson(doc: BomDoc): Promise<boolean> {
  const path = await save({
    defaultPath: `${doc.meta.name ?? "bom"}.json`,
    filters: [{ name: "BOM JSON", extensions: ["json"] }],
  });
  if (!path) return false;
  await invoke("bom_export", { path, doc });
  return true;
}

export interface QuoteItem {
  partNo: string;
  qty: number;
}

export interface QuoteProgress {
  supplier: string;
  done: number;
  total: number;
}

/** Fetch supplier quotes for items (cache-first; misses fetched in chunks).
 *  Returns one quote per input item (aligned by index). */
export const quote = (
  supplier: string,
  items: QuoteItem[],
  force = false,
): Promise<SupplierQuote[]> => invoke("quote", { supplier, items, force });

/** Subscribe to backend `quote-progress` events. Await the returned fn to stop. */
export const onQuoteProgress = (cb: (p: QuoteProgress) => void): Promise<UnlistenFn> =>
  listen<QuoteProgress>("quote-progress", (e) => cb(e.payload));
