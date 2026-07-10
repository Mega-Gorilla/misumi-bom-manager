// MISUMI cart-add — FULL flow observation (persistent login, auto mapping).
//
// Goal: capture the real "add to cart" POST. The まとめて一括入力 flow is:
//   textarea(TSV) → 次へ → 【データ項目の種類を指定: 型番/数量/お客様注文番号】 → 次へ
//   → grid populated + price-checked → カートへ追加 (this POST is what we want).
//
// PRIVACY / LOGIN:
//   - Uses a DEDICATED persistent Edge profile (./.edge-profile), NOT your real Edge
//     profile. You log in ONCE inside the opened window; the session persists across
//     runs in that folder (gitignored). Your password never touches this script.
//   - sessionId / auth tokens are MASKED before anything is written or printed.
//
// This is the same model as the app's production plan: a persistent WebView2 profile the
// user logs into once. No order is placed — items only land in the cart (delete after).
//
// Usage: node probe-cart-b.mjs

import { chromium } from 'playwright';
import { appendFileSync, writeFileSync, mkdirSync } from 'node:fs';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const PROFILE = './.edge-profile';
const LOG = 'cart-b.redacted.log';
const TSV = 'CBT3-8\t2\nCBT3-10\t3\nSFJ3-10\t1';

mkdirSync(PROFILE, { recursive: true });
writeFileSync(LOG, `# MISUMI cart-add full-flow observation (redacted)\n`);

const misumi = /^https:\/\/[a-z0-9.-]*misumi-ec\.com\//i;
const asset = /\.(js|css|png|jpe?g|gif|svg|woff2?|ico)(\?|$)|_next\/|akam|pixel|sensor|cameleer|recommend-|log\/add/i;
// Redact every session/credential-ish value before it is written or shown.
const mask = (s) =>
  (s ?? '')
    .replace(/sessionId=[A-Za-z0-9_.\-]+/gi, 'sessionId=***')
    .replace(/\bat=[A-Za-z0-9_.\-]{12,}/gi, 'at=***')
    .replace(/"sensor_data":"[^"]*"/g, '"sensor_data":"***"')
    .replace(/("(?:cookie|authorization|token|refreshToken|accessToken|sessionId|customerCode|userId|loginId|employeeCode)"\s*:\s*)"[^"]*"/gi, '$1"***"')
    .replace(/(Cookie|Authorization|x-[a-z-]*token)[:=]\s*[^\s;]+/gi, '$1: ***');

let recording = false;
const line = (s) => {
  const m = mask(s);
  appendFileSync(LOG, m + '\n');
  if (recording) console.log(m);
};

const ctx = await chromium.launchPersistentContext(PROFILE, {
  channel: 'msedge',
  headless: false,
  viewport: null,
  locale: 'ja-JP',
  permissions: ['clipboard-read', 'clipboard-write'],
});
const page = ctx.pages()[0] ?? (await ctx.newPage());

ctx.on('request', (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (r.method() === 'GET' && !/cart|order\/|quotation|estimate|\/api\//.test(u)) return;
  line(`\n>> ${r.method()} ${u}`);
  const pd = r.postData();
  if (pd) line(`   body: ${pd.slice(0, 1800)}`);
});
ctx.on('response', async (r) => {
  if (!recording) return;
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!/cart|order\/|quotation|estimate/.test(u) && !/\/api\//.test(u)) return;
  let body = '';
  try {
    body = (await r.text()).slice(0, 1800);
  } catch {
    body = '(no body)';
  }
  line(`<< ${r.status()} ${r.request().method()} ${u}\n   ${body}`);
});

console.log(`\n== Opening ${PAGE} (profile: ${PROFILE})`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(6000);

// --- wait for login (auto-detected; instant if the profile is already logged in) ---
console.log('\n If not logged in, LOG IN in the Edge window. Auto-detected; no terminal input.\n');
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
  await ctx.close();
  process.exit(1);
}
console.log('== logged in — recording the full cart flow.');
recording = true;
line('=== RECORDING START ===');
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(5000);

// helper: dump every select + button so we can see the mapping UI structure
async function dumpForm(tag) {
  const info = await page.evaluate(() => {
    const sels = Array.from(document.querySelectorAll('select')).map((s) => ({
      name: s.name || s.getAttribute('aria-label') || '',
      value: s.value,
      options: Array.from(s.options).map((o) => o.text.trim()),
    }));
    const btns = Array.from(document.querySelectorAll('button, a[role=button]'))
      .filter((b) => b.offsetParent !== null)
      .map((b) => ({ t: (b.innerText || '').replace(/\s+/g, ' ').trim(), disabled: b.disabled }))
      .filter((b) => b.t);
    return { sels, btns };
  });
  line(`\n-- FORM DUMP [${tag}] selects=${info.sels.length}`);
  info.sels.forEach((s, i) => line(`   select[${i}] name="${s.name}" value="${s.value}" options=${JSON.stringify(s.options)}`));
  line(`   buttons=${JSON.stringify(info.btns.map((b) => (b.disabled ? `${b.t}(off)` : b.t)))}`);
  await page.screenshot({ path: `probe-b-${tag}.png`, fullPage: true }).catch(() => {});
}

async function clickByText(re, { needEnabled = true, last = false } = {}) {
  const loc = page.locator('button:visible, a:visible').filter({ hasText: re });
  const n = await loc.count();
  const order = last ? [...Array(n).keys()].reverse() : [...Array(n).keys()];
  for (const i of order) {
    const el = loc.nth(i);
    if (!needEnabled || !(await el.isDisabled().catch(() => false))) {
      const label = (await el.innerText().catch(() => '')).replace(/\s+/g, ' ').trim();
      await el.click({ timeout: 8000 }).catch(() => null);
      return label || true;
    }
  }
  return false;
}

try {
  // 1) fill textarea + normalize options (データ行 / タブ(TAB) / 括り文字なし) then 次へ
  const ta = page.locator('textarea:visible').first();
  await ta.click();
  await ta.fill(TSV);
  for (const label of ['データ行', 'タブ(TAB)', 'なし']) {
    await page.getByText(label, { exact: false }).first().click().catch(() => {});
  }
  line(`== filled textarea (${TSV.split('\n').length} rows), set データ行 / タブ / 括りなし`);
  await dumpForm('01-before-next');
  const n1 = await clickByText(/^次へ$/);
  line(`== clicked 次へ #1 -> ${n1}`);
  await page.waitForTimeout(7000);

  // 2) column-type mapping screen: dump it, then try to set 型番 / 数量 / お客様注文番号
  await dumpForm('02-mapping');
  const mapped = await page.evaluate(() => {
    const want = ['型番', '数量', 'お客様注文番号'];
    const sels = Array.from(document.querySelectorAll('select')).filter((s) => s.offsetParent !== null);
    const out = [];
    let wi = 0;
    for (const s of sels) {
      const opts = Array.from(s.options);
      const target = opts.find((o) => o.text.includes(want[wi]));
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
  line(`== auto-mapped columns: ${JSON.stringify(mapped)}`);
  await page.waitForTimeout(2000);
  await dumpForm('03-after-map');

  // The bulk flow has THREE 次へ steps (parse-confirm → mapping-confirm → preview→grid).
  // Advance through the remaining 次へ until カートへ追加 becomes enabled, then click it —
  // that click issues the add-to-cart POST we want to capture.
  let added = false;
  for (let step = 0; step < 5; step++) {
    const cartBtn = page.locator('button:visible').filter({ hasText: /カートへ追加|カートに追加/ }).first();
    const cartEnabled = (await cartBtn.count()) > 0 && !(await cartBtn.isDisabled().catch(() => true));
    if (cartEnabled) {
      line(`== カートへ追加 ENABLED at step ${step}; clicking to capture add-to-cart POST`);
      await cartBtn.click({ timeout: 8000 }).catch((e) => line(`   click err: ${String(e).slice(0, 120)}`));
      await page.waitForTimeout(12000);
      added = true;
      break;
    }
    const nx = await clickByText(/^次へ$/, { last: true });
    line(`== advance 次へ (step ${step}) -> ${nx}`);
    await page.waitForTimeout(8000);
    await dumpForm(`adv-${step}`);
  }
  line(`== added=${added} url=${page.url()}`);
  await dumpForm('final');
} catch (e) {
  line(`!! automation error: ${String(e).slice(0, 200)}`);
}

line('=== RECORDING END ===');
console.log(`\n== ${LOG} written (redacted). Window stays open 120s.`);
console.log('== If automation stalled, finish the mapping/カートへ追加 by hand — still recording.');
await page.waitForTimeout(120000);
await ctx.close();
