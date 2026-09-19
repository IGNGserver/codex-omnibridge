#!/usr/bin/env node
// Every HTTP client in this project must explicitly refuse redirects.
//
// reqwest's default is `Policy::limited(10)`, and on a 307/308 it **resends the
// request body** to the redirect target. The account client POSTs a live OAuth
// refresh token, so a redirect would hand that token to another host — verified
// locally: the redirect target received `refresh_token=SECRET-RT`.
//
// The provider/upstream clients carry API keys and the Router's capability
// header, so the same rule applies to them.
//
// Run: node scripts/check-http-clients.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const failures = [];

/** Files that construct an HTTP client. */
/// Remove every top-level `#[cfg(test)]` module from `source`.
///
/// Test code is skipped because it may construct throwaway clients and
/// supervisors that the production rules do not apply to.
///
/// This is deliberately line-based rather than brace-counted. A brace counter
/// (even a string-aware one) is fragile here: this codebase contains raw-string
/// JSON literals and lone apostrophes (`trim_end_matches('/')`) that must be
/// tokenised correctly to stay balanced, and any mistake silently swallows
/// production code — which is exactly how an earlier version of this guard
/// missed a bare `Client::new()` sending a provider API key. Every test module in
/// this project is written at column 0 as `#[cfg(test)]` + `mod <name> {`, so
/// stripping to the next line that is exactly `}` is both simpler and safer.
function stripTestModules(source) {
  const lines = source.split("\n");
  const kept = [];
  let skipping = false;
  for (const line of lines) {
    if (!skipping) {
      if (/^#\[cfg\(test\)\]\s*$/.test(line)) {
        skipping = true;
        continue;
      }
      kept.push(line);
    } else if (/^\}\s*$/.test(line)) {
      // End of the top-level test module.
      skipping = false;
    }
  }
  return kept.join("\n");
}

const CRATES = ["core", "credentials", "catalog", "integration", "manager", "router", "web", "cli", "desktop"];

function rustFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(full));
    else if (entry.name.endsWith(".rs")) out.push(full);
  }
  return out;
}

for (const crate of CRATES) {
  const src = path.join(ROOT, "crates", crate, "src");
  if (!fs.existsSync(src)) continue;
  for (const file of rustFiles(src)) {
    const raw = fs.readFileSync(file, "utf8");

    const production = stripTestModules(raw);
    const source = production
      .split("\n")
      .filter((line) => {
        const trimmed = line.trimStart();
        return !trimmed.startsWith("//") && !trimmed.startsWith("///");
      })
      .join("\n");
    // Count client constructions, then how many explicitly set the policy.
    const builders = (source.match(/Client::builder\(\)/g) || []).length;
    const policies = (source.match(/redirect\(reqwest::redirect::Policy::none\(\)\)/g) || []).length;
    if (builders > policies) {
      failures.push(
        `${path.relative(ROOT, file)} has ${builders} \`Client::builder()\` but only ` +
          `${policies} explicit \`redirect(Policy::none())\`; a redirected POST would ` +
          `resend its body (API key / refresh token) to another host`,
      );
    }

    // `Client::new()` is the *unhardened* constructor: it follows up to ten
    // redirects. Earlier this guard skipped it entirely on the assumption that it
    // only ever appeared as a logged fallback — which let the CLI's
    // `fetch-models` use it as its PRIMARY client while sending the provider's
    // API key. Every `Client::new()` must now sit inside an `unwrap_or_else`
    // fallback (where the runtime prints a FATAL warning).
    const bareNew = (source.match(/Client::new\(\)/g) || []).length;
    const fallbackNew = (
      source.match(/unwrap_or_else\([\s\S]{0,400}?Client::new\(\)/g) || []
    ).length;
    if (bareNew > fallbackNew) {
      failures.push(
        `${path.relative(ROOT, file)} uses \`Client::new()\` ${bareNew} time(s) but only ` +
          `${fallbackNew} of them are inside an \`unwrap_or_else\` fallback; ` +
          `\`Client::new()\` follows redirects and would resend credentials to another host`,
      );
    }
  }
}

if (failures.length > 0) {
  console.error("http client check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log("http client check: OK (every client refuses redirects)");
