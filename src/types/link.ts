// Excel リンクモードの IPC 型 — src-tauri/src/model.rs (+ excel_link/env.rs) の写像。
// wire 規約: 構造体・タグ付き enum のフィールドは camelCase (タグ付き enum は
// `rename_all_fields = "camelCase"` — PR-6 で統一)。文字列 enum の「値」だけは
// DB CHECK と揃えるため snake_case のまま (model.rs の設計判断)。
// wire 形は Rust 側の model::wire_tests が固定している。

import type { BomDoc } from "./bom";

// ---- 文字列 enum (snake_case 値・DB パリティ) ----

export type SyncStatus = "linked" | "needs_review" | "broken" | "conflict";
export type CalcState = "unverified" | "trusted" | "stale" | "missing";
export type EnvVerdict = "allow" | "no_writeback";
export type LinkOwnership = "app" | "user" | "skipped";
export type LinkProjection = "writeback" | "suggest";
export type RefuseReason =
  | "env"
  | "structure"
  | "fingerprint_changed"
  | "unresolved_conflict"
  | "spill"
  | "formula_cell"
  | "truncated"
  | "nothing_to_write";

// ---- 環境判定 (§4.10) ----

/** env.rs EnvCheck。Option は null で必ず出る (skip_serializing_if なし)。 */
export interface EnvCheck {
  verdict: EnvVerdict;
  resolvedPath: string | null;
  fsName: string | null;
  readTarget: string | null;
  /** NoWriteback のとき "env:<code>: <detail>"、Allow のとき null。 */
  reason: string | null;
}

// ---- ウィザード (probe / create / remap) ----

export interface SheetProbe {
  name: string;
  /** 使用範囲の先頭 20 行 × 30 列 (行の長さは不揃い)。 */
  preview: string[][];
  /** preview[0] の 1-based 絶対行番号 (使用範囲が 1 行目から始まるとは限らない)。 */
  startRow: number;
  /** 文字列セル最多の行 (1-based 絶対行番号)。 */
  suggestedHeaderRow?: number;
  truncated: boolean;
}

export interface LinkProbe {
  env: EnvCheck;
  sheets: SheetProbe[];
  warnings: string[];
}

export interface LinkColumnConfig {
  /** 0-based Excel 列 index。 */
  excelCol: number;
  headerLabel?: string;
  /** undefined = 読み飛ばし列 (skipped)。 */
  appKey?: string;
  ownership: LinkOwnership;
  required?: boolean;
  role?: string;
  /** EC 項目の dotted path または計算フィールド (quote.subtotal / quote.moqNote)。 */
  sourceField?: string;
  projection?: LinkProjection;
}

export interface LinkCreateConfig {
  /** 既存 BOM の昇格時のみ指定。 */
  bomId?: string;
  name?: string;
  workbookPath: string;
  sheetName: string;
  /** 1-based。 */
  headerRow: number;
  /** 1-based (> headerRow)。 */
  dataStartRow: number;
  columns: LinkColumnConfig[];
}

/** 再マッピング (Broken 修復)。契約全置換 — state/世代/EC スナップショットは保持。 */
export interface LinkRemapRequest {
  sheetName: string;
  headerRow: number;
  dataStartRow: number;
  columns: LinkColumnConfig[];
}

// ---- 構造判定・候補 (タグ付き enum: kind + camelCase フィールド) ----

export type LinkResolutionCandidate =
  | { kind: "adoptSheetRename"; newSheet: string }
  | { kind: "adoptHeaderRowMove"; newHeaderRow: number; newDataStartRow: number }
  | { kind: "adoptColumnMove"; appKey: string; newExcelCol: number }
  | { kind: "adoptRename"; appKey: string; newLabel: string }
  | { kind: "dropOptionalColumn"; appKey: string }
  | { kind: "importSkippedAsUser"; excelCol: number; label: string }
  | { kind: "keepSkipped"; excelCol: number; newLabel: string | null }
  | { kind: "skipColumn"; excelCol: number };

export type StructureVerdict =
  | { kind: "safe"; newColumns: string[] }
  | {
      kind: "confirm";
      reasons: string[];
      candidates: LinkResolutionCandidate[];
      /** confirm コマンドへ echo back する鮮度ガード。 */
      structureFp: string;
    }
  | { kind: "broken"; reasons: string[] };

// ---- リンク BOM ビュー ----

export interface LinkColumnMeta {
  appKey: string;
  /** 0-based。 */
  excelCol: number;
  /** skipped 列は columnsMeta に現れない (app/user のみ)。 */
  ownership: LinkOwnership;
  required: boolean;
  role?: string;
  sourceField?: string;
  projection?: LinkProjection;
}

/** 数式セル (0-based 絶対座標・fx 表示用)。 */
export interface FormulaCell {
  row: number;
  col: number;
  formula?: string;
}

export interface PendingInfo {
  requestedGeneration: number;
  requestedAt: string;
  lastAttemptAt?: string;
  attemptCount: number;
  blockedReason?: string;
}

export interface LinkedBomView {
  /** 合成済み。Confirm/Broken 時は前回スナップショット。 */
  doc: BomDoc;
  verdict: StructureVerdict;
  calcState: CalcState;
  syncStatus: SyncStatus;
  env: EnvCheck;
  columnsMeta: LinkColumnMeta[];
  formulaCells: FormulaCell[];
  truncated: boolean;
  warnings: string[];
  pending?: PendingInfo;
}

// ---- 反映 (apply) ----

export type ApplyOutcome =
  | { kind: "applied"; generation: number; fingerprint: string; warnings: string[] }
  | { kind: "pending"; reason: string; warnings: string[] }
  | { kind: "refused"; reason: RefuseReason; warnings: string[] }
  | { kind: "conflict"; backupId: number; backupPath: string; warnings: string[] };

// ---- ステータス・競合 ----

export interface ConflictInfo {
  backupId: number;
  backupPath: string;
  createdAt: string;
  /** false = ワークブック横 (volume_temp) に未移送。 */
  transferred: boolean;
}

export interface LinkStatus {
  syncStatus: SyncStatus;
  syncError?: string;
  calcState: CalcState;
  ecGeneration: number;
  appliedGeneration: number;
  pending?: PendingInfo;
  conflicts: ConflictInfo[];
  untransferred: number;
  recoveryPending: boolean;
  excelLockHint: boolean;
}

// ---- confirm / resolve ----

export interface LinkConfirmRequest {
  structureFp: string;
  accepted: LinkResolutionCandidate[];
}

export type ConflictAction = "resolved";

// ---- UI 補助 ----

export const SYNC_STATUS_LABEL: Record<SyncStatus, string> = {
  linked: "同期中",
  needs_review: "要確認",
  broken: "破綻",
  conflict: "競合",
};

export const CALC_STATE_LABEL: Record<CalcState, string> = {
  unverified: "未検証",
  trusted: "検証済み",
  stale: "未再計算",
  missing: "計算値なし",
};

/** §9-9: stale/missing の値は業務利用しない (EC 取得・カート投入・集計)。 */
export const calcUsable = (c: CalcState): boolean => c === "unverified" || c === "trusted";
