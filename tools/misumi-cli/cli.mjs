#!/usr/bin/env node
// Headless-capable CLI for the MISUMI price/delivery lookup.
//
// Uses the SAME shared core as the Tauri app (../../shared/misumi-lookup.js),
// injected into a real Edge/Chromium page (Akamai-satisfied) via Playwright.
// Outputs JSON to stdout; exits non-zero on failure so callers can assert.
//
//   node cli.mjs lookup CBT3-8
//   node cli.mjs batch CBT3-8 CBTB5-12 E-GBSCB4-20
//   node cli.mjs lookup CBT3-8 --pretty            # human summary to stderr
//   node cli.mjs lookup CBT3-8 --timeout=45        # overall deadline (sec)
//
// GUARANTEED EXIT: every run is bounded by a deadline (default 60s, --timeout /
// TIMEOUT_MS). On timeout or any error it prints {ok:false,error} JSON and exits
// 1 — it never hangs. Useful where the real browser may be blocked by Akamai
// (e.g. headless / CI): you still get a deterministic non-zero exit.
//
// Env:
//   HEADLESS=1     run headless (experimental; Akamai may block it)
//   TIMEOUT_MS / --timeout=SEC   overall deadline

import { chromium } from "playwright";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const __dirname = dirname(fileURLToPath(import.meta.url));
const CORE = readFileSync(join(__dirname, "..", "..", "shared", "misumi-lookup.js"), "utf8");
const PAGE = "https://jp.misumi-ec.com/order/part-number/create";

function usage() {
  console.error("usage: node cli.mjs <lookup|batch> <part...> [--pretty] [--timeout=SEC]");
  process.exit(2);
}

const argv = process.argv.slice(2);
const cmd = argv.shift();
const flags = argv.filter((a) => a.startsWith("--"));
const parts = argv.filter((a) => !a.startsWith("--"));
const pretty = flags.includes("--pretty");
const timeoutFlag = flags.find((f) => f.startsWith("--timeout="));
const timeoutSec = Number((timeoutFlag && timeoutFlag.split("=")[1]) || process.env.TIMEOUT_MS / 1000 || 60);
const TIMEOUT_MS = Math.max(5, Number.isFinite(timeoutSec) ? timeoutSec : 60) * 1000;
if (!cmd || parts.length === 0 || (cmd !== "lookup" && cmd !== "batch")) usage();

const headless = process.env.HEADLESS === "1";

function out(obj) {
  process.stdout.write(JSON.stringify(obj, null, 2) + "\n");
}
const deadline = (ms, label) =>
  new Promise((_, rej) => setTimeout(() => rej(new Error(label || `timeout after ${ms}ms`)), ms));

// Last-resort failsafe: if even browser.close() stalls, force the process out.
const hardKill = setTimeout(() => {
  try { out({ ok: false, error: `hard timeout (${TIMEOUT_MS + 8000}ms) — forced exit` }); } catch {}
  process.exit(1);
}, TIMEOUT_MS + 8000);
if (hardKill.unref) hardKill.unref();

let browser;
async function work() {
  browser = await chromium.launch({ channel: "msedge", headless });
  const ctx = await browser.newContext({ locale: "ja-JP" });
  const page = await ctx.newPage();
  page.setDefaultTimeout(TIMEOUT_MS);
  page.setDefaultNavigationTimeout(TIMEOUT_MS);
  await page.goto(PAGE, { waitUntil: "domcontentloaded", timeout: TIMEOUT_MS });
  await page.waitForTimeout(Math.min(6000, TIMEOUT_MS / 2)); // let Akamai JS establish _abck
  await page.evaluate(CORE); // defines window.MisumiCore

  // page.evaluate has no built-in timeout, so race the in-page call against a
  // JS timer: a hung fetch (Akamai holding the connection) rejects instead of
  // hanging forever.
  const runner = ({ ps, ms, single }) =>
    Promise.race([
      single ? window.MisumiCore.lookupOne(ps[0]) : window.MisumiCore.lookupMany(ps),
      new Promise((_, r) => setTimeout(() => r(new Error("in-page timeout")), ms)),
    ]);
  return await page.evaluate(runner, { ps: parts, ms: TIMEOUT_MS, single: cmd === "lookup" });
}

let exitCode = 0;
try {
  const result = await Promise.race([work(), deadline(TIMEOUT_MS, `timeout after ${TIMEOUT_MS}ms`)]);
  if (pretty) {
    for (const d of result.price?.detailList || []) {
      const err = (d.errorMessageList || []).map((e) => e.message).join("; ");
      console.error(
        `${d.product?.inputProductCode}\t¥${d.salesPrice?.salesUnitPrice} (税込¥${d.salesPrice?.salesUnitPriceIncludingTax})\t出荷:${d.leadTime?.vsd}\t在庫:${d.trade?.immediateShippableQty}${err ? "\t⚠ " + err : ""}`
      );
    }
  }
  out(result);
  if (result && result.ok === false) exitCode = 1;
} catch (e) {
  out({ ok: false, error: String((e && e.message) || e) });
  exitCode = 1;
} finally {
  try {
    await Promise.race([browser ? browser.close() : Promise.resolve(), deadline(5000, "close timeout")]);
  } catch {}
  clearTimeout(hardKill);
}
process.exit(exitCode);
