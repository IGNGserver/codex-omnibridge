// Derive all brand assets from the approved raster artwork.
//
// assets/icon-source.png is the single source of truth. The SVG is a
// self-contained raster wrapper for legacy consumers, not vector artwork.
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
const SOURCE = path.join(ROOT, "assets", "icon-source.png");

const CHROME = process.env.CHROME_PATH || undefined;

// The source artwork contains a dark rounded backing plate on an opaque black
// square. Keep the artwork intact, but clip the generated application assets to
// the backing plate so Windows does not embed a black square in the taskbar.
// These ratios are measured against the 512px canonical app icon and scale to
// every generated size, including the 64px favicon.
const MASK_INSET_RATIO = 25 / 512;
const MASK_RADIUS_RATIO = 88 / 512;

(async () => {
  const source = fs.readFileSync(SOURCE);
  const dataUrl = `data:image/png;base64,${source.toString("base64")}`;

  const browser = await chromium.launch(
    CHROME ? { executablePath: CHROME } : {},
  );
  try {
    const page = await browser.newPage();
    const outputs = [
      [512, "assets/icon.png"],
      [256, "apps/panel/app-icon.png"],
      [64, "apps/panel/favicon.png"],
      [180, "apps/panel/apple-touch-icon.png"],
    ];
    console.log("==> rendering brand icons");
    for (const [size, relativePath] of outputs) {
      const png = await page.evaluate(async ({ dataUrl, size, insetRatio, radiusRatio }) => {
        const img = new Image();
        img.src = dataUrl;
        await img.decode();
        if (img.naturalWidth !== img.naturalHeight) {
          throw new Error("Icon source must be square");
        }
        const canvas = document.createElement("canvas");
        canvas.width = canvas.height = size;
        const ctx = canvas.getContext("2d");
        ctx.imageSmoothingEnabled = true;
        ctx.imageSmoothingQuality = "high";
        ctx.drawImage(img, 0, 0, size, size);
        // Remove only the opaque outer square; all internal shadows and artwork
        // remain unchanged.
        const inset = size * insetRatio;
        const radius = size * radiusRatio;
        ctx.globalCompositeOperation = "destination-in";
        ctx.beginPath();
        ctx.roundRect(inset, inset, size - inset * 2, size - inset * 2, radius);
        ctx.fill();
        ctx.globalCompositeOperation = "source-over";
        return canvas.toDataURL("image/png").split(",")[1];
      }, { dataUrl, size, insetRatio: MASK_INSET_RATIO, radiusRatio: MASK_RADIUS_RATIO });
      const buf = Buffer.from(png, "base64");
      fs.writeFileSync(path.join(ROOT, relativePath), buf);
      console.log(`  ${relativePath}  ${size}x${size}  ${buf.length} bytes`);
    }
    const svgInset = 512 * MASK_INSET_RATIO;
    const svgRadius = 512 * MASK_RADIUS_RATIO;
    fs.writeFileSync(path.join(ROOT, "assets/icon.svg"),
      `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 512 512" role="img" aria-label="Codex OmniBridge"><!-- Generated raster wrapper; source: icon-source.png --><defs><clipPath id="icon-shape"><rect x="${svgInset}" y="${svgInset}" width="${512 - svgInset * 2}" height="${512 - svgInset * 2}" rx="${svgRadius}" /></clipPath></defs><image width="512" height="512" clip-path="url(#icon-shape)" xlink:href="${dataUrl}" /></svg>\n`);
  } finally {
    await browser.close();
  }
})();
