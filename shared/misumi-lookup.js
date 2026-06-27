// Single source of truth for the MISUMI part-number -> price/delivery fetch chain.
//
// This runs INSIDE a jp.misumi-ec.com page context (a real browser that has
// satisfied Akamai Bot Manager). It is used by BOTH:
//   - the Tauri bridge WebView (src-tauri/src/lib.rs via include_str! + eval), and
//   - the headless CLI (tools/misumi-cli via Playwright page.evaluate).
//
// Keep it dependency-free and side-effect-light: it only defines window.MisumiCore.
// See docs/misumi-api/ for the reverse-engineered spec.
(function () {
  var APP_ID = "de30e2b2-db86-435d-9929-646c11a3c4cd";
  var SUGGEST = "https://jp.misumi-ec.com/api/v1/partNumber/suggest";
  var PRICE =
    "https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check";

  async function getJson(url, opts) {
    var res = await fetch(url, opts);
    if (!res.ok) throw new Error("HTTP " + res.status + " @ " + url);
    return await res.json();
  }

  // 型番正規化: keyword -> { partNumber, brandCode, seriesCode, ... } | null
  async function suggest(keyword) {
    var url =
      SUGGEST +
      "?applicationId=" +
      APP_ID +
      "&keyword=" +
      encodeURIComponent(String(keyword).toLowerCase()) +
      "&field=%40default%2CpartNumberList.checkCFlag";
    // same-origin (jp.misumi-ec.com); credentials are harmless here
    var j = await getJson(url, { credentials: "include" });
    return (j.partNumberList || [])[0] || null;
  }

  // 単価・出荷日: detailList = [{ qty, inputProductCode, brandCode }] -> response JSON
  //
  // IMPORTANT: api-jp returns `Access-Control-Allow-Origin: *`, which is
  // incompatible with credentialed requests. The request MUST be credential-less
  // (default/omit) or the browser blocks it with "Failed to fetch". The standard
  // (non-logged-in) price needs no cookies anyway.
  async function priceDelivery(detailList) {
    return await getJson(PRICE, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ detailList: detailList }),
    });
  }

  // 単一型番: suggest -> price
  async function lookupOne(partNumber) {
    var first = await suggest(partNumber);
    if (!first) {
      return { ok: false, partNumber: partNumber, error: "型番が見つかりません: " + partNumber };
    }
    var price = await priceDelivery([
      { qty: 1, inputProductCode: first.partNumber, brandCode: first.brandCode },
    ]);
    return { ok: true, partNumber: partNumber, suggest: first, price: price };
  }

  // 複数型番（バッチ）: 各型番を suggest で正規化し、1リクエストで価格・出荷日を取得
  async function lookupMany(parts) {
    var suggests = await Promise.all(
      parts.map(function (p) {
        return suggest(p).catch(function () {
          return null;
        });
      })
    );
    var detailList = parts.map(function (p, i) {
      var s = suggests[i];
      return {
        qty: 1,
        inputProductCode: s ? s.partNumber : p,
        brandCode: s ? s.brandCode : "MSM1",
      };
    });
    var price = await priceDelivery(detailList);
    return { ok: true, parts: parts, suggests: suggests, price: price };
  }

  window.MisumiCore = {
    APP_ID: APP_ID,
    suggest: suggest,
    priceDelivery: priceDelivery,
    lookupOne: lookupOne,
    lookupMany: lookupMany,
  };
})();
