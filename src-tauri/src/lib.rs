// MISUMI BOM Manager - backend
//
// Akamai Bot Manager guards jp.misumi-ec.com, so we cannot call the internal
// price/delivery API from a plain HTTP client. Instead we run a hidden "bridge"
// WebView that has actually loaded jp.misumi-ec.com (a real browser context that
// satisfies Akamai), and execute the fetch chain there via `eval`.
//
// The fetch chain itself lives in shared/misumi-lookup.js (single source of truth,
// also used by the headless CLI in tools/misumi-cli). We inject it here and call
// window.MisumiCore.lookupOne(...).
//
// The bridge returns its result through one of two channels (whichever works):
//   1. Tauri event `mbm-result` (when IPC is injected into the remote page), or
//   2. a `mbm://` navigation that we intercept in `on_navigation` (fallback).
//
// See docs/misumi-api/ for the reverse-engineered API spec.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tauri::{
    AppHandle, Emitter, Listener, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tokio::sync::oneshot;

mod db;
// pub: PR-1 時点では store がプロダクションコード未消費のため、私有 mod だと lib ターゲットで
// dead_code が発火する (CI は clippy -D warnings)。lib クレートの公開 API として宣言する。
// model も store の公開シグネチャが参照するため pub (private_interfaces 警告の回避)。
pub mod excel_link;
pub mod model;
mod spreadsheet;
mod supplier;

use supplier::{provider_for, QuoteItem};

/// SQLite connection (system-of-record), behind a Mutex (rusqlite::Connection is !Sync).
/// DB commands are synchronous so the lock is never held across an `.await`.
struct DbState(std::sync::Mutex<rusqlite::Connection>);

/// Live file watchers for linked BOMs (PR-7), keyed by bom_id. Dropping a handle
/// stops that watch; excel_link_watch(enable) inserts/removes entries.
#[derive(Default)]
struct LinkWatchers(
    std::sync::Mutex<std::collections::HashMap<String, excel_link::watch::WatchHandle>>,
);

/// §0 初期値 500ms — Excel 保存の生イベント発火回数は環境依存のため、debug ビルドの
/// 校正ログ (watch.rs) で実測して調整する。
const WATCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

const BRIDGE_URL: &str = "https://jp.misumi-ec.com/order/part-number/create";
/// MISUMI cart page — shown (in the authenticated bridge) by `misumi_open_cart`.
const CART_URL: &str = "https://jp.misumi-ec.com/order/cart";

/// Single source of truth for the suggest -> price/delivery fetch chain, shared
/// with the headless CLI (tools/misumi-cli). Defines `window.MisumiCore`.
const LOOKUP_CORE_JS: &str = include_str!("../../shared/misumi-lookup.js");

/// Injected at document_start on the bridge (via initialization_script) so it hooks
/// window.fetch/XHR BEFORE the page's own scripts run and captures the site's
/// `Authorization: Bearer` (+ side headers) for cart-detail/add. See
/// shared/misumi-auth-hook.js and docs/misumi-api/09-cart-add.md.
const AUTH_HOOK_JS: &str = include_str!("../../shared/misumi-auth-hook.js");

/// One line to add to the MISUMI cart. `brand_code` is optional — `addToCart` resolves it
/// via suggest when absent (mirrors the price-lookup normalization; usually "MSM1").
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CartItem {
    input_product_code: String,
    qty: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    brand_code: Option<String>,
    /// お客様注文番号 (customerItemSubReference). Single free-text field per line; omitted
    /// when empty. The value passes through to cart-detail/add unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    customer_item_sub_reference: Option<String>,
}

#[derive(Default)]
struct Bridge {
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    counter: AtomicU64,
    ready: AtomicBool,
}

/// Deliver a result coming back from the bridge to the matching pending request.
fn route(bridge: &Bridge, value: Value) {
    let id = value.get("__id").and_then(|v| {
        v.as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| v.as_u64())
    });
    if let Some(id) = id {
        if let Some(tx) = bridge.pending.lock().unwrap().remove(&id) {
            let _ = tx.send(value);
        }
    }
}

/// Build the JS executed inside the bridge (a jp.misumi-ec.com page context):
/// inject the shared core, evaluate `expr` (an async expression resolving to the
/// result payload), and ship it back via IPC event or `mbm://` navigation.
/// A single cold-start retry covers the case where Akamai cookies aren't ready yet.
fn bridge_script(id: u64, expr: &str) -> String {
    format!(
        r#"{core}
(async () => {{
  const id = "{id}";
  async function done(obj) {{
    obj.__id = id;
    try {{
      if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {{
        await window.__TAURI_INTERNALS__.invoke("plugin:event|emit", {{ event: "mbm-result", payload: obj }});
        return;
      }}
    }} catch (e) {{}}
    try {{ window.location.href = "mbm://result?payload=" + encodeURIComponent(JSON.stringify(obj)); }} catch (e) {{}}
  }}
  try {{
    let out;
    try {{ out = await ({expr}); }}
    catch (e1) {{
      // cold start (Akamai cookie not ready yet) -> wait and retry once
      await new Promise(function (r) {{ setTimeout(r, 2500); }});
      out = await ({expr});
    }}
    await done(out);
  }} catch (e) {{
    await done({{ ok: false, error: String((e && e.message) || e) }});
  }}
}})();"#,
        core = LOOKUP_CORE_JS,
        id = id,
        expr = expr
    )
}

/// Single-part lookup script: `MisumiCore.lookupOne(<part>)`.
fn build_lookup_script(id: u64, part_number: &str) -> String {
    let kw = serde_json::to_string(part_number).unwrap_or_else(|_| "\"\"".into());
    bridge_script(id, &format!("window.MisumiCore.lookupOne({kw})"))
}

#[tauri::command]
async fn bridge_ready(app: AppHandle) -> bool {
    app.state::<Arc<Bridge>>().ready.load(Ordering::Relaxed)
}

/// Look up a single part number and return `{ ok, suggest, price }` (or `{ ok:false, error }`).
#[tauri::command]
async fn lookup_part(app: AppHandle, part_number: String) -> Result<Value, String> {
    let part_number = part_number.trim().to_string();
    if part_number.is_empty() {
        return Err("型番を入力してください".into());
    }
    let bridge = app.state::<Arc<Bridge>>().inner().clone();

    // Wait until the bridge webview has had time to load and clear Akamai.
    let mut waited = 0u64;
    while !bridge.ready.load(Ordering::Relaxed) {
        if waited >= 30_000 {
            return Err("ブリッジの初期化がタイムアウトしました".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        waited += 200;
    }

    let id = bridge.counter.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel::<Value>();
    bridge.pending.lock().unwrap().insert(id, tx);

    let webview = app
        .get_webview_window("bridge")
        .ok_or_else(|| "ブリッジWebViewが見つかりません".to_string())?;
    let script = build_lookup_script(id, &part_number);
    if let Err(e) = webview.eval(&script) {
        bridge.pending.lock().unwrap().remove(&id);
        return Err(format!("スクリプト実行に失敗しました: {e}"));
    }

    match tokio::time::timeout(Duration::from_secs(60), rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err("結果の受信に失敗しました".into()),
        Err(_) => {
            bridge.pending.lock().unwrap().remove(&id);
            Err("問い合わせがタイムアウトしました（ネットワークまたはBot対策の可能性）".into())
        }
    }
}

// ---- BOM CRUD (SQLite system-of-record). Excel/CSV import/export is frontend-driven. ----

#[tauri::command]
fn bom_list(db: State<DbState>) -> Result<Vec<model::BomSummary>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_boms(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
fn bom_load(db: State<DbState>, id: String) -> Result<Option<model::BomDoc>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::load_bom(&conn, &id).map_err(|e| e.to_string())
}

#[tauri::command]
fn bom_save(db: State<DbState>, doc: model::BomDoc) -> Result<String, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    db::save_bom(&mut conn, &doc).map_err(|e| e.to_string())
}

/// Meta-only save (name / 数量倍率 / order-no separator) — the only save path a
/// linked BOM may use (§4.8: qtyMultiplier is DB-owned meta; columns/rows are
/// Excel-owned and save_bom refuses linked BOMs).
#[tauri::command]
fn bom_update_meta(db: State<DbState>, id: String, meta: model::BomMeta) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_bom_meta(&conn, &id, &meta).map_err(|e| e.to_string())
}

#[tauri::command]
fn bom_delete(db: State<DbState>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::delete_bom(&conn, &id).map_err(|e| e.to_string())
}

/// Price/delivery history for a (supplier, part number), newest first. Reads the
/// append-only rows written on each fetch (`supplier_price_history`) for the history view.
#[tauri::command]
fn price_history(
    db: State<DbState>,
    supplier: String,
    part_no: String,
    limit: i64,
) -> Result<Vec<model::PriceHistoryEntry>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::price_history(&conn, &supplier, &part_no, limit).map_err(|e| e.to_string())
}

/// Parse an Excel (.xlsx/.xls/.ods) or CSV file (path picked via the dialog plugin) into
/// a flat string grid. The frontend import wizard maps rows/columns onto a BomDoc and
/// persists via `bom_save` — backend stays a thin file<->grid converter.
#[tauri::command]
fn spreadsheet_read(path: String) -> Result<spreadsheet::Workbook, String> {
    spreadsheet::read_workbook(&path)
}

/// Write a flat grid (header row + data rows) to xlsx or CSV (by extension). The frontend
/// builds the grid from the current BomDoc (columns as-displayed, incl. fetched EC values).
#[tauri::command]
fn spreadsheet_write(
    path: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
) -> Result<(), String> {
    spreadsheet::write_grid(&path, &headers, &rows)
}

// ---- Excel link mode (read-only link: probe/create/open/unlink — PR-3) ----

/// Wizard probe: sheets preview + environment verdict, read-only (§2.3).
#[tauri::command]
fn excel_link_probe(path: String) -> Result<model::LinkProbe, String> {
    excel_link::probe(&path)
}

/// Create a link (env check → contract → first open) and return the composed view.
#[tauri::command]
fn excel_link_create(
    db: State<DbState>,
    config: model::LinkCreateConfig,
) -> Result<model::LinkedBomView, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::create_link(&mut conn, &config)
}

/// Read + structure verdict + calc-state restore + compose. Common entry for first
/// display, the manual "更新" button and (PR-7) auto reload.
#[tauri::command]
fn excel_link_open(
    app: AppHandle,
    db: State<DbState>,
    bom_id: String,
) -> Result<model::LinkedBomView, String> {
    let backup_dir = link_backup_dir(&app)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::open_link(&mut conn, &bom_id, &backup_dir)
}

/// App-data directory the §4.2.2 backup transfer targets (shared by open/apply —
/// open needs it too because crash recovery finishes the transfer).
fn link_backup_dir(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("backups");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Unlink: freeze the display cache as a conventional BOM (§1.3).
#[tauri::command]
fn excel_link_unlink(db: State<DbState>, bom_id: String) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::unlink(&mut conn, &bom_id)
}

// ---- Supplier quote (cache-first batch fetch via the bridge) ----

/// Evaluate `script` in the bridge webview and await the matching `__id` result.
/// The pending map / oneshot is never locked across the `.await`.
async fn bridge_fetch(
    app: &AppHandle,
    bridge: &Arc<Bridge>,
    id: u64,
    script: String,
    secs: u64,
) -> Result<Value, String> {
    let (tx, rx) = oneshot::channel::<Value>();
    bridge.pending.lock().unwrap().insert(id, tx);
    let webview = app
        .get_webview_window("bridge")
        .ok_or_else(|| "ブリッジWebViewが見つかりません".to_string())?;
    if let Err(e) = webview.eval(&script) {
        bridge.pending.lock().unwrap().remove(&id);
        return Err(format!("スクリプト実行に失敗しました: {e}"));
    }
    match tokio::time::timeout(Duration::from_secs(secs), rx).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(_)) => Err("結果の受信に失敗しました".into()),
        Err(_) => {
            bridge.pending.lock().unwrap().remove(&id);
            Err("問い合わせがタイムアウトしました（ネットワークまたはBot対策の可能性）".into())
        }
    }
}

/// True if the cached quote was fetched on `today` (local YYYY-MM-DD). Once the
/// calendar day changes, MISUMI price/stock may differ, so a stale entry is re-fetched
/// on the next bulk fetch (same-day entries are served from cache).
fn fetched_today(q: &model::SupplierQuote, today: &str) -> bool {
    match q.fetched_at.as_deref() {
        Some(s) if s.len() >= 10 => &s[..10] == today,
        _ => false,
    }
}

/// Quote `items` from `supplier`. Cache-first (cross-BOM `supplier_cache`), then
/// fetch only the misses in `<= caps.max_batch` chunks, emitting `quote-progress`.
/// Returns one normalized quote per input item (repeats share the cached quote).
/// When `bom_id` names a LINKED BOM, the run is adopted into its per-BOM snapshot
/// and the generation advances iff the snapshot meaningfully changed
/// (implementation.md §2.3); conventional BOMs get `generation: None`.
#[tauri::command]
async fn quote(
    app: AppHandle,
    db: State<'_, DbState>,
    supplier: String,
    items: Vec<QuoteItem>,
    force: bool,
    bom_id: Option<String>,
) -> Result<model::QuoteOutcome, String> {
    let provider =
        provider_for(&supplier).ok_or_else(|| format!("未対応のサプライヤです: {supplier}"))?;
    let caps = provider.caps();

    // Unique, non-empty part numbers (a BOM may repeat a part across rows).
    let mut uniq: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for it in &items {
        let p = it.part_no.trim().to_string();
        if !p.is_empty() && seen.insert(p.clone()) {
            uniq.push(p);
        }
    }

    let mut map: HashMap<String, model::SupplierQuote> = HashMap::new();

    // 1) Cache-first (skip on force). Lock is scoped — never held across an await.
    let mut misses: Vec<String> = Vec::new();
    if force {
        misses = uniq.clone();
    } else {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Same-day freshness: reuse a cache entry only if it was fetched today; entries
        // from a previous day are re-fetched (price/stock may have changed since).
        let today = db::today_local(&conn).map_err(|e| e.to_string())?;
        for p in &uniq {
            match db::cache_get(&conn, &supplier, p).map_err(|e| e.to_string())? {
                Some(q) if fetched_today(&q, &today) => {
                    map.insert(p.clone(), q);
                }
                _ => misses.push(p.clone()),
            }
        }
    }

    let total = misses.len();
    let _ = app.emit(
        "quote-progress",
        serde_json::json!({ "supplier": supplier, "done": 0, "total": total }),
    );

    if total > 0 {
        // Wait for the bridge webview to clear Akamai (same as lookup_part).
        let bridge = app.state::<Arc<Bridge>>().inner().clone();
        let mut waited = 0u64;
        while !bridge.ready.load(Ordering::Relaxed) {
            if waited >= 30_000 {
                return Err("ブリッジの初期化がタイムアウトしました".into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            waited += 200;
        }

        let mut done = 0usize;
        for chunk in misses.chunks(caps.max_batch.max(1)) {
            let parts: Vec<String> = chunk.to_vec();
            let id = bridge.counter.fetch_add(1, Ordering::Relaxed);
            let script = bridge_script(id, &provider.fetch_call_js(&parts));
            // lookupMany(N) does N suggests + 1 price POST, so allow a longer deadline.
            let raw = bridge_fetch(&app, &bridge, id, script, 120).await?;
            let mut quotes = provider.normalize(&parts, &raw);

            // Stamp one consistent fetched_at, then write cache (lock scoped, no await).
            {
                let conn = db.0.lock().map_err(|e| e.to_string())?;
                let now = db::now_string(&conn).map_err(|e| e.to_string())?;
                for q in &mut quotes {
                    q.fetched_at = Some(now.clone());
                }
                for (p, q) in parts.iter().zip(quotes.iter()) {
                    db::cache_put(&conn, &supplier, p, q).map_err(|e| e.to_string())?;
                }
            }
            for (p, q) in parts.into_iter().zip(quotes) {
                map.insert(p, q);
            }
            done += chunk.len();
            let _ = app.emit(
                "quote-progress",
                serde_json::json!({ "supplier": supplier, "done": done, "total": total }),
            );
        }
    }

    // 2) Adopt into the linked BOM's snapshot + advance the generation (§2.3).
    //    Third lock scope, after every await: the snapshot UPSERTs and the
    //    generation bump commit in ONE transaction inside adopt_quotes.
    let generation = if let Some(bid) = &bom_id {
        let pairs: Vec<(String, model::SupplierQuote)> = uniq
            .iter()
            .filter_map(|p| map.get(p).map(|q| (p.clone(), q.clone())))
            .collect();
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        excel_link::adopt_quotes(&mut conn, bid, &pairs)?
    } else {
        None
    };

    // 3) Align results to the input items (repeated parts share their quote).
    let failed: Vec<model::QuoteFailure> = uniq
        .iter()
        .filter_map(|p| {
            let q = map.get(p)?;
            (q.status == "error").then(|| model::QuoteFailure {
                parts_no: p.clone(),
                message: q.errors.join("; "),
            })
        })
        .collect();
    let results = items
        .iter()
        .map(|it| {
            let p = it.part_no.trim();
            map.get(p).cloned().unwrap_or_else(|| model::SupplierQuote {
                supplier_code: supplier.clone(),
                status: "error".to_string(),
                product: None,
                quote: None,
                errors: vec!["型番が空です".to_string()],
                warnings: vec![],
                fetched_at: None,
                raw: None,
            })
        })
        .collect();
    Ok(model::QuoteOutcome {
        results,
        generation,
        failed,
    })
}

/// 「Excel へ反映」 = §4.2.2 steps 1-9 (write-back with backup + post-hoc conflict
/// detection). Manual retry uses the same command (implementation.md §2.3).
#[tauri::command]
fn excel_link_apply(
    app: AppHandle,
    db: State<DbState>,
    bom_id: String,
) -> Result<model::ApplyOutcome, String> {
    let backup_dir = link_backup_dir(&app)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::apply_link(&mut conn, &bom_id, &backup_dir)
}

/// Lightweight link status (§2.3): DB + one file-existence probe, poll-friendly.
#[tauri::command]
fn excel_link_status(db: State<DbState>, bom_id: String) -> Result<model::LinkStatus, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::status(&conn, &bom_id)
}

/// Confirm-verdict resolution (§1.3 候補確定 → linked). Returns the refreshed view.
#[tauri::command]
fn excel_link_confirm(
    app: AppHandle,
    db: State<DbState>,
    bom_id: String,
    resolution: model::LinkConfirmRequest,
) -> Result<model::LinkedBomView, String> {
    let backup_dir = link_backup_dir(&app)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::confirm_link(&mut conn, &bom_id, &resolution, &backup_dir)
}

/// Watch toggle (PR-7, §4.2.3): raw filesystem events are judged in Rust
/// (parent-dir watch, debounce, `~$` filter) and only synthesized events reach
/// the frontend — `excel-link:changed` (re-open) and `excel-link:excel-closed`
/// (retry a pending apply, §9-5). Watching is a convenience trigger only;
/// correctness stays with read-time verification and the pre-write fingerprint
/// check.
#[tauri::command]
fn excel_link_watch(
    app: AppHandle,
    db: State<DbState>,
    watchers: State<LinkWatchers>,
    bom_id: String,
    enable: bool,
) -> Result<(), String> {
    let mut map = watchers.0.lock().map_err(|e| e.to_string())?;
    map.remove(&bom_id); // toggle/replace: any previous watch stops first
    if !enable {
        return Ok(());
    }
    let (dir, name) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        excel_link::watch::watch_target(&conn, &bom_id)?
    };
    let emitter = app.clone();
    let id = bom_id.clone();
    let handle = excel_link::watch::start_watch(&dir, &name, WATCH_DEBOUNCE, move |ev| {
        let event = match ev {
            excel_link::watch::WatchEvent::Changed => "excel-link:changed",
            excel_link::watch::WatchEvent::ExcelClosed => "excel-link:excel-closed",
        };
        let _ = emitter.emit(event, serde_json::json!({ "bomId": id }));
    })?;
    map.insert(bom_id, handle);
    Ok(())
}

/// Full contract remap — the Broken-repair path (PR-6). Keeps state/generation/
/// EC snapshots, unlike unlink+create.
#[tauri::command]
fn excel_link_remap(
    app: AppHandle,
    db: State<DbState>,
    bom_id: String,
    request: model::LinkRemapRequest,
) -> Result<model::LinkedBomView, String> {
    let backup_dir = link_backup_dir(&app)?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::remap_link(&mut conn, &bom_id, &request, &backup_dir)
}

/// Record the user's conflict resolution (§1.3: lifts the conflict stop). Opening
/// the backup folder is the frontend's job (PR-6, opener plugin).
#[tauri::command]
fn excel_link_resolve_conflict(
    db: State<DbState>,
    bom_id: String,
    backup_id: i64,
    action: model::ConflictAction,
) -> Result<(), String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    excel_link::resolve_conflict(&mut conn, &bom_id, backup_id, action)
}

// ---- Cart (MISUMI) — add BOM rows to the logged-in cart via the bridge ----
//
// The cart requires login. The bridge's auth hook (AUTH_HOOK_JS) captures the site's
// `Authorization: Bearer` from its own api-jp calls; `MisumiCore.addToCart` reuses it to
// POST cart-detail/add. The Bearer never leaves the page — we only receive the result.

/// Wait until the bridge webview has cleared Akamai (shared by lookup/quote/cart).
async fn wait_bridge_ready(bridge: &Arc<Bridge>) -> Result<(), String> {
    let mut waited = 0u64;
    while !bridge.ready.load(Ordering::Relaxed) {
        if waited >= 30_000 {
            return Err("ブリッジの初期化がタイムアウトしました".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        waited += 200;
    }
    Ok(())
}

/// Add `items` to the supplier's cart (currently MISUMI). Returns the JS result object
/// `{ ok, result }` on success or `{ ok:false, error }` (e.g. "NOT_LOGGED_IN"/"AUTH_EXPIRED").
#[tauri::command]
async fn cart_add(app: AppHandle, supplier: String, items: Vec<CartItem>) -> Result<Value, String> {
    if !supplier.eq_ignore_ascii_case("MISUMI") {
        return Err(format!("カート投入は未対応のECです: {supplier}"));
    }
    if items.is_empty() {
        return Err("カートに追加する行がありません".into());
    }
    let bridge = app.state::<Arc<Bridge>>().inner().clone();
    wait_bridge_ready(&bridge).await?;
    let items_json = serde_json::to_string(&items).map_err(|e| e.to_string())?;
    let id = bridge.counter.fetch_add(1, Ordering::Relaxed);
    let script = bridge_script(id, &format!("window.MisumiCore.addToCart({items_json})"));
    bridge_fetch(&app, &bridge, id, script, 120).await
}

/// Report MISUMI login/auth state: `{ ok, loggedIn, captured }`. `captured` means a Bearer
/// has been intercepted and cart-detail/add is callable.
#[tauri::command]
async fn misumi_auth_status(app: AppHandle) -> Result<Value, String> {
    let bridge = app.state::<Arc<Bridge>>().inner().clone();
    if !bridge.ready.load(Ordering::Relaxed) {
        return Ok(serde_json::json!({ "ok": true, "loggedIn": false, "captured": false }));
    }
    let id = bridge.counter.fetch_add(1, Ordering::Relaxed);
    let script = bridge_script(id, "window.MisumiCore.authStatus()");
    bridge_fetch(&app, &bridge, id, script, 15).await
}

/// Show the bridge webview so the user can log in to MISUMI, then poll until a Bearer is
/// captured (= logged in) or ~5 min elapses, and hide it again. Returns `{ loggedIn }`.
/// Login happens entirely in the WebView — this app never sees the password.
#[tauri::command]
async fn misumi_login(app: AppHandle) -> Result<Value, String> {
    let webview = app
        .get_webview_window("bridge")
        .ok_or_else(|| "ブリッジWebViewが見つかりません".to_string())?;
    let _ = webview.set_title("MISUMI ログイン");
    let _ = webview.show();
    let _ = webview.set_focus();

    let bridge = app.state::<Arc<Bridge>>().inner().clone();
    let start = std::time::Instant::now();
    let deadline = Duration::from_secs(300);
    let mut captured = false;
    let mut nudged = false;
    while start.elapsed() < deadline {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        // Only probe on jp.misumi-ec.com — never eval into the SSO/login pages mid-flow.
        let on_jp = webview
            .url()
            .map(|u| u.host_str() == Some("jp.misumi-ec.com"))
            .unwrap_or(false);
        if !on_jp {
            continue;
        }
        let id = bridge.counter.fetch_add(1, Ordering::Relaxed);
        let script = bridge_script(id, "window.MisumiCore.authStatus()");
        if let Ok(v) = bridge_fetch(&app, &bridge, id, script, 8).await {
            if v.get("captured").and_then(Value::as_bool).unwrap_or(false) {
                captured = true;
                break;
            }
            // Logged in but no Bearer captured yet (landed on a page that made no api-jp
            // call). Nudge the bridge to the order page once — it reliably fires an
            // authenticated api-jp request, which the hook captures.
            let logged_in = v.get("loggedIn").and_then(Value::as_bool).unwrap_or(false);
            if logged_in && !nudged {
                let _ = webview.eval(format!("window.location.href={:?};", BRIDGE_URL));
                nudged = true;
            }
        }
    }
    let _ = webview.hide();
    let _ = webview.set_title("misumi-bridge");
    Ok(serde_json::json!({ "loggedIn": captured }))
}

/// Show the (authenticated) bridge navigated to the MISUMI order/cart page so the user can
/// review what was added. The cart lives in THIS WebView's session, so the default browser
/// (logged out) would show an empty cart — hence we surface it in the bridge.
#[tauri::command]
async fn misumi_open_cart(app: AppHandle) -> Result<(), String> {
    let webview = app
        .get_webview_window("bridge")
        .ok_or_else(|| "ブリッジWebViewが見つかりません".to_string())?;
    let _ = webview.set_title("MISUMI カート");
    let _ = webview.eval(format!("window.location.href={:?};", CART_URL));
    let _ = webview.show();
    let _ = webview.set_focus();
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // SQLite data layer (system-of-record).
            let db_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&db_dir)?;
            let conn = db::open(&db_dir.join("misumi-bom.db"))?;
            app.manage(DbState(std::sync::Mutex::new(conn)));
            app.manage(LinkWatchers::default());

            let bridge = Arc::new(Bridge::default());
            app.manage(bridge.clone());

            // Channel 1: Tauri event from the remote page (when IPC is available).
            let ev_bridge = bridge.clone();
            app.listen("mbm-result", move |event| {
                if let Ok(value) = serde_json::from_str::<Value>(event.payload()) {
                    route(&ev_bridge, value);
                }
            });

            // Build the hidden bridge webview: inject the auth-capture hook at document_start,
            // and intercept the mbm:// fallback.
            let nav_bridge = bridge.clone();
            let bridge_win = WebviewWindowBuilder::new(
                app.handle(),
                "bridge",
                WebviewUrl::External(BRIDGE_URL.parse().expect("valid bridge url")),
            )
            .title("misumi-bridge")
            .visible(false)
            .initialization_script(AUTH_HOOK_JS)
            .on_navigation(move |url| {
                if url.scheme() == "mbm" {
                    let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
                    if let Some(payload) = pairs.get("payload") {
                        if let Ok(value) = serde_json::from_str::<Value>(payload) {
                            route(&nav_bridge, value);
                        }
                    }
                    return false;
                }
                true
            })
            .build()?;

            // The bridge doubles as the login / cart-view window when shown. Closing it must
            // HIDE (not destroy) it, or the price-lookup + cart engine would die.
            let hide_win = bridge_win.clone();
            bridge_win.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = hide_win.hide();
                }
            });

            // Mark ready after the page has had time to load and satisfy Akamai.
            let ready_bridge = bridge.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                ready_bridge.ready.store(true, Ordering::Relaxed);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            lookup_part,
            bridge_ready,
            bom_list,
            bom_load,
            bom_save,
            bom_delete,
            price_history,
            spreadsheet_read,
            spreadsheet_write,
            quote,
            cart_add,
            misumi_auth_status,
            misumi_login,
            misumi_open_cart,
            excel_link_probe,
            excel_link_create,
            excel_link_open,
            excel_link_unlink,
            excel_link_apply,
            excel_link_status,
            excel_link_confirm,
            excel_link_resolve_conflict,
            excel_link_remap,
            excel_link_watch,
            bom_update_meta
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
