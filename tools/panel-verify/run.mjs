// Run the full panel verification suite.
//
// Boots the mock backend, runs structure / contrast / functional in sequence,
// and returns a non-zero exit code if any of them fails.
//
// Playwright is not a project dependency (the panel ships zero runtime deps),
// so this reports clearly when it is unavailable rather than failing obscurely.
// In CI, install it first:
//
//   npm i -D playwright && npx playwright install --with-deps chromium
//   node tools/panel-verify/run.mjs
//
// Locally, point it at an existing install:
//
//   PLAYWRIGHT_PATH=/path/to/playwright CHROME_PATH=/path/to/chrome \
//     node tools/panel-verify/run.mjs
import { spawn } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PORT = process.env.PANEL_PORT || "4599";
const OUT = process.env.PANEL_SHOTS || path.resolve("panel-verify-shots");

// Fail fast with a clear message if Playwright cannot be resolved.
try {
  const { loadPlaywright } = await import("./lib.mjs");
  loadPlaywright();
} catch (e) {
  console.error(e.message);
  process.exit(2);
}

const server = spawn(process.execPath, [path.join(HERE, "serve.mjs"), "", PORT], {
  stdio: ["ignore", "pipe", "pipe"],
});

// Wait for the server to accept connections, surfacing its own error output if
// it cannot (most often the port is already taken by a stray instance).
let serverStderr = "";
server.stderr.on("data", (d) => {
  serverStderr += d.toString();
});

await new Promise((resolve, reject) => {
  const timer = setTimeout(() => reject(new Error("mock server did not start in 10s")), 10000);
  server.stdout.on("data", (d) => {
    if (d.toString().includes("mock server on")) {
      clearTimeout(timer);
      resolve();
    }
  });
  server.on("exit", (code) => {
    clearTimeout(timer);
    const hint = /EADDRINUSE/.test(serverStderr)
      ? `port ${PORT} is already in use — stop the other server or set PANEL_PORT`
      : serverStderr.trim() || "no output";
    reject(new Error(`mock server exited with ${code}: ${hint}`));
  });
});

const suites = ["structure.mjs", "contrast.mjs", "functional.mjs"];
const results = [];

for (const suite of suites) {
  const name = suite.replace(".mjs", "");
  process.stdout.write(`\n──────── ${name} ────────\n`);
  const code = await new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(HERE, suite), OUT], {
      stdio: "inherit",
      env: { ...process.env, PANEL_BASE: `http://127.0.0.1:${PORT}` },
    });
    child.on("exit", (c) => resolve(c ?? 1));
  });
  results.push({ name, ok: code === 0 });
}

server.kill();

console.log("\n════════ summary ════════");
for (const r of results) console.log(`  ${r.ok ? "✓" : "✗"} ${r.name}`);
console.log(`  screenshots -> ${OUT}`);

const failed = results.filter((r) => !r.ok);
if (failed.length) {
  console.error(`\n${failed.length} suite(s) failed: ${failed.map((f) => f.name).join(", ")}`);
  process.exit(1);
}
console.log("\nall panel verification suites passed");
