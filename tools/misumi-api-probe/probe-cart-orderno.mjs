// MISUMI cart — CUSTOMER ORDER NUMBER spec probe.
//
// Goal: confirm how お客様注文番号 (customer order number) is represented in the internal
// API when adding to cart. The bulk-input column mapping offers 注文番号1〜3, but our earlier
// cart-detail/add capture had none set — so the JSON field name / shape / max count are
// unconfirmed. This probe fills the bulk input with 型番/数量/お客様注文番号1〜3, maps them,
// clicks カートへ追加, and records EVERY misumi API request/response BODY during the flow so
// we can see where the order numbers land (cart-detail/add? file/parse? a separate call?).
//
// Also dumps the column-mapping <select> options so we learn the exact order-number labels
// and how many slots exist.
//
// PRIVACY: non-persistent (fresh login each run, no profile). Auth values (sessionId / at /
// rt / Bearer / JWT / cookies) are MASKED before writing. The test order numbers below are
// dummy strings (not sensitive). No order is placed — items only land in the cart; delete
// them afterward. Your password is never read by this script.
//
// Usage: node probe-cart-orderno.mjs

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const LOG = 'cart-orderno.redacted.log';
// 型番 TAB 数量 TAB 注文番号1 TAB 注文番号2 TAB 注文番号3 (dummy order numbers).
const TSV = 'CBT3-8\t1\tPO-TEST-A\tPO-TEST-B\tPO-TEST-C';

writeFileSync(LOG, `# MISUMI customer-order-number spec probe (redacted auth)\n`);

const misumi = /^https:\/\/[a-z0-9.-]*misumi-ec\.com\//i;
const asset = /\.(js|css|png|jpe?g|gif|svg|woff2?|ico)(\?|$)|_next\/|akam|pixel|sensor|cameleer|recommend-|log\/add/i;
const mask = (s) =>
  (s ?? '')
    .replace(/sessionId=[A-Za-z0-9_.\-]+/gi, 'sessionId=***')
    .replace(/\b(at|rt)=[A-Za-z0-9_.\-]{12,}/gi, '$1=***')
    .replace(/Bearer\s+[A-Za-z0-9._\-]+/gi, 'Bearer ***')
    .replace(/eyJ[A-Za-z0-9._\-]{20,}/g, '***JWT***')
    .replace(/"sensor_data":"[^"]*"/g, '"sensor_data":"***"')
    .replace(/(Cookie|Authorization|x-[a-z-]*token)[:=]\s*[^\s;]+/gi, '$1: ***');

let recording = false;
const line = (s) => {
  const m = mask(String(s));
  appendFileSync(LOG, m + '\n');
  if (recording) console.log(m);
};

const browser = await chromium.launch({
  channel: 'msedge',
  headless: false,
  args: ['--start-maximized', '--new-window'],
});
const ctx = await browser.newContext({ locale: 'ja-JP', viewport: null });
const page = await ctx.newPage();

// Record request/response bodies for the order/cart/parse endpoints (where order numbers
// might appear). Auth is masked; the payloads themselves are the point.
page.on('request', (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!/cart|order\/|sales-order|price-delivery|quotation|estimate/.test(u)) return;
  const pd = r.postData();
  line(`\n>> ${r.method()} ${u}`);
  if (pd) line(`   body: ${pd.slice(0, 2000)}`);
});
page.on('response', async (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!/cart|order\/|sales-order|price-delivery/.test(u)) return;
  let body = '';
  try {
    body = (await r.text()).slice(0, 2000);
  } catch {
    body = '(no body)';
  }
  line(`<< ${r.status()} ${r.request().method()} ${u}\n   ${body}`);
});

console.log(`\n== Opening ${PAGE}`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(5000);

console.log('\n In the Edge window, LOG IN to your MISUMI account. Auto-detected; no input needed.\n');
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
console.log('== logged in — recording the order-number cart flow.');
recording = true;
line('=== RECORDING START ===');
// The post-login page may still be navigating on its own; a fresh goto can race and
// ERR_ABORTED. Retry once, then proceed (the create page is likely already loaded).
for (let i = 0; i < 2; i++) {
  try {
    await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
    break;
  } catch (e) {
    line(`   (reload race ${i}: ${String(e).slice(0, 80)}; retrying)`);
    await page.waitForTimeout(3000);
  }
}
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
  // 1) fill textarea (5 columns) + データ行 / タブ / 括りなし → 次へ
  const ta = page.locator('textarea:visible').first();
  await ta.click();
  await ta.fill(TSV);
  for (const label of ['データ行', 'タブ(TAB)', 'なし']) {
    await page.getByText(label, { exact: false }).first().click().catch(() => {});
  }
  line(`== filled textarea: ${JSON.stringify(TSV)}`);
  const n1 = await clickByText(/^次へ$/);
  line(`== 次へ #1 -> ${n1}`);
  await page.waitForTimeout(7000);

  // 2) column mapping: dump every select's options, then assign
  //    col0=型番, col1=数量, col2..4 = お客様注文番号(sequential).
  const mapping = await page.evaluate(() => {
    const sels = Array.from(document.querySelectorAll('select')).filter((s) => s.offsetParent !== null);
    const dump = sels.map((s, i) => ({
      i,
      name: s.name || s.getAttribute('aria-label') || '',
      options: Array.from(s.options).map((o) => ({ text: o.text.trim(), value: o.value })),
    }));
    // Only the column-type selects carry a "productCode" option; other dropdowns on the
    // page (e.g. a [1,5,10,20] control) must be skipped. Map them in column order:
    // 型番 / 数量 / お客様注文番号1 / 2 / 3.
    const colSels = sels.filter((s) => Array.from(s.options).some((o) => o.value === 'productCode'));
    const wants = [
      'productCode',
      'qty',
      'customerItemSubReferenceFirst',
      'customerItemSubReferenceSecond',
      'customerItemSubReferenceThird',
    ];
    const out = [];
    colSels.forEach((s, i) => {
      const want = wants[i];
      if (!want) return;
      const target = Array.from(s.options).find((o) => o.value === want);
      if (target) {
        s.value = target.value;
        s.dispatchEvent(new Event('change', { bubbles: true }));
        out.push(`col${i}="${target.text}"(value=${target.value})`);
      }
    });
    return { dump, out };
  });
  line(`\n-- MAPPING UI dump --`);
  mapping.dump.forEach((d) =>
    line(`   select[${d.i}] name="${d.name}" options=${JSON.stringify(d.options)}`),
  );
  line(`== auto-mapped: ${JSON.stringify(mapping.out)}`);
  await page.waitForTimeout(2000);

  // 3) advance 次へ until カートへ追加 enabled, click it (captures cart-detail/add body)
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
console.log(`\n== ${LOG} written (auth redacted). Window stays open 120s.`);
console.log('== If automation stalled, finish 次へ/カートへ追加 by hand — still recording.');
console.log('== Remember to delete the test items from the cart afterward.');
await page.waitForTimeout(120000);
await browser.close();
