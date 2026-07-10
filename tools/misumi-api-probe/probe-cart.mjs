// MISUMI cart-add probe (reproducible).
//
// Goal: discover how the 型番手入力 page (order/part-number/create) adds a part
// to the cart — the endpoint, payload, and whether login is required (guest
// cart?). Same technique as probe.mjs: real Edge so Akamai's JS can run.
//
// Usage:
//   node probe-cart.mjs CBT3-8      # type part no, click カートに追加, capture network
//
// This only touches the CART (no order is placed). Run without login to see
// the anonymous behavior; log in manually in the opened window to see the
// authenticated behavior.

import { chromium } from 'playwright';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const PART = process.argv[2] || 'CBT3-8';

const browser = await chromium.launch({ channel: 'msedge', headless: false });
const ctx = await browser.newContext({ locale: 'ja-JP' });
const page = await ctx.newPage();

// ---- network capture: only misumi-ec.com API-ish traffic (3rd-party trackers are noise) ----
const interesting = /cart|order\/|quotation|estimate|checkout|auth|login/i;
const misumi = /^https:\/\/[a-z0-9.-]*misumi-ec\.com\//i;
const asset = /\.(js|css|png|jpe?g|gif|svg|woff2?|ico|json)(\?|$)|_next\/|akam|pixel|sensor|cameleer|recommend-/i;

page.on('request', (r) => {
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (r.method() !== 'GET' || interesting.test(u) || /\/api\//.test(u)) {
    console.log(`\n>> ${r.method()} ${u}`);
    const pd = r.postData();
    if (pd) console.log(`   body: ${pd.slice(0, 800)}`);
  }
});
page.on('response', async (r) => {
  const u = r.url();
  if (!misumi.test(u) || asset.test(u)) return;
  if (!interesting.test(u) && !/\/api\//.test(u)) return;
  let body = '';
  try {
    body = (await r.text()).slice(0, 1200);
  } catch {
    body = '(no body)';
  }
  console.log(`\n<< ${r.status()} ${r.request().method()} ${u}\n   ${body}`);
});
page.on('framenavigated', (f) => {
  if (f === page.mainFrame()) console.log(`\n== navigated: ${f.url()}`);
});

console.log(`== open ${PAGE}`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(7000); // Akamai _abck warm-up

// ---- fill the part-number input ----
// The create page is a Next.js grid; find the first visible textbox.
const inputs = page.locator('input:visible');
const n = await inputs.count();
console.log(`== visible inputs: ${n}`);
for (let i = 0; i < Math.min(n, 8); i++) {
  const el = inputs.nth(i);
  console.log(
    `   [${i}] type=${await el.getAttribute('type')} placeholder=${await el.getAttribute('placeholder')} name=${await el.getAttribute('name')} aria=${await el.getAttribute('aria-label')}`,
  );
}

// The grid's part-number cells are input[name=partNumber]; the header search box is NOT
// (its placeholder merely mentions 型番 — do not press Enter there, it navigates away).
const target = page.locator('input[name="partNumber"]:visible').first();

console.log(`== type part number: ${PART}`);
await target.click();
await target.fill(PART);
await page.keyboard.press('Tab'); // blur triggers validation, Enter would submit/navigate
await page.waitForTimeout(8000); // wait for suggest + price check to settle

// ---- find and click the カート button ----
const cartBtn = page
  .locator('button:visible, a:visible')
  .filter({ hasText: /カート/ })
  .first();
if ((await cartBtn.count()) === 0) {
  console.log('!! no カート button found; dumping visible buttons:');
  const btns = page.locator('button:visible');
  const bn = await btns.count();
  for (let i = 0; i < Math.min(bn, 20); i++) console.log(`   [${i}] ${(await btns.nth(i).innerText()).replace(/\s+/g, ' ').slice(0, 40)}`);
  await page.screenshot({ path: 'probe-cart-nobtn.png', fullPage: true });
} else {
  console.log(`== click: ${(await cartBtn.innerText()).replace(/\s+/g, ' ')}`);
  const disabled = await cartBtn.isDisabled().catch(() => false);
  console.log(`   (disabled=${disabled})`);
  if (!disabled) {
    await cartBtn.click();
    await page.waitForTimeout(10000); // capture the cart call / login redirect
    console.log(`== after click, url: ${page.url()}`);
    await page.screenshot({ path: 'probe-cart-after.png', fullPage: true });

    // Check the cart page state (guest cart?)
    console.log('== open cart page');
    await page.goto(`${ORIGIN}/my/cart.html`, { waitUntil: 'domcontentloaded', timeout: 60000 }).catch((e) => console.log(`   cart nav: ${e.message}`));
    await page.waitForTimeout(6000);
    console.log(`== cart url now: ${page.url()}`);
    await page.screenshot({ path: 'probe-cart-page.png', fullPage: true });
    const bodyText = (await page.locator('body').innerText().catch(() => '')).replace(/\s+/g, ' ');
    console.log(`== cart page text (first 500): ${bodyText.slice(0, 500)}`);
  }
}

console.log('\n== done (window stays open 15s for inspection)');
await page.waitForTimeout(15000);
await browser.close();
