#!/usr/bin/env node
// Guards the credential-cache invalidation invariant.
//
// The Router caches resolved provider credentials per `registry.generation()`,
// and `CredentialCache::get` only clears its entries when the generation it is
// asked for *differs* from the cached one. So any code path that mutates routing
// state (a provider's base_url/protocol/enabled, a model's enabled flag, an API
// key) and saves must also call `bump_generation()`.
//
// Without it, editing an API key while a Router is running keeps sending the
// previous key — verified live: the upstream kept receiving `Bearer sk-OLD-KEY`
// after the key had been changed.
//
// The check is structural: a function that saves a registry it mutated in place
// must mention bump_generation. Functions that go through the core APIs
// (`add_provider`, `edit_model`, ...) already bump internally and are exempt.
//
// Run: node scripts/check-generation-bump.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const failures = [];

// (file, function-name-substring) pairs that mutate routing state in place.
const guarded = [
  ["crates/manager/src/lib.rs", "pub fn edit_provider"],
  ["crates/manager/src/lib.rs", "pub fn set_model_enabled"],
  ["crates/cli/src/main.rs", "ProviderCommand::Edit"],
  ["crates/cli/src/main.rs", "fn set_model_enabled"],
];

// Core APIs that bump the generation themselves.
const selfBumping = [
  "add_provider(",
  "remove_provider(",
  "add_model(",
  "edit_provider_with_auth(",
  "edit_model(",
  "remove_model(",
  "set_official_model_ids(",
];

for (const [file, marker] of guarded) {
  const full = path.join(ROOT, file);
  if (!fs.existsSync(full)) {
    failures.push(`${file} is missing`);
    continue;
  }
  const source = fs.readFileSync(full, "utf8");
  const start = source.indexOf(marker);
  if (start === -1) {
    failures.push(`${file}: could not find \`${marker}\`; update this guard`);
    continue;
  }
  // Brace-match from the first `{` after the marker so the whole body is
  // inspected. A naive "up to the next `}`" stops at the first nested block and
  // reported a false positive on `edit_provider`.
  const open = source.indexOf("{", start);
  let depth = 0;
  let end = open;
  for (let i = open; i < source.length; i += 1) {
    if (source[i] === "{") depth += 1;
    else if (source[i] === "}") {
      depth -= 1;
      if (depth === 0) {
        end = i + 1;
        break;
      }
    }
  }
  const body = source.slice(start, end);
  const bumps = body.includes("bump_generation");
  const delegates = selfBumping.some((api) => body.includes(api));
  if (!bumps && !delegates) {
    failures.push(
      `${file}: \`${marker}\` mutates routing state but neither calls ` +
        `bump_generation() nor delegates to a self-bumping core API; a running ` +
        `Router would keep using cached credentials`,
    );
  }
}

if (failures.length > 0) {
  console.error("generation-bump check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  "generation-bump check: OK (every in-place routing mutation invalidates the credential cache)",
);
