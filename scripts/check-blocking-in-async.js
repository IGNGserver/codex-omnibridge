#!/usr/bin/env node
// A synchronous keyring call inside an `async fn` panics at runtime.
//
// On Linux the keyring backend (`zbus`) bridges to a synchronous API with
// `Runtime::block_on`, which aborts with "Cannot start a runtime from within a
// runtime" when called from an async worker. This project has now hit that six
// separate times (CLI, Router, Web account handlers, provider purge, Web
// provider/model handlers, and `provider fetch-models` — which panicked on every
// invocation). Each previous fix patched the call site that had been reported
// rather than searching for the pattern, which is why it kept coming back.
//
// The supported way to touch credentials from async code is the offloaded
// `codex_mp_credentials::{get,set,delete}_blocking` helpers.
//
// Detection is indentation-based rather than brace-based: walking backwards to
// the nearest less-indented `fn` is robust against braces inside string and char
// literals, which broke the brace-counting guard used elsewhere in this repo.
//
// Run: node scripts/check-blocking-in-async.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");

/** Direct (synchronous) keyring access that must not appear in async code. */
const SYNC_CALL = /(?:\.credentials\.(?:get|set|delete)\(|NativeCredentialStore::default\(\)\s*\.\s*(?:get|set|delete)\()/;

/** The offloaded helpers, which are safe from async code. */
const OFFLOADED = /_(?:blocking)\s*\(|run_blocking/;

function rustFiles(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) out.push(...rustFiles(full));
    else if (entry.name.endsWith(".rs")) out.push(full);
  }
  return out;
}

const indentOf = (line) => line.length - line.trimStart().length;

const failures = [];

for (const file of rustFiles(path.join(ROOT, "crates"))) {
  const lines = fs.readFileSync(file, "utf8").split("\n");
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const code = line.trimStart();
    if (code.startsWith("//") || code.startsWith("///")) continue;
    if (!SYNC_CALL.test(line)) continue;
    if (OFFLOADED.test(line)) continue;
    // Offloaded by the enclosing statement (e.g. a multi-line builder call).
    if (OFFLOADED.test(lines.slice(Math.max(0, i - 3), i + 1).join("\n"))) continue;

    // Walk backwards to the nearest enclosing declaration with less indentation.
    const indent = indentOf(line);
    for (let j = i - 1; j >= 0; j -= 1) {
      const candidate = lines[j];
      if (!/^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s/.test(candidate)) continue;
      if (indentOf(candidate) >= indent) continue;
      if (/^\s*(?:pub(?:\([^)]*\))?\s+)?async\s+fn\s/.test(candidate)) {
        const isTest = /#\[(?:tokio::)?test\]/.test(
          lines.slice(Math.max(0, j - 3), j).join("\n"),
        );
        if (!isTest) {
          failures.push(
            `${path.relative(ROOT, file)}:${i + 1} calls the keyring synchronously inside an ` +
              `async fn (\`${candidate.trim().slice(0, 60)}\`); use ` +
              `codex_mp_credentials::{get,set,delete}_blocking or spawn_blocking`,
          );
        }
      }
      break;
    }
  }
}

if (failures.length > 0) {
  console.error("blocking-in-async check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log("blocking-in-async check: OK (no synchronous keyring calls in async code)");
