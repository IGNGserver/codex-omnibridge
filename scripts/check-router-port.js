#!/usr/bin/env node
// Guards a defect that is invisible to every other check.
//
// `sync` writes a fixed `base_url` (default `http://127.0.0.1:8787/v1`) into the
// managed `config.toml`, and stock Codex dials *that* address. It never reads the
// Router's endpoint file. A supervisor that starts a Router with
// `RouterSupervisor::new(..)` therefore binds an ephemeral port, and every Codex
// request is sent to a port nobody owns — the whole product stops working, with
// no error anywhere: the endpoint file advertises the real port, so the Router
// looks healthy and all endpoint-file-based tests pass.
//
// This script asserts that every call site which *starts* a Router also pins the
// configured port via `.with_port(..)`.
//
// Run: node scripts/check-router-port.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const failures = [];

// Call sites that legitimately do not start a Router.
// - `uninstall` only sends `/admin/shutdown` to whatever the endpoint file names.
const ALLOWED_WITHOUT_PORT = [
  { file: "crates/cli/src/main.rs", fn: "uninstall" },
];

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

function walk(dir) {
  const out = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "target" || entry.name === "node_modules") continue;
      out.push(...walk(full));
    } else if (entry.name.endsWith(".rs")) {
      out.push(full);
    }
  }
  return out;
}

/** Find the enclosing `fn <name>` of a byte offset, if any. */
function enclosingFunction(source, offset) {
  const before = source.slice(0, offset);
  const matches = [...before.matchAll(/\bfn\s+([a-zA-Z0-9_]+)\s*[(<]/g)];
  return matches.length > 0 ? matches[matches.length - 1][1] : null;
}

for (const file of walk(path.join(ROOT, "crates"))) {
  // Scan production code only, with every test module removed (not merely
  // truncated at the first one).
  const source = stripTestModules(fs.readFileSync(file, "utf8"));
  const relative = path.relative(ROOT, file);

  for (const match of source.matchAll(/RouterSupervisor::new\s*\(/g)) {
    const index = match.index;
    // Look at the statement that follows for a `.with_port(..)` chained call.
    const window = source.slice(index, index + 700);
    // Stop at the end of the statement (a `;` at depth 0 is a good enough proxy).
    const terminator = window.indexOf(";");
    const statement = terminator === -1 ? window : window.slice(0, terminator);
    if (/\.with_port\s*\(/.test(statement)) continue;

    const fn = enclosingFunction(source, index);
    const allowed = ALLOWED_WITHOUT_PORT.some(
      (entry) => entry.file === relative && entry.fn === fn,
    );
    if (allowed) continue;

    const line = source.slice(0, index).split("\n").length;
    failures.push(
      `${relative}:${line} (fn ${fn || "<unknown>"}) starts a Router without .with_port(..); ` +
        `Codex dials config.toml's base_url and would reach nothing`,
    );
  }
}

// User-facing documentation must not tell anyone to start the Router on an
// ephemeral port: stock Codex dials the fixed `base_url` from `config.toml`, so
// the documented command would produce a Router no client can reach.
// The audit/verification reports legitimately quote the historical bug, so they
// are excluded.
const DOC_EXCLUDES = ["CODE_AUDIT_AND_FIX_PLAN.md", "VERIFICATION_AND_FIX_PLAN.md"];
const docsDir = path.join(ROOT, "docs");
for (const name of fs.readdirSync(docsDir)) {
  if (!name.endsWith(".md") || DOC_EXCLUDES.includes(name)) continue;
  const full = path.join(docsDir, name);
  const text = fs.readFileSync(full, "utf8");
  if (/--port\s+0\b/.test(text)) {
    failures.push(
      `docs/${name} documents '--port 0', which starts the Router on a port Codex ` +
        `does not know about; document the configured port instead`,
    );
  }
}
const readme = path.join(ROOT, "README.md");
if (fs.existsSync(readme) && /--port\s+0\b/.test(fs.readFileSync(readme, "utf8"))) {
  failures.push("README.md documents '--port 0'; document the configured port instead");
}

if (failures.length > 0) {
  console.error("router port check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  console.error(
    "\nBind the port the managed config advertises: " +
      "use `codex_mp_integration::router_port_for_registry(registry)` with `.with_port(..)`.",
  );
  process.exit(1);
}

console.log(
  "router port check: OK (every Router-starting call site pins the configured port)",
);
