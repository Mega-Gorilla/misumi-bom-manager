// MISUMI cart Phase B — SPIKE: can our own JS obtain the Bearer token in a
// logged-in WebView? Decides B1 (direct cart-detail/add) vs B2 (drive the site UI).
//
// The only open question for Phase B is: `api-jp` authenticates with
// `Authorization: Bearer <JWT>`, and `fetch` does NOT auto-attach it — so our code
// must supply it. This spike answers, in a logged-in page context:
//   1. Is the access token READABLE by our JS? (document.cookie / localStorage /
//      sessionStorage). If GACCESSTOKEN is absent from document.cookie it is HttpOnly.
//   2. DECISIVE: with a readable token candidate, does a real authed call succeed?
//      Uses the READ-ONLY `cart-detail/count` (GET) so the cart is NOT modified.
//   → 200 + count ⇒ B1 viable (build Authorization ourselves).
//   → no readable token / 401 / 403 ⇒ B1 blocked ⇒ use B2 (site's authenticated
//     context). Taxonomy (see docs/misumi-api/09-cart-add.md):
//       B2-a = fetch/XHR intercept: hook the site's own request, capture its
//              Authorization: Bearer, then WE issue cart-detail/add (verify separately).
//       B2-b = UI automation: drive the bulk-input UI (proven by probe-cart-b.mjs);
//              the site attaches the token for us — the reliable fallback.
//
// SECURITY: the token VALUE never leaves the page — evaluate() returns only lengths,
// booleans, key NAMES, and HTTP status. No token is ever written to the log. The
// Authorization header is constructed inside the page and never serialized out.
// Non-persistent context (no ./.edge-profile). Your password is never read.
//
// Usage: node probe-cart-spike.mjs

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const LOG = 'cart-spike.redacted.log';

writeFileSync(LOG, `# MISUMI cart Phase B spike — token reachability (no token values)\n`);

// Defense-in-depth: even though evaluate() is designed to never return token values,
// mask any token-ish string before writing, in case a body echoes one.
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

const browser = await chromium.launch({ channel: 'msedge', headless: false, args: ['--start-maximized', '--new-window'] });
const ctx = await browser.newContext({ locale: 'ja-JP', viewport: null });
const page = await ctx.newPage();

console.log(`\n== Opening ${PAGE}`);
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
line('=== logged in — running token-reachability spike ===');
// Re-load an authed page cleanly.
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(4000);

// ---- Step 1: is the token readable by our JS? (values NEVER returned) ----
const reach = await page.evaluate(() => {
  const TOKEN_COOKIES = ['GACCESSTOKEN', 'GACCESSTOKENKEY', 'GREFRESHTOKENHASH', 'ACCESS_TOKEN_EXPIRATION'];
  const readCookieLen = (name) => {
    const m = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
    return m ? decodeURIComponent(m[1]).length : null; // length only
  };
  const cookieReadable = {};
  for (const n of TOKEN_COOKIES) cookieReadable[n] = readCookieLen(n);

  const tokenishKey = (k) => /token|access|auth|jwt|bearer|session/i.test(k);
  const scanStore = (store) => {
    const out = [];
    for (let i = 0; i < store.length; i++) {
      const k = store.key(i);
      const v = store.getItem(k) || '';
      out.push({ key: k, len: v.length, tokenish: tokenishKey(k), looksJwt: /^eyJ/.test(v) });
    }
    return out;
  };

  return {
    docCookieNames: document.cookie.split(';').map((c) => c.split('=')[0].trim()).filter(Boolean),
    cookieReadableLen: cookieReadable, // null ⇒ NOT readable (HttpOnly or absent)
    localStorage: scanStore(localStorage),
    sessionStorage: scanStore(sessionStorage),
  };
});

line('\n-- Step 1: token reachability (lengths & key names only) --');
line(`document.cookie names (${reach.docCookieNames.length}): ${reach.docCookieNames.join(', ')}`);
line(`token cookies readable via document.cookie (len, null=HttpOnly/absent):`);
for (const [k, v] of Object.entries(reach.cookieReadableLen)) line(`   ${k}: ${v === null ? 'NOT readable' : `len=${v}`}`);
line(`localStorage keys (${reach.localStorage.length}):`);
reach.localStorage.forEach((e) => line(`   ${e.key} (len=${e.len}${e.tokenish ? ', tokenish' : ''}${e.looksJwt ? ', JWT-shaped' : ''})`));
line(`sessionStorage keys (${reach.sessionStorage.length}):`);
reach.sessionStorage.forEach((e) => line(`   ${e.key} (len=${e.len}${e.tokenish ? ', tokenish' : ''}${e.looksJwt ? ', JWT-shaped' : ''})`));

// ---- Step 2: DECISIVE authed read-only call from page context ----
// Try each readable token candidate against cart-detail/count (GET, no cart change).
// The token is built and used INSIDE the page; only {source, status, ok, sample} come back.
const test = await page.evaluate(async () => {
  const readCookie = (name) => {
    const m = document.cookie.match(new RegExp('(?:^|; )' + name + '=([^;]*)'));
    return m ? decodeURIComponent(m[1]) : null;
  };
  const tokenishKey = (k) => /token|access|auth|jwt|bearer/i.test(k);

  // Build candidate tokens from readable sources (value stays in-page).
  const candidates = [];
  const gat = readCookie('GACCESSTOKEN');
  if (gat) candidates.push({ source: 'cookie:GACCESSTOKEN', token: gat });
  for (const store of [['localStorage', localStorage], ['sessionStorage', sessionStorage]]) {
    const [name, s] = store;
    for (let i = 0; i < s.length; i++) {
      const k = s.key(i);
      const v = s.getItem(k) || '';
      if ((tokenishKey(k) || /^eyJ/.test(v)) && v.length > 40) candidates.push({ source: `${name}:${k}`, token: v });
    }
  }

  const url = 'https://api-jp.misumi-ec.com/shopping-cart/v1/cart-detail/count';
  const results = [];
  for (const c of candidates) {
    try {
      const r = await fetch(url, { method: 'GET', headers: { Authorization: 'Bearer ' + c.token } });
      let body = '';
      try {
        body = (await r.text()).slice(0, 120);
      } catch {}
      results.push({ source: c.source, tokenLen: c.token.length, status: r.status, ok: r.ok, sample: body });
    } catch (e) {
      results.push({ source: c.source, tokenLen: c.token.length, status: 'fetch-error', ok: false, sample: String(e).slice(0, 120) });
    }
  }
  return { candidateCount: candidates.length, results };
});

line('\n-- Step 2: authed read-only call (cart-detail/count) per token candidate --');
line(`candidates tried: ${test.candidateCount}`);
if (test.candidateCount === 0) {
  line('   (no readable token candidate found → GACCESSTOKEN is HttpOnly / token in-memory)');
}
for (const r of test.results) {
  line(`   source=${r.source} tokenLen=${r.tokenLen} → status=${r.status} ok=${r.ok} sample=${r.sample}`);
}

// ---- Verdict ----
const b1Works = test.results.some((r) => r.ok === true && /totalCount/i.test(r.sample || ''));
line('\n=== VERDICT ===');
if (b1Works) {
  const win = test.results.find((r) => r.ok === true && /totalCount/i.test(r.sample || ''));
  line(`B1 VIABLE ✅ — token readable from ${win.source}; cart-detail/count returned 200 with totalCount.`);
  line('→ Phase B は cart-detail/add 直叩き(B1)で実装可。Authorization: Bearer を自前構築できる。');
} else {
  line('B1 NOT viable ✗ — 注入 JS からトークンを直接取得できない(HttpOnly + メモリ内保持)。');
  line('→ Phase B は B2(サイト自身の認証済みコンテキストを使う)を採用:');
  line('   B2-a(第一候補): fetch/XHR フックでサイトの Authorization: Bearer を捕捉→我々が cart-detail/add を発行(要検証: probe-cart-hook.mjs)。');
  line('   B2-b(実証済み fallback): 一括入力 UI をプログラム操作(probe-cart-b.mjs)。サイトが Bearer を付与。');
}

line(`\n== ${LOG} written (no token values). Browser stays open 30s.`);
await page.waitForTimeout(30000);
await browser.close();
