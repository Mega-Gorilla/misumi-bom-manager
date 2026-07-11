// MISUMI cart-add — AUTH MECHANISM capture (Phase B の下準備).
//
// The ONE open question before Phase B (直接 cart-detail/add を叩く全自動) is:
//   「api-jp 系(cart-detail/add 等)へ、認証情報を Cookie で渡すのか / Authorization
//    ヘッダ(Bearer)か / sessionId クエリか」
// The existing probes record request/response *bodies* only — never headers — so the
// mechanism is still unconfirmed. This probe fills exactly that gap.
//
// WHAT IT CAPTURES (and how it stays safe to share):
//   - Request header KEY LIST (names are not secret) for each api / cart / auth call.
//   - Cookie header → only the COOKIE NAMES (e.g. sessionId, AKA_*), NEVER the values,
//     plus total length. Lets us see which cookie carries the session.
//   - Authorization header → only the scheme (e.g. "Bearer") + length, value masked.
//   - Response CORS headers: access-control-allow-origin / -credentials / vary, and
//     set-cookie reduced to name + flags (HttpOnly/Secure/SameSite) — value masked.
//   - Whether each request is cross-origin (page=jp.* → api-jp.* is a different origin).
//
// NON-PERSISTENT: a fresh context each run (no ./.edge-profile). You log in in the opened
// Edge window; the session is discarded on close. Sidesteps the profile-folder DENY-ACE.
// Your password is never read by this script. No order is placed (test items only land in
// the cart — delete them after).
//
// Usage: node probe-cart-auth.mjs

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const PAGE_HOST = 'jp.misumi-ec.com';
const LOG = 'cart-auth.redacted.log';
// One cheap in-stock part is enough to reach カートへ追加 and capture its headers.
const TSV = 'CBT3-8\t1';

writeFileSync(LOG, `# MISUMI cart-add AUTH mechanism capture (redacted headers)\n`);

const misumi = /^https:\/\/[a-z0-9.-]*misumi-ec\.com\//i;
const asset = /\.(js|css|png|jpe?g|gif|svg|woff2?|ico)(\?|$)|_next\/|akam|pixel|sensor|cameleer|recommend-|log\/add/i;
// Endpoints where the auth mechanism actually matters.
const authRelevant = /cart|order\/|quotation|estimate|\/api\/|auth|sales-order|price-delivery|shopping-cart/i;

// Body/URL masker (session tokens never hit the log).
const mask = (s) =>
  (s ?? '')
    .replace(/sessionId=[A-Za-z0-9_.\-]+/gi, 'sessionId=***')
    .replace(/\bat=[A-Za-z0-9_.\-]{12,}/gi, 'at=***')
    // SSO token-refresh body carries at=(access) & rt=(refresh) — both are credentials.
    .replace(/\brt=[A-Za-z0-9_.\-]{12,}/gi, 'rt=***')
    .replace(/"sensor_data":"[^"]*"/g, '"sensor_data":"***"')
    .replace(/("(?:cookie|authorization|token|refreshToken|accessToken|sessionId|customerCode|userId|loginId|employeeCode)"\s*:\s*)"[^"]*"/gi, '$1"***"')
    .replace(/(Cookie|Authorization|x-[a-z-]*token)[:=]\s*[^\s;]+/gi, '$1: ***');

let recording = false;
const line = (s) => {
  const m = mask(s);
  appendFileSync(LOG, m + '\n');
  if (recording) console.log(m);
};

// Summarize REQUEST headers without leaking any secret value.
function reqHeaderSummary(host, headers) {
  const keys = Object.keys(headers).map((k) => k.toLowerCase()).sort();
  const parts = [];
  const cookie = headers['cookie'] ?? headers['Cookie'];
  if (cookie != null) {
    const names = cookie
      .split(';')
      .map((c) => c.split('=')[0].trim())
      .filter(Boolean);
    parts.push(`cookie{names=[${names.join(', ')}], len=${cookie.length}}`);
  }
  const auth = headers['authorization'] ?? headers['Authorization'];
  if (auth != null) {
    const scheme = auth.split(/\s+/)[0] || '(none)';
    parts.push(`authorization{scheme=${scheme}, len=${auth.length}}`);
  }
  // Any custom auth-ish headers (names only).
  const custom = keys.filter((k) => /token|session|auth|credential|x-.*-id\b/.test(k) && k !== 'authorization');
  if (custom.length) parts.push(`custom=[${custom.join(', ')}]`);
  return {
    crossOrigin: host !== PAGE_HOST,
    keyList: keys,
    summary: parts.length ? parts.join('  ') : '(no cookie / no authorization header)',
  };
}

// Summarize RESPONSE headers (CORS + set-cookie flags), values masked.
function respHeaderSummary(headers) {
  const out = {};
  for (const [k, v] of Object.entries(headers)) {
    const lk = k.toLowerCase();
    if (/^access-control-/.test(lk) || lk === 'vary') out[lk] = v;
    else if (lk === 'set-cookie') {
      // Multiple cookies may be joined by "\n"; mask EACH value — never leak any.
      out['set-cookie'] = v
        .split('\n')
        .map((c) => {
          const name = c.split('=')[0].trim();
          const flags = (c.match(/(HttpOnly|Secure|SameSite=\w+|Path=[^;]+|Domain=[^;]+)/gi) || []).join(' ');
          return `${name}=*** ${flags}`.trim();
        })
        .join(' | ');
    }
  }
  return out;
}

const browser = await chromium.launch({ channel: 'msedge', headless: false });
const ctx = await browser.newContext({ locale: 'ja-JP', permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();

page.on('request', async (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!authRelevant.test(u)) return;
  let host = '';
  try {
    host = new URL(u).host;
  } catch {}
  line(`\n>> ${r.method()} ${u}`);
  try {
    const h = await r.allHeaders();
    const info = reqHeaderSummary(host, h);
    line(`   [req-auth] crossOrigin=${info.crossOrigin}  ${info.summary}`);
    line(`   [req-keys] ${info.keyList.join(', ')}`);
  } catch (e) {
    line(`   [req-auth] (headers unavailable: ${String(e).slice(0, 80)})`);
  }
  const pd = r.postData();
  if (pd) line(`   body: ${pd.slice(0, 1200)}`);
});
page.on('response', async (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!authRelevant.test(u)) return;
  let respInfo = {};
  try {
    respInfo = respHeaderSummary(await r.allHeaders());
  } catch {}
  let body = '';
  try {
    body = (await r.text()).slice(0, 1200);
  } catch {
    body = '(no body)';
  }
  line(`<< ${r.status()} ${r.request().method()} ${u}`);
  if (Object.keys(respInfo).length) line(`   [resp-cors] ${JSON.stringify(respInfo)}`);
  line(`   ${body}`);
});

console.log(`\n== Opening ${PAGE}`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(6000);

console.log('\n────────────────────────────────────────────────────────');
console.log(' In the Edge window, LOG IN to your MISUMI account.');
console.log(' (Your credentials are never read by this script.)');
console.log(' The script auto-detects login and continues — no terminal input needed.');
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
  console.log('!! login not detected; closing.');
  await browser.close();
  process.exit(1);
}
console.log('== logged in — recording the cart flow (headers included).');
recording = true;
line('=== RECORDING START (logged-in) ===');
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(5000);

async function clickByText(re, { last = false } = {}) {
  const loc = page.locator('button:visible, a:visible').filter({ hasText: re });
  const n = await loc.count();
  const order = last ? [...Array(n).keys()].reverse() : [...Array(n).keys()];
  for (const i of order) {
    const el = loc.nth(i);
    if (!(await el.isDisabled().catch(() => false))) {
      const label = (await el.innerText().catch(() => '')).replace(/\s+/g, ' ').trim();
      await el.click({ timeout: 8000 }).catch(() => null);
      return label || true;
    }
  }
  return false;
}

try {
  // 1) fill textarea (データ行 / タブ / 括りなし) → 次へ
  const ta = page.locator('textarea:visible').first();
  await ta.click();
  await ta.fill(TSV);
  for (const label of ['データ行', 'タブ(TAB)', 'なし']) {
    await page.getByText(label, { exact: false }).first().click().catch(() => {});
  }
  line(`== filled textarea (${TSV.split('\n').length} row)`);
  const n1 = await clickByText(/^次へ$/);
  line(`== 次へ #1 -> ${n1}`);
  await page.waitForTimeout(7000);

  // 2) column mapping: 型番 / 数量 / お客様注文番号
  const mapped = await page.evaluate(() => {
    const want = ['型番', '数量', 'お客様注文番号'];
    const sels = Array.from(document.querySelectorAll('select')).filter((s) => s.offsetParent !== null);
    const out = [];
    let wi = 0;
    for (const s of sels) {
      const target = Array.from(s.options).find((o) => o.text.includes(want[wi]));
      if (target) {
        s.value = target.value;
        s.dispatchEvent(new Event('change', { bubbles: true }));
        out.push(`${want[wi]}=${target.text}`);
        wi++;
      }
      if (wi >= want.length) break;
    }
    return out;
  });
  line(`== auto-mapped: ${JSON.stringify(mapped)}`);
  await page.waitForTimeout(2000);

  // 3) advance 次へ until カートへ追加 enabled, then click → captures cart-detail/add headers
  let added = false;
  for (let step = 0; step < 5; step++) {
    const cartBtn = page.locator('button:visible').filter({ hasText: /カートへ追加|カートに追加/ }).first();
    const cartEnabled = (await cartBtn.count()) > 0 && !(await cartBtn.isDisabled().catch(() => true));
    if (cartEnabled) {
      line(`== カートへ追加 ENABLED at step ${step}; clicking`);
      await cartBtn.click({ timeout: 8000 }).catch((e) => line(`   click err: ${String(e).slice(0, 120)}`));
      await page.waitForTimeout(12000);
      added = true;
      break;
    }
    const nx = await clickByText(/^次へ$/, { last: true });
    line(`== 次へ advance (step ${step}) -> ${nx}`);
    await page.waitForTimeout(8000);
  }
  line(`== added=${added} url=${page.url()}`);
} catch (e) {
  line(`!! automation error: ${String(e).slice(0, 200)}`);
}

line('=== RECORDING END ===');
console.log(`\n== ${LOG} written (headers redacted). Window stays open 120s.`);
console.log('== If automation stalled, finish 次へ/カートへ追加 by hand — still recording.');
console.log('== Remember to delete the test item from the cart afterward.');
await page.waitForTimeout(120000);
await browser.close();
