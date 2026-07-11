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
  var CART_ADD = "https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/add";

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

  // 認証状態: window.__mbmAuth（shared/misumi-auth-hook.js が document_start で捕捉）を読む。
  // loggedIn は DOM のログイン導線の不在、captured は cart-detail/add に使える Bearer 捕捉済みか。
  function authStatus() {
    var a = window.__mbmAuth || {};
    var txt = "";
    try {
      txt = (document.body && document.body.innerText) || "";
    } catch (e) {}
    var loggedIn = txt.indexOf("ログイン・新規登録") < 0;
    return { ok: true, loggedIn: loggedIn, captured: !!(a.captured && a.bearer) };
  }

  // カート投入: items = [{ inputProductCode, qty, brandCode? }]
  // フックが捕捉した Authorization: Bearer + 付随ヘッダを再利用して cart-detail/add を発行する
  // （トークンは HttpOnly + メモリ内保持のため直接取得できない。09-cart-add.md 参照）。
  // 注文ではなくカート投入（あとで削除可能）。ブランドコード未指定は suggest で正規化する。
  async function addToCart(items) {
    var a = window.__mbmAuth;
    if (!a || !a.captured || !a.bearer) {
      return { ok: false, error: "NOT_LOGGED_IN" };
    }
    // brandCode / 正規 partNumber を suggest で解決（lookupMany と同型。多くは MSM1）。
    var list = await Promise.all(
      (items || []).map(async function (it) {
        var code = it.inputProductCode;
        var brand = it.brandCode;
        if (!brand) {
          var s = await suggest(code).catch(function () {
            return null;
          });
          if (s) {
            code = s.partNumber || code;
            brand = s.brandCode || "MSM1";
          } else {
            brand = "MSM1";
          }
        }
        return { qty: it.qty, brandCode: brand, inputProductCode: code };
      })
    );
    var idem =
      (window.crypto && window.crypto.randomUUID && window.crypto.randomUUID()) ||
      "mbm-" + Date.now() + "-" + Math.floor(Math.random() * 1e9);
    var res = await fetch(CART_ADD, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Authorization: "Bearer " + a.bearer,
        "x-client-program": a.headers["x-client-program"] || "JP_ORDER",
        "x-language-code": a.headers["x-language-code"] || "JPN",
        "idempotency-key": idem,
      },
      body: JSON.stringify({
        cartDetailList: list,
        indirectSalesOrderInstrumentationFlag: "1",
      }),
    });
    var text = "";
    try {
      text = await res.text();
    } catch (e) {}
    var json = null;
    try {
      json = JSON.parse(text);
    } catch (e) {}
    if (!res.ok) {
      // 401/403 はトークン失効 → 呼び出し側で再ログインを促す
      if (res.status === 401 || res.status === 403) {
        return { ok: false, error: "AUTH_EXPIRED", status: res.status };
      }
      return {
        ok: false,
        status: res.status,
        error: (json && json.message) || "HTTP " + res.status,
      };
    }
    return { ok: true, result: json };
  }

  window.MisumiCore = {
    APP_ID: APP_ID,
    suggest: suggest,
    priceDelivery: priceDelivery,
    lookupOne: lookupOne,
    lookupMany: lookupMany,
    authStatus: authStatus,
    addToCart: addToCart,
  };
})();
