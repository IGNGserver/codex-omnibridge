#!/usr/bin/env node

// Keep the three icon consumers in sync: the Electron shell, the embedded panel
// and the browser-installed panel. This is intentionally dependency-free so it
// also runs in a clean release checkout before electron-builder starts.

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const expected = [
  ["assets/icon-source.png", 1254, 1254],
  ["assets/icon.png", 512, 512],
  ["apps/panel/app-icon.png", 256, 256],
  ["apps/panel/favicon.png", 64, 64],
  ["apps/panel/apple-touch-icon.png", 180, 180],
];

const failures = [];

function pngSize(relative) {
  const file = path.join(ROOT, relative);
  if (!fs.existsSync(file)) {
    failures.push(`${relative} is missing; run node scripts/build-icons.mjs`);
    return null;
  }
  const bytes = fs.readFileSync(file);
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (bytes.length < 24 || !bytes.subarray(0, 8).equals(signature)) {
    failures.push(`${relative} is not a PNG`);
    return null;
  }
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

for (const [relative, width, height] of expected) {
  const actual = pngSize(relative);
  if (!actual) continue;
  if (actual.width !== width || actual.height !== height) {
    failures.push(
      `${relative} is ${actual.width}x${actual.height}; expected ${width}x${height}`,
    );
  }
}

const indexPath = path.join(ROOT, "apps/panel/index.html");
const index = fs.readFileSync(indexPath, "utf8");
for (const asset of ["favicon.png", "apple-touch-icon.png", "app-icon.png"]) {
  if (!index.includes(asset)) failures.push(`apps/panel/index.html does not reference ${asset}`);
}

if (failures.length) {
  console.error("icon check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log("icon check: OK (source, generated sizes and panel references are present)");
