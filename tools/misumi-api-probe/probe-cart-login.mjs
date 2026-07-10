// MISUMI cart-add observation (LOGGED IN) — records the endpoints/payloads used by
// the 見積・注文 page's まとめて一括入力 → 次へ → カートへ追加 flow.
//
// PRIVACY: You log in yourself in the opened Edge window. Your ID/password are never
// read by this script. Captured logs are written locally only (cart-observe.log) and
// Cookie / auth headers are MASKED before writing. Do NOT commit the log.
//
// Flow (NO terminal interaction — everything is detected in the browser):
//   1. Opens the 見積・注文 page in a visible Edge window.
//   2. Polls until login is detected — the まとめて一括入力 panel stops showing
//      「先にログインしてください」and the textarea becomes usable. You just log in
//      in the Edge window; the script proceeds on its own.
//   3. Fills the まとめて一括入力 textarea with a few test parts (TSV), clicks 次へ,
//      then カートへ追加 — capturing every misumi-ec.com API call + payload.
//   4. Leaves the browser open so you can inspect / empty the cart manually.
//
// Usage: node probe-cart-login.mjs
// NOTE: no order is placed; test items only land in the cart (delete them after).

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const LOG = 'cart-observe.log';
// Excel-style TSV: 型番 TAB 数量 — small, cheap, in-stock sample parts.
const TSV = 'CBT3-8\t2\nCBT3-10\t3\nSFJ3-10\t1';

writeFileSync(LOG, `# MISUMI cart-add observation ${new Date().toISOString?.() ?? ''}\n`);
const misumi = /^https:\/\/[a-z0-9.-]*misumi-ec\.com\//i;
const asset = /\.(js|css|png|jpe?g|gif|svg|woff2?|ico)(\?|$)|_next\/|akam|pixel|sensor|cameleer|recommend-|log\/add/i;
// Mask cookie/token-ish values so the log is safe to share.
const mask = (s) =>
  (s ?? '')
    .replace(/("(?:cookie|authorization|token|refreshToken|accessToken|sessionId|customerCode|userId)"\s*:\s*)"[^"]*"/gi, '$1"***"')
    .replace(/(Cookie|Authorization|x-[a-z-]*token)[:=]\s*[^\s;]+/gi, '$1: ***');

let recording = false;
const line = (s) => {
  appendFileSync(LOG, s + '\n');
  if (recording) console.log(s);
};

const browser = await chromium.launch({ channel: 'msedge', headless: false });
const ctx = await browser.newContext({ locale: 'ja-JP', permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();

page.on('request', (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (r.method() === 'GET' && !/cart|order\/|quotation|estimate|\/api\//.test(u)) return;
  line(`\n>> ${r.method()} ${u}`);
  const pd = r.postData();
  if (pd) line(`   body: ${mask(pd).slice(0, 1500)}`);
});
page.on('response', async (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!/cart|order\/|quotation|estimate/.test(u) && !/\/api\//.test(u)) return;
  let body = '';
  try {
    body = mask(await r.text()).slice(0, 1500);
  } catch {
    body = '(no body)';
  }
  line(`<< ${r.status()} ${r.request().method()} ${u}\n   ${body}`);
});
page.on('framenavigated', (f) => {
  if (recording && f === page.mainFrame()) line(`\n== navigated: ${f.url()}`);
});

console.log(`\n== Opening ${PAGE}`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(6000);

console.log('\n────────────────────────────────────────────────────────');
console.log(' In the Edge window, LOG IN to your MISUMI account.');
console.log(' (Your credentials are never read by this script.)');
console.log(' The script auto-detects login and continues — no terminal input needed.');
console.log('────────────────────────────────────────────────────────\n');

// Poll for login: the bulk-input panel shows「先にログインしてください」while logged
// out; once logged in that gate text disappears (and the header ログイン button too).
const DEADLINE = 10 * 60 * 1000; // 10 min to log in
const started = performance.now();
let loggedIn = false;
while (performance.now() - started < DEADLINE) {
  // Only trust the signal on jp.misumi-ec.com (login itself happens on the account.*
  // OAuth domain, where neither marker is present → would false-positive).
  const onJp = /^https:\/\/jp\.misumi-ec\.com\//.test(page.url());
  const bodyText = onJp ? await page.locator('body').innerText().catch(() => '') : '';
  const headerLogin = bodyText.includes('ログイン・新規登録');
  if (onJp && bodyText && !headerLogin) {
    loggedIn = true;
    break;
  }
  await page.waitForTimeout(3000);
}
if (!loggedIn) {
  line('!! login not detected within 10 min — aborting');
  console.log('!! login not detected; closing.');
  await browser.close();
  process.exit(1);
}
console.log('== login detected — recording the cart flow now.');
recording = true;
line('=== RECORDING START (logged-in) ===');

// Re-open the create page fresh so we capture the whole flow from a clean state.
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(5000);

// --- Fill the まとめて一括入力 "エクセルから一括コピー" textarea ---
const ta = page.locator('textarea:visible').first();
if ((await ta.count()) === 0) {
  line('!! no visible textarea (bulk-input may still be gated — are you logged in?)');
} else {
  await ta.click();
  await ta.fill(TSV);
  line(`== filled bulk textarea with ${TSV.split('\n').length} rows`);
  // Click 次へ within the bulk-input panel.
  const next = page.locator('button:visible, a:visible').filter({ hasText: /^次へ$/ }).first();
  if ((await next.count()) > 0) {
    line(`== click 次へ`);
    await next.click();
    await page.waitForTimeout(8000);
  } else {
    line('!! 次へ button not found');
  }
}

// --- Click カートへ追加 ---
const addCart = page.locator('button:visible, a:visible').filter({ hasText: /カートへ追加|カートに追加/ }).first();
if ((await addCart.count()) > 0) {
  const disabled = await addCart.isDisabled().catch(() => false);
  line(`== カートへ追加 found (disabled=${disabled})`);
  if (!disabled) {
    await addCart.click();
    await page.waitForTimeout(9000);
    line(`== after カートへ追加, url: ${page.url()}`);
  }
} else {
  line('!! カートへ追加 button not found');
}

line('=== RECORDING END ===');
console.log(`\n== Log written to ${LOG} (Cookie/auth masked). Browser stays open 60s.`);
console.log('== You can inspect the cart and delete the test items manually.');
await page.waitForTimeout(60000);
await browser.close();
