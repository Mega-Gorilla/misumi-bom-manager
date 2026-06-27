#!/usr/bin/env node
// Headless CLI for the MISUMI price/delivery lookup.
//
// Uses the SAME shared core as the Tauri app (../../shared/misumi-lookup.js),
// injected into a real Edge/Chromium page (Akamai-satisfied) via Playwright.
// Outputs JSON to stdout; exits non-zero on failure so CI / Claude Code can assert.
//
//   node cli.mjs lookup CBT3-8
//   node cli.mjs batch CBT3-8 CBTB5-12 E-GBSCB4-20
//   node cli.mjs lookup CBT3-8 --pretty        # human summary to stderr
//
// Env:
//   HEADLESS=0   run headed (default; matches the Akamai-proven path)
//   HEADLESS=1   run headless (experimental; Akamai may block)

import { chromium } from "playwright";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const CORE = readFileSync(join(__dirname, "..", "..", "shared", "misumi-lookup.js"), "utf8");
const PAGE = "https://jp.misumi-ec.com/order/part-number/create";

function usage() {
  console.error("usage: node cli.mjs <lookup|batch> <part...> [--pretty] [--json]");
  process.exit(2);
}

const argv = process.argv.slice(2);
const cmd = argv.shift();
const flags = new Set(argv.filter((a) => a.startsWith("--")));
const parts = argv.filter((a) => !a.startsWith("--"));
if (!cmd || parts.length === 0 || (cmd !== "lookup" && cmd !== "batch")) usage();

const headless = process.env.HEADLESS === "1";

const browser = await chromium.launch({ channel: "msedge", headless });
let exitCode = 0;
try {
  const ctx = await browser.newContext({ locale: "ja-JP" });
  const page = await ctx.newPage();
  await page.goto(PAGE, { waitUntil: "domcontentloaded", timeout: 60000 });
  await page.waitForTimeout(6000); // let Akamai JS establish a valid _abck
  await page.evaluate(CORE); // defines window.MisumiCore

  let result;
  if (cmd === "lookup") {
    result = await page.evaluate((pn) => window.MisumiCore.lookupOne(pn), parts[0]);
  } else {
    result = await page.evaluate((ps) => window.MisumiCore.lookupMany(ps), parts);
  }

  // Pretty human summary (to stderr, so stdout stays pure JSON).
  if (flags.has("--pretty")) {
    const rows = (result.price?.detailList || []).map((d) => ({
      pn: d.product?.inputProductCode,
      price: d.salesPrice?.salesUnitPrice,
      tax: d.salesPrice?.salesUnitPriceIncludingTax,
      vsd: d.leadTime?.vsd,
      stock: d.trade?.immediateShippableQty,
      err: (d.errorMessageList || []).map((e) => e.message).join("; "),
    }));
    for (const r of rows) {
      console.error(`${r.pn}\t¥${r.price} (税込¥${r.tax})\t出荷:${r.vsd}\t在庫:${r.stock}${r.err ? "\t⚠ " + r.err : ""}`);
    }
  }

  process.stdout.write(JSON.stringify(result, null, 2) + "\n");
  if (result && result.ok === false) exitCode = 1;
} catch (e) {
  process.stdout.write(JSON.stringify({ ok: false, error: String((e && e.message) || e) }, null, 2) + "\n");
  exitCode = 1;
} finally {
  await browser.close();
}
process.exit(exitCode);
