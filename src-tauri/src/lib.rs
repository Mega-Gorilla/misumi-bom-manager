// MISUMI BOM Manager - backend
//
// Akamai Bot Manager guards jp.misumi-ec.com, so we cannot call the internal
// price/delivery API from a plain HTTP client. Instead we run a hidden "bridge"
// WebView that has actually loaded jp.misumi-ec.com (a real browser context that
// satisfies Akamai), and execute the fetch chain there via `eval`.
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
use tauri::{AppHandle, Listener, Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::sync::oneshot;

const APP_ID: &str = "de30e2b2-db86-435d-9929-646c11a3c4cd";
const BRIDGE_URL: &str = "https://jp.misumi-ec.com/order/part-number/create";

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
/// suggest (型番正規化 → brandCode) → sales-price-delivery/check (単価・出荷日).
fn build_lookup_script(id: u64, part_number: &str) -> String {
    let kw = serde_json::to_string(part_number).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"(async () => {{
  const id = "{id}";
  const kw = {kw};
  const APP_ID = "{app_id}";
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
  async function getJson(url, opts) {{
    const res = await fetch(url, opts);
    if (!res.ok) throw new Error("HTTP " + res.status);
    return await res.json();
  }}
  async function run() {{
    const sUrl = "https://jp.misumi-ec.com/api/v1/partNumber/suggest?applicationId=" + APP_ID
      + "&keyword=" + encodeURIComponent(kw.toLowerCase())
      + "&field=%40default%2CpartNumberList.checkCFlag";
    const sjson = await getJson(sUrl, {{ credentials: "include" }});
    const first = (sjson.partNumberList || [])[0];
    if (!first) return {{ ok: false, error: "型番が見つかりません: " + kw }};
    const body = {{ detailList: [{{ qty: 1, inputProductCode: first.partNumber, brandCode: first.brandCode }}] }};
    const pjson = await getJson("https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check", {{
      method: "POST",
      headers: {{ "Content-Type": "application/json" }},
      credentials: "include",
      body: JSON.stringify(body)
    }});
    return {{ ok: true, suggest: first, price: pjson }};
  }}
  try {{
    let out;
    try {{ out = await run(); }} catch (e1) {{
      // cold start (Akamai cookie not ready yet) -> wait and retry once
      await new Promise(function (r) {{ setTimeout(r, 2500); }});
      out = await run();
    }}
    await done(out);
  }} catch (e) {{
    await done({{ ok: false, error: String((e && e.message) || e) }});
  }}
}})();"#,
        id = id,
        kw = kw,
        app_id = APP_ID
    )
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let bridge = Arc::new(Bridge::default());
            app.manage(bridge.clone());

            // Channel 1: Tauri event from the remote page (when IPC is available).
            let ev_bridge = bridge.clone();
            app.listen("mbm-result", move |event| {
                if let Ok(value) = serde_json::from_str::<Value>(event.payload()) {
                    route(&ev_bridge, value);
                }
            });

            // Build the hidden bridge webview and intercept the mbm:// fallback.
            let nav_bridge = bridge.clone();
            WebviewWindowBuilder::new(
                app.handle(),
                "bridge",
                WebviewUrl::External(BRIDGE_URL.parse().expect("valid bridge url")),
            )
            .title("misumi-bridge")
            .visible(false)
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

            // Mark ready after the page has had time to load and satisfy Akamai.
            let ready_bridge = bridge.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(5)).await;
                ready_bridge.ready.store(true, Ordering::Relaxed);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![lookup_part, bridge_ready])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
