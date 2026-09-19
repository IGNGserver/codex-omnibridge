#!/usr/bin/env node
// Static invariant checks for the panel's authentication token handling.
//
// The panel has no DOM test harness in this repository, so these assertions are
// deliberately source-level. They exist because the bugs they guard against are
// silent: a token that is never cleared keeps authenticating requests after the
// user logs out, and nothing in a normal run reveals it.
//
// Run: node scripts/check-panel-auth.js

const fs = require("fs");
const path = require("path");

const panelPath = path.resolve(__dirname, "../apps/panel/main.js");
const source = fs.readFileSync(panelPath, "utf8");

const failures = [];

function requireMatch(description, pattern) {
  if (!pattern.test(source)) {
    failures.push(description);
  }
}

function requireAbsent(description, pattern) {
  if (pattern.test(source)) {
    failures.push(description);
  }
}

// `queryToken` carries the privileged loopback token when the panel is opened
// with `?local_token=`. It must be reassignable so logout/401 can drop it; a
// `const` binding cannot be cleared and silently survives logout.
requireAbsent(
  "queryToken must not be declared with `const`: it has to be clearable on logout",
  /const\s+queryToken\s*=/,
);
requireMatch(
  "queryToken must be declared with `let` so it can be cleared",
  /let\s+queryToken\s*=/,
);

// The logout handler must clear every token source it can reach.
const logoutBody = source.slice(source.indexOf("logoutBtn.onclick"));
const logoutEnd = logoutBody.indexOf("};");
const logout = logoutEnd === -1 ? logoutBody : logoutBody.slice(0, logoutEnd);
if (!/queryToken\s*=\s*""/.test(logout)) {
  failures.push("logout must clear queryToken, not only the session token");
}
if (!/sessionToken\s*=\s*""/.test(logout)) {
  failures.push("logout must clear sessionToken");
}
if (!/localStorage\.removeItem\(\s*["']codex_mp_token["']\s*\)/.test(logout)) {
  failures.push("logout must remove the persisted session token");
}

// The 401 path must also discard the rejected token, otherwise every later
// request repeats the same failure against a token that can never work.
const unauthorizedIndex = source.indexOf("response.status === 401");
if (unauthorizedIndex === -1) {
  failures.push("the `api()` helper must handle HTTP 401");
} else {
  const handler = source.slice(unauthorizedIndex, unauthorizedIndex + 1200);
  if (!/sessionToken\s*=\s*""/.test(handler)) {
    failures.push("a 401 must clear sessionToken to avoid a stale-token retry loop");
  }
  if (!/queryToken\s*=\s*""/.test(handler)) {
    failures.push("a 401 must clear queryToken to avoid a stale-token retry loop");
  }
}

// The URL token must never be left in the address bar (history/Referer leak).
requireMatch(
  "the URL-provided token must be stripped from the address bar immediately",
  /urlParams\.delete\(\s*["']local_token["']\s*\)/,
);
requireMatch(
  "stripping the URL token must use replaceState, not a new history entry",
  /history\.replaceState/,
);

// --- Backend warnings must reach the user ------------------------------------
//
// The backend saves a change and then asks a running Router to reload it. If the
// reload fails the change is on disk but not being served, and the backend says so
// in `router_reload_warning`. A warning nobody renders is the same as no warning:
// the panel must surface it.
const panelMainPath = path.resolve(__dirname, "../apps/panel/main.js");
if (fs.existsSync(panelMainPath)) {
  const panel = fs.readFileSync(panelMainPath, "utf8");
  if (!panel.includes("router_reload_warning")) {
    failures.push(
      "apps/panel/main.js never renders `router_reload_warning`; a saved change that " +
        "the running Router did not pick up would be reported to the user as success",
    );
  }
}

// --- Electron backend supervision -------------------------------------------
//
// The main process restarts the Rust backend with exponential backoff and gives
// up after 5 attempts. If that counter is never reset, five crashes spread over
// hours of otherwise healthy operation permanently disable auto-restart, and the
// user is left with a dead panel until they relaunch by hand. The counter must
// therefore be reset once the backend reports that it is serving.
const electronMainPath = path.resolve(__dirname, "../apps/electron/main.js");
if (fs.existsSync(electronMainPath)) {
  const main = fs.readFileSync(electronMainPath, "utf8");
  // Look only *after* the declaration line: the declaration itself contains
  // `restartAttempts = 0`, so matching from just past the variable name would
  // always succeed and the guard would be vacuous.
  const declIndex = main.indexOf("let restartAttempts");
  const declEnd = declIndex === -1 ? -1 : main.indexOf("\n", declIndex);
  const afterDecl = declEnd === -1 ? "" : main.slice(declEnd + 1);
  const resetsCounter = /restartAttempts\s*=\s*0\s*;/.test(afterDecl);
  if (!resetsCounter) {
    failures.push(
      "apps/electron/main.js never resets restartAttempts; the restart limit must " +
        "mean 'consecutive failures', not 'failures since launch'",
    );
  }
}

if (failures.length > 0) {
  console.error("panel auth check FAILED:");
  for (const failure of failures) {
    console.error(`  - ${failure}`);
  }
  process.exit(1);
}

console.log("panel auth check: OK (token sources are cleared on logout and 401)");
