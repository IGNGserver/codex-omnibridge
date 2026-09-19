#!/usr/bin/env node
// Guards against packaging scripts diverging from the canonical install hooks.
//
// `scripts/build-deb.sh` used to inline its own copy of `DEBIAN/prerm`, which
// drifted from `installer/linux/deb/prerm`: the inline version matched processes
// with a substring `pkill -f '/usr/bin/codex-mp'` (harming unrelated processes)
// and only called `uninstall` when `SUDO_USER` was set, so a root-context removal
// (container, CI, `su`) silently left the user's Codex config rewritten.
//
// The rule: there must be exactly one prerm implementation, and the build script
// must install it rather than generate its own.
//
// Run: node scripts/check-packaging.js

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const failures = [];

const canonical = "installer/linux/deb/prerm";
const buildScript = "scripts/build-deb.sh";

if (!fs.existsSync(path.join(ROOT, canonical))) {
  failures.push(`${canonical} is missing; there must be one canonical deb prerm`);
}

if (fs.existsSync(path.join(ROOT, buildScript))) {
  const source = fs.readFileSync(path.join(ROOT, buildScript), "utf8");
  // A heredoc writing a prerm means a second implementation exists.
  if (/DEBIAN\/prerm"?\s*<<|cat\s*>\s*"?\$\{BUILD_DIR\}\/DEBIAN\/prerm/.test(source)) {
    failures.push(
      `${buildScript} generates its own DEBIAN/prerm; install ${canonical} instead so the ` +
        `two cannot drift`,
    );
  }
  if (!source.includes(canonical) && !source.includes("installer/linux/deb/prerm")) {
    failures.push(`${buildScript} does not reference the canonical ${canonical}`);
  }
}

// The canonical prerm must stop processes by exact name, not by substring.
const prermPath = path.join(ROOT, canonical);
if (fs.existsSync(prermPath)) {
  const prerm = fs.readFileSync(prermPath, "utf8");
  if (/pkill\s+-f\s/.test(prerm)) {
    failures.push(
      `${canonical} uses 'pkill -f' (substring match), which can kill unrelated ` +
        `processes; use 'pkill -x codex-mp'`,
    );
  }
  if (!/pkill\s+-x\s+codex-mp/.test(prerm)) {
    failures.push(`${canonical} must stop the router with 'pkill -x codex-mp'`);
  }
}

if (failures.length > 0) {
  console.error("packaging check FAILED:");
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log("packaging check: OK (one canonical deb prerm, exact process matching)");
