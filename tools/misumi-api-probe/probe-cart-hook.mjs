// MISUMI cart Phase B — B2-a VERIFICATION: does a document_start fetch/XHR hook
// capture the site's `Authorization: Bearer` (+ required side headers), and can we
// REUSE them ourselves to hit api-jp?
//
// probe-cart-spike.mjs proved B1 (read the token from cookie/storage) is impossible
// (tokens are HttpOnly + in-memory). B2-a's plan is: hook window.fetch/XHR BEFORE the
// page's own scripts run, let the site issue its normal api-jp calls (on load), capture
// the headers it sends (Authorization + x-client-program + x-language-code), then WE call
// cart-detail/add with that set. This probe verifies the load-bearing steps:
//   (1) CAPTURE — the hook sees an api-jp request carrying `Authorization: Bearer` and
//                 records its side headers (x-client-program / x-language-code).
//   (2) REUSE   — using the captured Bearer + side headers, OUR own fetch to the
//                 READ-ONLY `cart-detail/count` returns 200. (Cart is NOT modified.)
// captured + reuse-200 ⇒ B2-a is viable. (A first run with Authorization ALONE returned
// 400 "Client Program is null." — i.e. the token was ACCEPTED; only x-client-program was
// missing. So we now capture and replay that header too.)
//
// SECURITY: the Bearer VALUE never leaves the page (stashed on window.__mbmBearer, used
// only in-page). evaluate() returns ONLY booleans, lengths, scheme, URL, HTTP status, and
// the NON-secret side headers (x-client-program is a static client id, x-language-code a
// locale — neither is a credential). The log also masks Bearer/JWT. Non-persistent context.
//
// Usage: node probe-cart-hook.mjs

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const LOG = 'cart-hook.redacted.log';

writeFileSync(LOG, `# MISUMI cart Phase B — B2-a hook capture/reuse verification (no token values)\n`);

const mask = (s) =>
  (s ?? '')
    .replace(/sessionId=[A-Za-z0-9_.\-]+/gi, 'sessionId=***')
    .replace(/\b(at|rt)=[A-Za-z0-9_.\-]{12,}/gi, '$1=***')
    .replace(/Bearer\s+[A-Za-z0-9._\-]+/gi, 'Bearer ***')
    .replace(/eyJ[A-Za-z0-9._\-]{20,}/g, '***JWT***');

const line = (s) => {
  const m = mask(String(s));
  appendFileSync(LOG, m + '\n');
  console.log(m);
};

const browser = await chromium.launch({
  channel: 'msedge',
  headless: false,
  args: ['--start-maximized', '--new-window'],
});
const ctx = await browser.newContext({ locale: 'ja-JP', viewport: null });

// Hook injected at document_start on EVERY navigation (before page scripts run). Records
// metadata about the first api-jp request bearing Authorization, stashes the raw token on
// window.__mbmBearer (in-page only), and keeps the NON-secret side headers separately.
await ctx.addInitScript(() => {
  if (window.__mbmHookInstalled) return;
  window.__mbmHookInstalled = true;
  window.__mbm = { captured: false, scheme: null, len: 0, url: null, via: null, sideHeaders: {} };

  const isApiJp = (u) => /^https?:\/\/api-jp\.misumi-ec\.com\//.test(u || '');
  // Only these non-secret side headers are surfaced (never Authorization/cookies).
  const SIDE = ['x-client-program', 'x-language-code', 'content-type', 'idempotency-key'];
  const capture = (url, headerMap, via) => {
    if (window.__mbm.captured || !isApiJp(url)) return;
    const auth = headerMap['authorization'];
    if (!auth) return;
    const m = /^(\S+)\s+(.+)$/.exec(auth);
    const side = {};
    for (const k of SIDE) if (headerMap[k] != null) side[k] = headerMap[k];
    window.__mbm = { captured: true, scheme: m ? m[1] : '(raw)', len: auth.length, url: String(url), via, sideHeaders: side };
    window.__mbmBearer = m ? m[2] : auth; // secret — stays in page
  };

  const toMap = (headers) => {
    const out = {};
    if (!headers) return out;
    if (typeof Headers !== 'undefined' && headers instanceof Headers) headers.forEach((v, k) => (out[k.toLowerCase()] = v));
    else if (Array.isArray(headers)) headers.forEach((p) => (out[String(p[0]).toLowerCase()] = p[1]));
    else for (const k in headers) out[k.toLowerCase()] = headers[k];
    return out;
  };

  const origFetch = window.fetch;
  window.fetch = function (input, init) {
    try {
      const url = typeof input === 'string' ? input : (input && input.url) || '';
      const map = toMap(init && init.headers);
      if (!map['authorization'] && input && typeof input === 'object' && input.headers && typeof input.headers.forEach === 'function') {
        input.headers.forEach((v, k) => (map[k.toLowerCase()] = v));
      }
      capture(url, map, 'fetch');
    } catch (e) {}
    return origFetch.apply(this, arguments);
  };

  const oOpen = XMLHttpRequest.prototype.open;
  const oSet = XMLHttpRequest.prototype.setRequestHeader;
  const oSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__mbmUrl = url;
    this.__mbmHdrs = {};
    return oOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.setRequestHeader = function (k, v) {
    try {
      (this.__mbmHdrs || (this.__mbmHdrs = {}))[String(k).toLowerCase()] = v;
    } catch (e) {}
    return oSet.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    try {
      capture(this.__mbmUrl, this.__mbmHdrs || {}, 'xhr');
    } catch (e) {}
    return oSend.apply(this, arguments);
  };
});

const page = await ctx.newPage();

console.log(`\n== Opening ${PAGE} (hook installed at document_start)`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(5000);

console.log('\n────────────────────────────────────────────────────────');
console.log(' In the Edge window, LOG IN to your MISUMI account.');
console.log(' (Your credentials are never read by this script.)');
console.log(' Auto-detected; no terminal input needed.');
console.log('────────────────────────────────────────────────────────\n');

const DEADLINE = 10 * 60 * 1000;
const t0 = performance.now();
let loggedIn = false;
while (performance.now() - t0 < DEADLINE) {
  const onJp = /^https:\/\/jp\.misumi-ec\.com\//.test(page.url());
  const body = onJp ? await page.locator('body').innerText().catch(() => '') : '';
  if (onJp && body && !body.includes('ログイン・新規登録')) {
    loggedIn = true;
    break;
  }
  await page.waitForTimeout(3000);
}
if (!loggedIn) {
  line('!! login not detected within 10 min — aborting');
  await browser.close();
  process.exit(1);
}
line('=== logged in — reloading with hook active to capture the site request headers ===');

await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
let cap = { captured: false };
for (let i = 0; i < 15; i++) {
  await page.waitForTimeout(2000);
  cap = await page.evaluate(() => window.__mbm || { captured: false });
  if (cap.captured) break;
}

line('\n-- Step 1: hook CAPTURE (token value NOT shown; side headers are non-secret) --');
if (cap.captured) {
  line(`captured ✅ via=${cap.via} scheme=${cap.scheme} bearerLen=${cap.len} url=${cap.url}`);
  line(`side headers: ${JSON.stringify(cap.sideHeaders)}`);
} else {
  line('captured ✗ — no api-jp request with Authorization seen within ~30s.');
}

// ---- Step 2: REUSE captured Bearer + side headers on read-only cart-detail/count ----
let reuse = { ran: false };
if (cap.captured) {
  reuse = await page.evaluate(async () => {
    const token = window.__mbmBearer;
    const side = (window.__mbm && window.__mbm.sideHeaders) || {};
    if (!token) return { ran: true, ok: false, status: 'no-token', sample: '' };
    const headers = { Authorization: 'Bearer ' + token };
    for (const k of ['x-client-program', 'x-language-code']) if (side[k]) headers[k] = side[k];
    try {
      const r = await fetch('https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/count', { method: 'GET', headers });
      let body = '';
      try {
        body = (await r.text()).slice(0, 160);
      } catch {}
      return { ran: true, ok: r.ok, status: r.status, sample: body, sentHeaders: Object.keys(headers) };
    } catch (e) {
      return { ran: true, ok: false, status: 'fetch-error', sample: String(e).slice(0, 160) };
    }
  });
  line('\n-- Step 2: REUSE Bearer + side headers on cart-detail/count (read-only) --');
  line(`sent headers: ${JSON.stringify(reuse.sentHeaders || [])}`);
  line(`reuse: status=${reuse.status} ok=${reuse.ok} sample=${reuse.sample}`);
}

// ---- Verdict ----
const b2aWorks = cap.captured && reuse.ran && reuse.ok === true && /totalCount/i.test(reuse.sample || '');
line('\n=== VERDICT ===');
if (b2aWorks) {
  line('B2-a VIABLE ✅ — hook captured Authorization + side headers AND our reuse call returned 200.');
  line(`→ Phase B は B2-a で実装できる。cart-detail/add に付ける必須ヘッダ: Authorization(捕捉) + ${Object.keys(cap.sideHeaders).join(' / ')}`);
} else if (cap.captured) {
  line('B2-a PARTIAL — captured但し reuse が 200 でない。');
  line(`   status=${reuse.status} sample=${reuse.sample}`);
  line('→ 不足ヘッダを追加して再検証、または B2-b(UI 自動操作)を fallback に。');
} else {
  line('B2-a NOT confirmed — hook did not capture an Authorization from api-jp.');
  line('→ B2-b(UI 自動操作, probe-cart-b.mjs で実証済み)を採用。');
}

line(`\n== ${LOG} written (no token values). Browser stays open 30s.`);
await page.waitForTimeout(30000);
await browser.close();
