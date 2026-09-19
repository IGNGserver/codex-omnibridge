// Rasterise the brand SVG into the PNG assets the packagers need.
//
// assets/icon.svg is the single source of truth for the mark. electron-builder
// wants a 512x512 PNG for the app icon, and the panel wants a small favicon;
// both are generated here so they can never drift from the SVG.
//
// Run: node scripts/build-icons.mjs
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// Playwright is a dev-only tool and is deliberately not a dependency of this
// project (the panel itself is dependency-free). Resolve it from the usual
// places, or from PLAYWRIGHT_PATH when it lives somewhere unusual.
function loadPlaywright() {
  const require = createRequire(import.meta.url);
  const candidates = [
    process.env.PLAYWRIGHT_PATH,
    "playwright",
    "playwright-core",
  ].filter(Boolean);
  for (const candidate of candidates) {
    try {
      return require(candidate);
    } catch {
      // try the next one
    }
  }
  throw new Error(
    "playwright not found. Install it (npm i -D playwright) or set PLAYWRIGHT_PATH.",
  );
}

const { chromium } = loadPlaywright();
const SVG = path.join(ROOT, "assets", "icon.svg");
const OUT_ICON = path.join(ROOT, "assets", "icon.png");
const OUT_FAVICON = path.join(ROOT, "apps", "panel", "favicon.png");

const CHROME = process.env.CHROME_PATH || undefined;

(async () => {
  const svg = fs.readFileSync(SVG, "utf8");

  const browser = await chromium.launch(
    CHROME ? { executablePath: CHROME } : {},
  );
  const ctx = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await ctx.newPage();

  async function render(size, outPath, background) {
    await page.setViewportSize({ width: size, height: size });
    await page.setContent(
      `<!doctype html><html><head><meta charset="utf-8"><style>
        html,body{margin:0;padding:0;width:${size}px;height:${size}px;
          background:${background || "transparent"};}
        svg{display:block;width:${size}px;height:${size}px}
      </style></head><body>${svg}</body></html>`,
      { waitUntil: "load" },
    );
    const buf = await page.screenshot({ omitBackground: !background, type: "png" });
    fs.writeFileSync(outPath, buf);
    console.log(`  ${path.relative(ROOT, outPath)}  ${size}x${size}  ${buf.length} bytes`);
  }

  console.log("==> rendering brand icons");
  await render(512, OUT_ICON);
  await render(64, OUT_FAVICON);

  await browser.close();
})();
