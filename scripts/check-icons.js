#!/usr/bin/env node

// Keep the three icon consumers in sync: the Electron shell, the embedded panel
// and the browser-installed panel. This is intentionally dependency-free so it
// also runs in a clean release checkout before electron-builder starts.

const fs = require("fs");
const path = require("path");
const zlib = require("zlib");

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

// Decode just enough of an 8-bit RGBA PNG to verify the generated corners.
// Keeping this dependency-free makes the check usable in a clean release
// checkout, before the optional Playwright icon renderer is installed.
function pngAlpha(relative) {
  const file = path.join(ROOT, relative);
  const bytes = fs.readFileSync(file);
  const bitDepth = bytes[24];
  const colorType = bytes[25];
  const interlace = bytes[28];
  if (bitDepth !== 8 || colorType !== 6 || interlace !== 0) {
    failures.push(`${relative} must be a non-interlaced 8-bit RGBA PNG`);
    return null;
  }

  let offset = 8;
  const idat = [];
  while (offset + 12 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.toString("ascii", offset + 4, offset + 8);
    if (type === "IDAT") {
      idat.push(bytes.subarray(offset + 8, offset + 8 + length));
    }
    offset += 12 + length;
    if (type === "IEND") break;
  }

  const width = bytes.readUInt32BE(16);
  const height = bytes.readUInt32BE(20);
  const stride = width * 4;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  let rawOffset = 0;
  let previous = Buffer.alloc(stride);
  let minimum = 255;
  let maximum = 0;
  let transparentPixels = 0;
  const corners = [];

  function paeth(a, b, c) {
    const estimate = a + b - c;
    const pa = Math.abs(estimate - a);
    const pb = Math.abs(estimate - b);
    const pc = Math.abs(estimate - c);
    return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
  }

  for (let y = 0; y < height; y += 1) {
    const filter = raw[rawOffset++];
    const row = Buffer.alloc(stride);
    for (let x = 0; x < stride; x += 1) {
      const left = x >= 4 ? row[x - 4] : 0;
      const up = previous[x];
      const upLeft = x >= 4 ? previous[x - 4] : 0;
      let value = raw[rawOffset++];
      if (filter === 1) value += left;
      else if (filter === 2) value += up;
      else if (filter === 3) value += Math.floor((left + up) / 2);
      else if (filter === 4) value += paeth(left, up, upLeft);
      else if (filter !== 0) {
        failures.push(`${relative} uses unsupported PNG filter ${filter}`);
        return null;
      }
      row[x] = value & 0xff;
    }

    if (y === 0) corners.push(row[3], row[stride - 1]);
    if (y === height - 1) corners.push(row[3], row[stride - 1]);
    for (let x = 3; x < stride; x += 4) {
      const alpha = row[x];
      minimum = Math.min(minimum, alpha);
      maximum = Math.max(maximum, alpha);
      if (alpha === 0) transparentPixels += 1;
    }
    previous = row;
  }

  return { minimum, maximum, transparentPixels, corners };
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

for (const relative of expected.slice(1).map(([file]) => file)) {
  const alpha = pngAlpha(relative);
  if (!alpha) continue;
  if (alpha.transparentPixels === 0 || alpha.maximum !== 255) {
    failures.push(`${relative} has no usable transparent edge alpha`);
  }
  if (alpha.corners.some((value) => value !== 0)) {
    failures.push(`${relative} must have transparent corners`);
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
