#!/usr/bin/env node
// Static guards for the panel's design-token discipline.
//
// The panel used to carry ~30 hard-coded colour values below its semantic
// layer, which is how a dark-theme primary ended up baked into state layers
// for the light theme. Those bugs are invisible in a screenshot of the theme
// you happen to be using, so they are checked mechanically here.
//
// Also guards the M3 Expressive type rules: baseline roles are 400/500, and
// 700 exists only on emphasized roles. A stray `font-weight: 700` on a
// baseline role silently breaks the "emphasized never reflows" property.
//
// Run: node scripts/check-panel-tokens.js

const fs = require("fs");
const path = require("path");

const PANEL = path.resolve(__dirname, "../apps/panel");
const TOKENS_FILE = "tokens.css";
const failures = [];

function read(file) {
  return fs.readFileSync(path.join(PANEL, file), "utf8");
}

const cssFiles = fs
  .readdirSync(PANEL)
  .filter((f) => f.endsWith(".css"))
  .sort();

if (!cssFiles.includes(TOKENS_FILE)) {
  console.error(`panel token check FAILED: ${TOKENS_FILE} is missing`);
  process.exit(1);
}

// --- 1. Raw colours may only appear in the token layer -----------------------
const COLOR_RE = /#[0-9a-fA-F]{3,8}\b|\brgba?\s*\(|\bhsla?\s*\(/g;
for (const file of cssFiles) {
  if (file === TOKENS_FILE) continue; // the one place they are allowed
  const src = read(file);
  src.split("\n").forEach((line, i) => {
    // `content: "\e313"` is a glyph codepoint, not a colour; icons.css is
    // generated and contains only those.
    if (/^\s*content:/.test(line)) return;
    const hits = line.match(COLOR_RE);
    if (hits) {
      failures.push(`${file}:${i + 1} raw colour ${hits.join(", ")} — use a --md-sys-color-* token`);
    }
  });
}

// --- 2. colour-mix must reference tokens, not literals ----------------------
for (const file of cssFiles) {
  const src = read(file);
  src.split("\n").forEach((line, i) => {
    if (!/color-mix\(/.test(line)) return;
    if (/#[0-9a-fA-F]{3,8}\b|\brgba?\s*\(/.test(line)) {
      failures.push(`${file}:${i + 1} color-mix uses a literal colour — mix a token instead`);
    }
  });
}

// --- 3. Baseline type roles must not carry bold weights ---------------------
// Emphasized roles are the only legitimate home for 700; the baseline scale is
// 400 (display/headline/body) or 500 (title/label).
const BASELINE_ROLES = [
  "display-large", "display-medium", "display-small",
  "headline-large", "headline-medium", "headline-small",
  "title-large", "title-medium", "title-small",
  "body-large", "body-medium", "body-small",
  "label-large", "label-medium", "label-small",
];
const baseCss = read("base.css");
// Strip comments so a role name mentioned in prose cannot satisfy the check.
const baseCssNoComments = baseCss.replace(/\/\*[\s\S]*?\*\//g, "");
for (const role of BASELINE_ROLES) {
  // Every rule that targets the baseline role exactly -- not
  // `.m3-<role>-emphasized`, and not a descendant/combined selector. Scanning
  // all of them matters: a later `font-weight: 700` override appended to the
  // file would otherwise hide behind the first matching block.
  const blockRe = new RegExp(
    `(?:^|[},])\\s*\\.m3-${role}\\s*\\{([^}]*)\\}`,
    "gs",
  );
  const blocks = [...baseCssNoComments.matchAll(blockRe)].map((m) => m[1]);
  if (blocks.length === 0) {
    failures.push(`base.css is missing the .m3-${role} baseline role`);
    continue;
  }
  for (const body of blocks) {
    const weight = body.match(/font-weight:\s*(\d+)/);
    if (weight && Number(weight[1]) > 500) {
      failures.push(`.m3-${role} uses font-weight ${weight[1]}; baseline roles cap at 500`);
    }
  }
}

// --- 4. Emphasized roles must not redefine size or line-height --------------
// That is the whole point of the emphasized scale: it is a drop-in swap.
const emphasizedBlocks = baseCss.match(/\.m3-(?:display|headline|title|body|label)-[\w-]+-emphasized[^{]*\{[^}]*\}/gs) || [];
for (const block of emphasizedBlocks) {
  if (/font-size:/.test(block)) {
    failures.push(`an emphasized role sets font-size; it must inherit the baseline size\n    ${block.split("{")[0].trim()}`);
  }
  if (/line-height:/.test(block)) {
    failures.push(`an emphasized role sets line-height; it must inherit the baseline line-height\n    ${block.split("{")[0].trim()}`);
  }
}

// --- 5. Required Expressive tokens -------------------------------------------------
const tokens = read(TOKENS_FILE);
const REQUIRED_TOKENS = [
  // Expressive shape steps
  "--md-sys-shape-corner-large-increased",
  "--md-sys-shape-corner-extra-large-increased",
  "--md-sys-shape-corner-extra-extra-large",
  // Spring motion
  "--md-sys-motion-spring-fast-spatial",
  "--md-sys-motion-spring-default-spatial",
  "--md-sys-motion-spring-slow-spatial",
  "--md-sys-motion-spring-fast-effects",
  "--md-sys-motion-spring-default-effects",
  "--md-sys-motion-spring-slow-effects",
  // Emphasized transition curves
  "--md-sys-motion-easing-emphasized-decelerate",
  "--md-sys-motion-easing-emphasized-accelerate",
  // Colour roles the panel relies on
  "--md-sys-color-inverse-surface",
  "--md-sys-color-inverse-on-surface",
  "--md-sys-color-surface-container-highest",
  "--md-sys-color-outline-variant",
];
for (const token of REQUIRED_TOKENS) {
  if (!tokens.includes(`${token}:`)) {
    failures.push(`tokens.css is missing ${token}`);
  }
}

// --- 6. The emphasized decelerate/accelerate curves must exist and differ ---
// Note: `easing-emphasized` and `easing-standard` legitimately share the curve
// cubic-bezier(0.2, 0, 0, 1) -- the spec separates them by DURATION (500ms vs
// 300ms), not by shape. The variants are what carry distinct curves, so those
// are what must exist and must not collapse into each other.
const easingCurves = {};
for (const name of ["emphasized", "emphasized-decelerate", "emphasized-accelerate", "standard"]) {
  const m = tokens.match(new RegExp(`--md-sys-motion-easing-${name}:\\s*([^;]+);`));
  if (!m) {
    failures.push(`tokens.css is missing --md-sys-motion-easing-${name}`);
  } else {
    easingCurves[name] = m[1].trim();
  }
}
if (
  easingCurves["emphasized-decelerate"] &&
  easingCurves["emphasized-accelerate"] &&
  easingCurves["emphasized-decelerate"] === easingCurves["emphasized-accelerate"]
) {
  failures.push("emphasized-decelerate and emphasized-accelerate must use different curves");
}

// Durations must differ too: emphasized transitions are longer than standard.
const durEmphasized = tokens.match(/--md-sys-motion-duration-long2:\s*(\d+)ms/);
const durStandard = tokens.match(/--md-sys-motion-duration-medium2:\s*(\d+)ms/);
if (durEmphasized && durStandard && Number(durEmphasized[1]) <= Number(durStandard[1])) {
  failures.push(
    `emphasized duration (${durEmphasized[1]}ms) must exceed standard (${durStandard[1]}ms)`,
  );
}

// --- 7. Icon usage must reference a generated glyph class -------------------
// An icon span with neither a ligature nor a codepoint class renders nothing.
for (const file of ["index.html", "main.js"]) {
  const src = read(file);
  const spans = src.match(/<span[^>]*class="[^"]*material-symbols-outlined[^"]*"[^>]*>/g) || [];
  for (const span of spans) {
    // The snackbar swaps its class at runtime, so it is allowed to start bare.
    if (/id="snackbar-icon"/.test(span)) continue;
    // A template literal (e.g. m3-i-${icon}) selects the glyph at runtime.
    if (!/\bm3-i-(?:[\w-]+|\$\{)/.test(span)) {
      failures.push(`${file}: icon span without a glyph class: ${span.slice(0, 80)}`);
    }
  }
}

// --- 8. Fonts and icons must be self-hosted ---------------------------------
// The desktop app loads the panel over file://, where a CDN font simply does
// not resolve, and the CSP keeps font-src at 'self'.
const index = read("index.html");
if (/fonts\.googleapis\.com|fonts\.gstatic\.com/.test(index)) {
  failures.push("index.html still references a Google Fonts origin; fonts must be self-hosted");
}
const icons = read("icons.css");
if (!/@font-face/.test(icons) || !/url\('fonts\//.test(icons)) {
  failures.push("icons.css does not declare the self-hosted font faces");
}
if (!/Noto Sans SC/.test(icons)) {
  failures.push("icons.css does not declare the self-hosted CJK face");
}
if (!/--md-sys-typescale-font:[^;]*"Noto Sans SC"/.test(tokens)) {
  failures.push('the font stack in tokens.css does not fall back to "Noto Sans SC" for CJK');
}
const fontDir = path.join(PANEL, "fonts");
for (const font of [
  "roboto-flex-latin.woff2",
  "roboto-flex-latin-ext.woff2",
  "noto-sans-sc.woff2",
  "material-symbols-outlined.woff2",
]) {
  if (!fs.existsSync(path.join(fontDir, font))) {
    failures.push(`missing self-hosted font: apps/panel/fonts/${font}`);
  }
}

// --- 9. Motion must respect both the OS and the in-app preference -----------
if (!/prefers-reduced-motion/.test(baseCss)) {
  failures.push("base.css does not honour prefers-reduced-motion");
}
if (!/\[data-motion="reduced"\]/.test(baseCss)) {
  failures.push('base.css does not honour the in-app [data-motion="reduced"] override');
}

if (failures.length > 0) {
  console.error("panel token check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `panel token check: OK (${cssFiles.length} stylesheets, ${BASELINE_ROLES.length} baseline roles, ` +
    `${REQUIRED_TOKENS.length} required tokens)`,
);
