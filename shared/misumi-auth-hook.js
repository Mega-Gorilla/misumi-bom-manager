// MISUMI auth-header capture hook (document_start init script).
//
// api-jp.misumi-ec.com authenticates with `Authorization: Bearer <JWT>` (NOT cookies),
// and the token is HttpOnly + held only in the SPA's memory — our injected code cannot
// read it directly (verified: docs/misumi-api/09-cart-add.md). So instead we hook
// window.fetch / XMLHttpRequest BEFORE the page's own scripts run and capture the
// `Authorization` header (plus the required side headers x-client-program / x-language-code)
// from the site's own api-jp requests, then reuse them to POST cart-detail/add ourselves.
//
// Injected as a Tauri `initialization_script` on the bridge WebView, so it runs at
// document_start on every navigation (before page JS). It is a transparent passthrough:
// it only observes headers, never blocks or mutates requests, and never throws.
//
// The captured Bearer stays ONLY on window.__mbmAuth (in-page). It is never emitted to
// Rust, logged, or persisted. MisumiCore.addToCart (shared/misumi-lookup.js) reads it.
//
// Mirrors tools/misumi-api-probe/probe-cart-hook.mjs (which proved the capture+reuse works).
(function () {
  if (window.__mbmAuthHookInstalled) return;
  window.__mbmAuthHookInstalled = true;
  // captured: have we seen an Authorization on an api-jp request?
  // bearer: the raw token (in-page only). headers: the non-secret side headers to replay.
  window.__mbmAuth = { captured: false, bearer: null, headers: {} };

  var isApiJp = function (u) {
    return /^https?:\/\/api-jp\.misumi-ec\.com\//.test(u || "");
  };
  // Side headers that api-jp requires alongside Authorization (non-secret).
  var SIDE = ["x-client-program", "x-language-code"];

  // Record the latest api-jp Authorization seen (kept fresh as the SPA rotates its token).
  var capture = function (url, headerMap) {
    if (!isApiJp(url)) return;
    var auth = headerMap["authorization"];
    if (!auth) return;
    var m = /^(\S+)\s+(.+)$/.exec(auth);
    var side = {};
    for (var i = 0; i < SIDE.length; i++) {
      if (headerMap[SIDE[i]] != null) side[SIDE[i]] = headerMap[SIDE[i]];
    }
    window.__mbmAuth = { captured: true, bearer: m ? m[2] : auth, headers: side };
  };

  var toMap = function (headers) {
    var out = {};
    try {
      if (!headers) return out;
      if (typeof Headers !== "undefined" && headers instanceof Headers) {
        headers.forEach(function (v, k) {
          out[String(k).toLowerCase()] = v;
        });
      } else if (Array.isArray(headers)) {
        headers.forEach(function (p) {
          out[String(p[0]).toLowerCase()] = p[1];
        });
      } else {
        for (var k in headers) out[String(k).toLowerCase()] = headers[k];
      }
    } catch (e) {}
    return out;
  };

  // --- fetch ---
  var origFetch = window.fetch;
  if (typeof origFetch === "function") {
    window.fetch = function (input, init) {
      try {
        var url = typeof input === "string" ? input : (input && input.url) || "";
        var map = toMap(init && init.headers);
        if (
          !map["authorization"] &&
          input &&
          typeof input === "object" &&
          input.headers &&
          typeof input.headers.forEach === "function"
        ) {
          input.headers.forEach(function (v, k) {
            map[String(k).toLowerCase()] = v;
          });
        }
        capture(url, map);
      } catch (e) {}
      return origFetch.apply(this, arguments);
    };
  }

  // --- XMLHttpRequest ---
  try {
    var oOpen = XMLHttpRequest.prototype.open;
    var oSet = XMLHttpRequest.prototype.setRequestHeader;
    var oSend = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.open = function (method, url) {
      this.__mbmUrl = url;
      this.__mbmHdrs = {};
      return oOpen.apply(this, arguments);
    };
    XMLHttpRequest.prototype.setRequestHeader = function (k, v) {
      try {
        if (!this.__mbmHdrs) this.__mbmHdrs = {};
        this.__mbmHdrs[String(k).toLowerCase()] = v;
      } catch (e) {}
      return oSet.apply(this, arguments);
    };
    XMLHttpRequest.prototype.send = function () {
      try {
        capture(this.__mbmUrl, this.__mbmHdrs || {});
      } catch (e) {}
      return oSend.apply(this, arguments);
    };
  } catch (e) {}
})();
