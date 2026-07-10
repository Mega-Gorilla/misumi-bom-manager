// MISUMI 型番一括入力 (order/part-number/create) — Excel-paste behavior probe.
//
// Question: does the create page accept a multi-line TSV paste (型番 TAB 数量,
// as copied from Excel) and expand it into multiple validated rows?
// If yes, the BOM app can simply put the list on the clipboard and open this
// page in the user's browser — no login handling, no cart API needed.
//
// Usage: node probe-paste.mjs

import { chromium } from 'playwright';

const ORIGIN = 'https://jp.misumi-ec.com';
const PAGE = `${ORIGIN}/order/part-number/create`;
// Excel-style TSV: part number TAB qty
const TSV = 'CBT3-8\t2\nCBT3-10\t3\nSFJ3-10\t1';

const browser = await chromium.launch({ channel: 'msedge', headless: false });
const ctx = await browser.newContext({ locale: 'ja-JP', permissions: ['clipboard-read', 'clipboard-write'] });
const page = await ctx.newPage();

console.log(`== open ${PAGE}`);
await page.goto(PAGE, { waitUntil: 'domcontentloaded', timeout: 60000 });
await page.waitForTimeout(7000); // Akamai warm-up

// Put TSV on the clipboard from within the page (real clipboard).
await page.evaluate(async (tsv) => await navigator.clipboard.writeText(tsv), TSV);
console.log('== clipboard set (3 rows TSV)');

// Look for a dedicated bulk-input UI first (まとめて/一括/貼り付け).
const bulk = page
  .locator('button:visible, a:visible')
  .filter({ hasText: /一括|まとめて|貼り付け|コピー/ });
const bulkN = await bulk.count();
console.log(`== bulk-input candidates: ${bulkN}`);
for (let i = 0; i < bulkN; i++) console.log(`   [${i}] ${(await bulk.nth(i).innerText()).replace(/\s+/g, ' ').slice(0, 50)}`);

// Focus the first grid part-number input (name=partNumber, NOT the header search box).
const target = page.locator('input[name="partNumber"]:visible').first();
await target.click();
await page.keyboard.press('Control+V');
console.log('== pasted, waiting for the grid to react…');
await page.waitForTimeout(12000);

// Dump the grid state: how many rows now hold our part numbers?
const text = (await page.locator('body').innerText()).replace(/\s+/g, ' ');
for (const pn of ['CBT3-8', 'CBT3-10', 'SFJ3-10']) {
  console.log(`   ${pn}: ${text.includes(pn) ? 'PRESENT' : 'missing'}`);
}
// Count visible inputs that contain our part numbers (value check)
const values = await page.evaluate(() =>
  Array.from(document.querySelectorAll('input'))
    .map((i) => i.value)
    .filter(Boolean),
);
console.log(`== non-empty input values: ${JSON.stringify(values.slice(0, 20))}`);
await page.screenshot({ path: 'probe-paste-after.png', fullPage: true });
console.log('== screenshot: probe-paste-after.png');

console.log('== done (window stays open 20s for inspection)');
await page.waitForTimeout(20000);
await browser.close();
