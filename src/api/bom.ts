// Thin invoke wrappers for the BOM backend commands + JSON import/export.
// File IO goes through backend commands (bom_import/bom_export); the dialog
// plugin only picks the path. See plan PR-B notes.

import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { BomDoc, BomSummary } from "../types/bom";

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
