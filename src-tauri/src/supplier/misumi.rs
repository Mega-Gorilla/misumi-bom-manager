// MISUMI provider (transport: webview). Uses the shared core's `lookupMany`
// (suggest -> sales-price-delivery/check) and normalizes the response to the
// canonical SupplierQuote per docs/plans/0003-bom-editor/plan.md §7.5 and the
// measured response in docs/misumi-api/03-price-delivery-check.md.

use super::{SupplierCaps, SupplierProvider};
use crate::model::{SupplierPricing, SupplierProduct, SupplierQuote};
use serde_json::Value;

pub struct Misumi;

const CODE: &str = "MISUMI";

impl SupplierProvider for Misumi {
    fn code(&self) -> &'static str {
        CODE
    }

    fn caps(&self) -> SupplierCaps {
        SupplierCaps {
            batch: true,
            max_batch: 100, // docs/misumi-api/07-batch-and-limits.md
            needs_auth: false,
            transport: "webview",
            currency: "JPY",
        }
    }

    fn fetch_call_js(&self, parts: &[String]) -> String {
        // Evaluated after shared/misumi-lookup.js is injected (defines MisumiCore).
        let arr = serde_json::to_string(parts).unwrap_or_else(|_| "[]".into());
        format!("window.MisumiCore.lookupMany({arr})")
    }

    fn normalize(&self, parts: &[String], raw: &Value) -> Vec<SupplierQuote> {
        let price = &raw["price"];
        let currency = price
            .get("ccyCode")
            .and_then(Value::as_str)
            .unwrap_or("JPY")
            .to_string();
        let details = price.get("detailList").and_then(Value::as_array);
        let suggests = raw.get("suggests").and_then(Value::as_array);

        // Request-wide messages, optionally attributed to a line by `lineNumber`.
        let top_errors = messages(price.get("errorMessageList"));
        let top_warnings = messages(price.get("warningMessageList"));

        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let suggest = suggests.and_then(|a| a.get(i)).filter(|v| !v.is_null());
                let detail = find_detail(details, i, suggest, part);

                let mut errors: Vec<String> = Vec::new();
                let mut warnings: Vec<String> = Vec::new();

                // Attribute request-wide messages. A `lineNumber`-less ERROR is request-
                // wide (auth/rate-limit/bad request) and applies to every row -> push to
                // errors so each row is marked status:error, never silently ok.
                let line = (i + 1) as i64;
                for (ln, msg) in &top_errors {
                    match ln {
                        Some(l) if *l == line => errors.push(msg.clone()),
                        None => errors.push(msg.clone()),
                        _ => {}
                    }
                }
                for (ln, msg) in &top_warnings {
                    if ln.is_none() || *ln == Some(line) {
                        warnings.push(msg.clone());
                    }
                }

                let Some(detail) = detail else {
                    // No matching line (unknown/incomplete part number).
                    errors.insert(0, format!("型番が見つかりません: {part}"));
                    return SupplierQuote {
                        supplier_code: CODE.to_string(),
                        status: "error".to_string(),
                        product: None,
                        quote: None,
                        errors,
                        warnings,
                        fetched_at: None,
                        raw: None,
                    };
                };

                // Per-line messages.
                for (_, m) in messages(detail.get("errorMessageList")) {
                    errors.push(m);
                }
                for (_, m) in messages(detail.get("warningMessageList")) {
                    warnings.push(m);
                }
                for (_, m) in messages(detail.get("infoMessageList")) {
                    warnings.push(m);
                }

                let product = detail.get("product");
                let sales = detail.get("salesPrice");
                let lead = detail.get("leadTime");
                let trade = detail.get("trade");

                let prod = SupplierProduct {
                    name: sget(product, "productName")
                        .or_else(|| suggest.and_then(|s| sget(Some(s), "seriesName"))),
                    brand: sget(product, "brandName"),
                    part_no: sget(product, "inputProductCode").or_else(|| Some(part.clone())),
                    category: sget(product, "productCategoryCode"),
                };

                let quote = SupplierPricing {
                    currency: Some(currency.clone()),
                    unit_price: sget(sales, "salesUnitPrice"),
                    unit_price_tax: sget(sales, "salesUnitPriceIncludingTax"),
                    tax_rate: sget(sales, "taxRate"),
                    ship_date: sget(lead, "vsd"),
                    lead_time_days: iget(lead, "actualShippingDays"),
                    stock: iget(trade, "immediateShippableQty"),
                    moq: iget(product, "minSoQty"),
                    pack_qty: iget(product, "soUnitQty"),
                    subtotal: None, // qty-agnostic cache; frontend computes unitPrice*qty*multiplier
                };

                let status = if errors.is_empty() { "ok" } else { "error" };
                SupplierQuote {
                    supplier_code: CODE.to_string(),
                    status: status.to_string(),
                    product: Some(prod),
                    quote: Some(quote),
                    errors,
                    warnings,
                    fetched_at: None,
                    raw: Some(detail.clone()),
                }
            })
            .collect()
    }
}

/// Find the price detail for request line `i` (0-based): prefer `lineNumber == i+1`,
/// fall back to matching `product.inputProductCode` against the suggest/part number.
fn find_detail<'a>(
    details: Option<&'a Vec<Value>>,
    i: usize,
    suggest: Option<&Value>,
    part: &str,
) -> Option<&'a Value> {
    let details = details?;
    let line = (i + 1) as i64;
    if let Some(d) = details
        .iter()
        .find(|d| d.get("lineNumber").and_then(Value::as_i64) == Some(line))
    {
        return Some(d);
    }
    let want = suggest
        .and_then(|s| sget(Some(s), "partNumber"))
        .unwrap_or_else(|| part.to_string());
    details
        .iter()
        .find(|d| sget(d.get("product"), "inputProductCode").as_deref() == Some(want.as_str()))
        .or_else(|| details.get(i))
}

/// Extract `(lineNumber, message)` pairs from a `[{code, message, lineNumber?}]` list.
fn messages(v: Option<&Value>) -> Vec<(Option<i64>, String)> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| {
                    let msg = m.get("message").and_then(Value::as_str)?;
                    let ln = m.get("lineNumber").and_then(Value::as_i64);
                    Some((ln, msg.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Get a string field from `obj[key]` (string as-is, number stringified).
fn sget(obj: Option<&Value>, key: &str) -> Option<String> {
    match obj?.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Get an integer field from `obj[key]` (number or numeric string).
fn iget(obj: Option<&Value>, key: &str) -> Option<i64> {
    match obj?.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The measured CBT3-8 response (docs/misumi-api/03), wrapped as lookupMany output.
    fn cbt3_8_raw() -> Value {
        json!({
            "ok": true,
            "parts": ["CBT3-8"],
            "suggests": [{ "partNumber": "CBT3-8", "brandCode": "MSM1", "seriesName": "六角穴付アルミボルト" }],
            "price": {
                "ccyCode": "JPY",
                "errorMessageList": [],
                "warningMessageList": [],
                "infoMessageList": [],
                "detailList": [{
                    "lineNumber": 1,
                    "qty": 1,
                    "product": {
                        "inputProductCode": "CBT3-8", "brandCode": "MSM1", "brandName": "ミスミ",
                        "productName": "ﾛｯｶｸｱﾅﾂｷｱﾙﾐﾎﾞﾙﾄ ﾁﾀﾝﾎﾞﾙﾄ", "soUnitQty": 1, "minSoQty": 1
                    },
                    "salesPrice": {
                        "salesUnitPrice": "400", "salesUnitPriceIncludingTax": "440", "taxRate": "10.00"
                    },
                    "leadTime": { "vsd": "2026-06-26", "actualShippingDays": 1 },
                    "trade": { "immediateShippableQty": 851 }
                }]
            }
        })
    }

    #[test]
    fn normalize_maps_core_fields() {
        let q = &Misumi.normalize(&["CBT3-8".to_string()], &cbt3_8_raw())[0];
        assert_eq!(q.status, "ok");
        assert_eq!(q.supplier_code, "MISUMI");
        let p = q.quote.as_ref().unwrap();
        assert_eq!(p.unit_price.as_deref(), Some("400"));
        assert_eq!(p.unit_price_tax.as_deref(), Some("440"));
        assert_eq!(p.ship_date.as_deref(), Some("2026-06-26"));
        assert_eq!(p.stock, Some(851));
        assert_eq!(p.moq, Some(1));
        assert_eq!(p.currency.as_deref(), Some("JPY"));
        assert_eq!(q.product.as_ref().unwrap().brand.as_deref(), Some("ミスミ"));
        assert!(q.errors.is_empty());
        assert!(q.raw.is_some());
    }

    #[test]
    fn normalize_marks_missing_part_as_error() {
        // suggest null + empty detailList => unknown part number.
        let raw = json!({
            "ok": true, "parts": ["NOPE-1"], "suggests": [null],
            "price": { "ccyCode": "JPY", "detailList": [] }
        });
        let q = &Misumi.normalize(&["NOPE-1".to_string()], &raw)[0];
        assert_eq!(q.status, "error");
        assert!(q.quote.is_none());
        assert!(q.errors[0].contains("NOPE-1"));
    }

    #[test]
    fn normalize_surfaces_per_line_moq_message() {
        // E-GBSCB4-20 style: price returns but a per-line message warns about min order qty.
        let raw = json!({
            "ok": true, "parts": ["E-GBSCB4-20"],
            "suggests": [{ "partNumber": "E-GBSCB4-20", "brandCode": "MSM1" }],
            "price": {
                "ccyCode": "JPY",
                "detailList": [{
                    "lineNumber": 1,
                    "product": { "inputProductCode": "E-GBSCB4-20", "minSoQty": 200 },
                    "salesPrice": { "salesUnitPrice": "14" },
                    "warningMessageList": [{ "code": "X", "message": "200個から注文可" }]
                }]
            }
        });
        let q = &Misumi.normalize(&["E-GBSCB4-20".to_string()], &raw)[0];
        assert_eq!(q.quote.as_ref().unwrap().moq, Some(200));
        assert!(q.warnings.iter().any(|w| w.contains("200")));
    }

    #[test]
    fn normalize_request_wide_error_marks_all_rows_error() {
        // A top-level error WITHOUT lineNumber is request-wide; every row must be error,
        // even if a price detail came back (regression for PR #7 review).
        let raw = json!({
            "ok": true, "parts": ["CBT3-8"],
            "suggests": [{ "partNumber": "CBT3-8", "brandCode": "MSM1" }],
            "price": {
                "ccyCode": "JPY",
                "errorMessageList": [{ "code": "E_AUTH", "message": "リクエストが拒否されました" }],
                "detailList": [{
                    "lineNumber": 1,
                    "product": { "inputProductCode": "CBT3-8" },
                    "salesPrice": { "salesUnitPrice": "400" }
                }]
            }
        });
        let q = &Misumi.normalize(&["CBT3-8".to_string()], &raw)[0];
        assert_eq!(q.status, "error");
        assert!(q.errors.iter().any(|e| e.contains("拒否")));
    }
}
