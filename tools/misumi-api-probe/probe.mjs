// MISUMI internal price/delivery API probe (reproducible).
//
// WHY a real browser: jp.misumi-ec.com is protected by Akamai Bot Manager.
// Plain HTTP clients (curl / fetch / Python requests) cannot generate a valid
// `_abck` cookie, so they get blocked. We launch real Edge, let Akamai's JS run,
// then issue requests via Playwright's APIRequestContext, which (a) reuses the
// browser's cookies and (b) is NOT subject to CORS.
//
// Usage:
//   node probe.mjs lookup CBT3-8            # suggest -> price/delivery for one part
//   node probe.mjs batch CBT3-8 CBT3-10 ... # one batched price/delivery request
//   node probe.mjs sweep                    # batch-size sweep to characterize limits
//
// See ../../docs/misumi-api/ for the documented findings.

import { chromium } from 'playwright';

const APP_ID = 'de30e2b2-db86-435d-9929-646c11a3c4cd';
const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
const SUGGEST = `${ORIGIN}/api/v1/partNumber/suggest`;
const PRICE = 'https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check';

const HEADERS = {
  'Content-Type': 'application/json',
  Origin: ORIGIN,
  Referer: PAGE,
};

async function withSession(fn) {
  const browser = await chromium.launch({ channel: 'msedge', headless: false });
  try {
    const ctx = await browser.newContext({ locale: 'ja-JP' });
    const page = await ctx.newPage();
    await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
    await page.waitForTimeout(6000); // let Akamai JS establish a valid _abck
    return await fn(ctx);
  } finally {
    await browser.close();
  }
}

// Resolve a part number -> brandCode / seriesCode via the suggest endpoint.
async function suggest(req, partNumber) {
  const url = `${SUGGEST}?applicationId=${APP_ID}&keyword=${encodeURIComponent(partNumber.toLowerCase())}&field=@default,partNumberList.checkCFlag`;
  const r = await req.get(url, { headers: { Referer: PAGE } });
  const j = await r.json();
  return j.partNumberList?.[0] ?? null;
}

async function priceDelivery(req, detailList) {
  const r = await req.post(PRICE, { headers: HEADERS, data: { detailList }, timeout: 120000 });
  return { status: r.status(), json: r.ok() ? await r.json() : null, raw: r.ok() ? null : await r.text() };
}

const cmd = process.argv[2];
const args = process.argv.slice(3);

if (cmd === 'lookup') {
  const pn = args[0] || 'CBT3-8';
  await withSession(async (ctx) => {
    const req = ctx.request;
    const s = await suggest(req, pn);
    if (!s) return console.log(`no suggestion for ${pn}`);
    console.log('suggest:', JSON.stringify(s, null, 2));
    const { status, json } = await priceDelivery(req, [{ qty: 1, inputProductCode: s.partNumber, brandCode: s.brandCode }]);
    const d = json?.detailList?.[0];
    console.log(`HTTP ${status}`);
    console.log(`  単価(税別): ${d?.salesPrice?.salesUnitPrice}  税込: ${d?.salesPrice?.salesUnitPriceIncludingTax}`);
    console.log(`  出荷日(vsd): ${d?.leadTime?.vsd}  在庫: ${d?.trade?.immediateShippableQty}`);
  });
} else if (cmd === 'batch') {
  const parts = args.length ? args : ['CBT3-8', 'CBT3-10', 'CBT3-12'];
  await withSession(async (ctx) => {
    const detailList = parts.map((p) => ({ qty: 1, inputProductCode: p, brandCode: 'MSM1' }));
    const t0 = Date.now();
    const { status, json } = await priceDelivery(ctx.request, detailList);
    console.log(`req=${parts.length} HTTP ${status} returned=${json?.detailList?.length ?? '-'} in ${Date.now() - t0}ms`);
    for (const d of json?.detailList ?? []) {
      console.log(`  ${d.product?.inputProductCode}\t¥${d.salesPrice?.salesUnitPrice}\t${d.leadTime?.vsd}`);
    }
  });
} else if (cmd === 'sweep') {
  await withSession(async (ctx) => {
    const sizes = [1, 50, 100, 200, 500, 1000, 2000];
    for (const n of sizes) {
      const detailList = Array.from({ length: n }, () => ({ qty: 1, inputProductCode: 'CBT3-8', brandCode: 'MSM1' }));
      const t0 = Date.now();
      let line;
      try {
        const { status, json, raw } = await priceDelivery(ctx.request, detailList);
        line = `req=${String(n).padStart(5)} HTTP ${status} returned=${json?.detailList?.length ?? '-'} ${Date.now() - t0}ms ${raw ? '| ' + raw.slice(0, 60) : ''}`;
      } catch (e) {
        line = `req=${String(n).padStart(5)} EXC ${e.message.slice(0, 80)}`;
      }
      console.log(line);
      await ctx.request.dispose?.();
      await new Promise((r) => setTimeout(r, 1500)); // be polite
    }
  });
} else {
  console.log('usage: node probe.mjs <lookup|batch|sweep> [parts...]');
}
