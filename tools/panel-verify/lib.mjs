// Shared helpers for the panel verification suites.
//
// Playwright is intentionally not a dependency of this project (the panel
// itself ships zero runtime dependencies). It is resolved lazily from the
// usual places, or from PLAYWRIGHT_PATH, so these suites stay runnable in CI
// only when the tool is actually available.
import { createRequire } from "node:module";

export const BASE = process.env.PANEL_BASE || "http://127.0.0.1:4599";

export function loadPlaywright() {
  const require = createRequire(import.meta.url);
  for (const candidate of [process.env.PLAYWRIGHT_PATH, "playwright", "playwright-core"]) {
    if (!candidate) continue;
    try {
      return require(candidate);
    } catch {
      // try the next candidate
    }
  }
  throw new Error(
    "playwright not found. Install it (npm i -D playwright) or set PLAYWRIGHT_PATH.",
  );
}

export async function launch() {
  const { chromium } = loadPlaywright();
  return chromium.launch(
    process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {},
  );
}

// CSP directive warnings that are expected by design. `frame-ancestors` is
// specified to be ignored when delivered via <meta> (the Rust response header
// carries it for the HTTP path), so its warning is not a defect.
const BENIGN = [/frame-ancestors.*ignored when delivered via a <meta>/i];

// Attach console/pageerror/request-failure listeners to a page.
// `fail` and `note` are callbacks so each suite controls its own reporting.
export function watch(page, where, fail, note) {
  page.on("console", (m) => {
    const text = m.text();
    if (BENIGN.some((re) => re.test(text))) {
      note?.(`benign CSP note: ${text}`);
      return;
    }
    if (/Content Security Policy|Refused to/i.test(text)) {
      fail(`${where}: CSP: ${text}`);
      return;
    }
    if (m.type() === "error") fail(`${where}: console.error: ${text}`);
  });
  page.on("pageerror", (e) => fail(`${where}: pageerror: ${e.message}`));
  page.on("requestfailed", (r) => {
    if (r.url().startsWith(BASE)) {
      fail(`${where}: requestfailed: ${r.url()} ${r.failure()?.errorText}`);
    }
  });
}

export async function gotoPanel(page, path = "/") {
  await page.goto(`${BASE}${path}`, { waitUntil: "networkidle" });
  await page.waitForTimeout(700);
}

export async function showView(page, viewId) {
  await page.evaluate((id) => {
    document
      .querySelectorAll(".m3-nav-item")
      .forEach((b) => b.dataset.target === id && b.click());
  }, viewId);
  await page.waitForTimeout(450);
}

// WCAG relative luminance and contrast ratio.
export function luminance([r, g, b]) {
  const f = (v) => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}

export function contrastRatio(a, b) {
  const [hi, lo] = luminance(a) > luminance(b) ? [luminance(a), luminance(b)] : [luminance(b), luminance(a)];
  return (hi + 0.05) / (lo + 0.05);
}

export function parseRgb(value) {
  return (value.match(/[\d.]+/g) || []).slice(0, 3).map(Number);
}

export function report(label, problems, notes = []) {
  console.log(`\n===== ${label} =====`);
  console.log(`problems: ${problems.length}`);
  for (const p of problems) console.log(`  ✗ ${p}`);
  if (notes.length) {
    console.log(`notes: ${notes.length}`);
    for (const n of notes) console.log(`  · ${n}`);
  }
  return problems.length ? 1 : 0;
}
