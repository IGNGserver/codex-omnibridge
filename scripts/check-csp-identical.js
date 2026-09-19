#!/usr/bin/env node
// The panel's Content-Security-Policy is declared twice and the two must match:
//
//   * `crates/web/src/lib.rs` sends it as an HTTP header for the browser case;
//   * `apps/panel/index.html` carries it as a <meta> tag for the Electron
//     `file://` case, where no HTTP header exists.
//
// A meta-tag CSP can only ever *restrict* what the header allows, so if the two
// drift the Electron build enforces a different (usually weaker or, worse,
// silently broken) policy than the browser build — and nothing would notice,
// because each file looks correct on its own.
//
// Run: node scripts/check-csp-identical.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const normalize = (value) => value.replace(/\\/g, " ").split(/\s+/).filter(Boolean).join(" ");

const rustPath = path.join(ROOT, "crates/web/src/lib.rs");
const htmlPath = path.join(ROOT, "apps/panel/index.html");

for (const file of [rustPath, htmlPath]) {
  if (!fs.existsSync(file)) {
    console.error(`csp check FAILED: ${path.relative(ROOT, file)} is missing`);
    process.exit(1);
  }
}

const rust = fs.readFileSync(rustPath, "utf8");
const html = fs.readFileSync(htmlPath, "utf8");

const headerMatch = rust.match(
  /header::CONTENT_SECURITY_POLICY,\s*HeaderValue::from_static\(\s*"([^"]+)"/s,
);
if (!headerMatch) {
  console.error("csp check FAILED: could not find the served CSP header in crates/web/src/lib.rs");
  process.exit(1);
}

const metaMatch =
  html.match(/http-equiv="Content-Security-Policy"[\s\S]{0,200}?content="([^"]+)"/) ||
  html.match(/content="(default-src[^"]+)"/);
if (!metaMatch) {
  console.error("csp check FAILED: could not find the <meta> CSP in apps/panel/index.html");
  process.exit(1);
}

const failures = [];
if (!headerMatch[1].includes("default-src 'none'")) {
  failures.push("the served CSP must start from 'default-src \\'none\\''");
}

const header = normalize(headerMatch[1]);
const meta = normalize(metaMatch[1]);
if (header !== meta) {
  failures.push(
    "the served CSP header and the index.html <meta> CSP have drifted apart:\n" +
      `      header: ${header}\n` +
      `      meta:   ${meta}`,
  );
}

if (failures.length > 0) {
  console.error("csp check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log("csp check: OK (served header and index.html <meta> declare the same policy)");
