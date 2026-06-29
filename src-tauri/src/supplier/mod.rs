// Supplier abstraction (provider dispatch). Phase 1 implements MISUMI only;
// the trait is forward-looking for future EC sites. See docs/plans/0003-bom-editor/plan.md §7.
//
// Transport split: webview providers (MISUMI, behind Akamai) supply the JS to run
// in the bridge WebView via `fetch_call_js`, plus a pure `normalize` mapping that
// turns the raw provider payload into the canonical `SupplierQuote`. The webview
// transport orchestration (chunking, progress, cache) lives in lib.rs `quote`.

use crate::model::SupplierQuote;
use serde::Deserialize;
use serde_json::Value;

mod misumi;

/// One requested line for `quote` (camelCase JSON from the frontend).
///
/// The (supplier, parts_no) cache is **qty-agnostic** (representative qty=1), so the
/// quote request carries no qty. The frontend computes subtotal and the per-row MOQ
/// check from the row's Qty locally. A future qty-aware "confirm" fetch (plan §11,
/// "確定は再取得で qty 指定取得") would add qty on a separate, cache-bypassing path.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteItem {
    pub part_no: String,
}

/// Provider capabilities (batch limits, transport, currency). Most fields are part
/// of the forward-looking abstraction (future http-api EC dispatch); Phase 1 reads
/// only `max_batch`.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SupplierCaps {
    pub batch: bool,
    pub max_batch: usize,
    pub needs_auth: bool,
    /// "webview" | "http-api"
    pub transport: &'static str,
    pub currency: &'static str,
}

/// A supplier provider: metadata + (webview) fetch script + pure normalization.
pub trait SupplierProvider: Send + Sync {
    /// Supplier code (e.g. "MISUMI"). Reserved for multi-provider dispatch/logging.
    #[allow(dead_code)]
    fn code(&self) -> &'static str;
    fn caps(&self) -> SupplierCaps;

    /// Async JS expression (resolving to the raw payload) evaluated in the bridge
    /// WebView after `shared/misumi-lookup.js` is injected. webview transport only.
    fn fetch_call_js(&self, parts: &[String]) -> String;

    /// Pure mapping: provider raw payload -> one normalized quote per requested part,
    /// aligned to `parts`. `fetched_at` is left None (lib.rs stamps it at cache write).
    fn normalize(&self, parts: &[String], raw: &Value) -> Vec<SupplierQuote>;
}

/// Resolve a provider by supplier code (e.g. "MISUMI"). Future EC sites register here.
pub fn provider_for(code: &str) -> Option<Box<dyn SupplierProvider>> {
    match code {
        "MISUMI" => Some(Box::new(misumi::Misumi)),
        _ => None,
    }
}
