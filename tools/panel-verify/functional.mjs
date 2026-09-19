// Functional regression walkthrough for the panel.
//
// Drives every user-facing action against the mock backend and asserts both
// that the right request was sent and that the UI reported the right outcome.
// This is the suite that catches "the button renders but no longer does
// anything", which a screenshot-based check cannot see.
//
// Run: node tools/panel-verify/functional.mjs
import { launch, gotoPanel, report } from "./lib.mjs";

const problems = [];
const notes = [];
const results = [];

const browser = await launch();
const ctx = await browser.newContext({ viewport: { width: 1220, height: 950 } });
const page = await ctx.newPage();

const calls = [];
page.on("request", (r) => {
  if (r.url().includes("/api/")) calls.push(`${r.method()} ${new URL(r.url()).pathname}`);
});
page.on("pageerror", (e) => problems.push(`pageerror: ${e.message}`));
page.on("console", (m) => {
  if (m.type() === "error" && !/frame-ancestors/.test(m.text())) problems.push(`console: ${m.text()}`);
});
page.on("response", (r) => {
  if (r.status() >= 500) problems.push(`HTTP ${r.status()} ${r.url()}`);
});

await gotoPanel(page);
await page.waitForTimeout(400);

// Run a step, recording which API calls it produced.
async function step(name, fn, expect = []) {
  const before = calls.length;
  try {
    await fn();
    const made = calls.slice(before);
    for (const want of expect) {
      if (!made.some((c) => c.includes(want))) {
        throw new Error(`expected a call to ${want}, got: ${made.join(", ") || "none"}`);
      }
    }
    results.push(`✓ ${name}`);
  } catch (e) {
    problems.push(`${name}: ${e.message}`);
    results.push(`✗ ${name}`);
  }
}

const snackbarActive = () =>
  page.evaluate(() => document.querySelector("#m3-snackbar")?.classList.contains("active"));
const view = (id) => page.click(`.m3-nav-item[data-target="${id}"]`).then(() => page.waitForTimeout(650));

// ------------------------------------------------------------- overview -----
await step("router status rendered", async () => {
  const t = await page.textContent("#router-state-text");
  if (!t.includes("正常")) throw new Error(`chip reads "${t}"`);
});

await step("sync to Codex", async () => {
  await page.click("#topbar-sync-btn");
  await page.waitForTimeout(900);
  if (!(await snackbarActive())) throw new Error("no snackbar shown");
}, ["/api/v1/catalog/sync"]);

await step("refresh all data", async () => {
  await page.click("#overview-refresh-btn");
  await page.waitForTimeout(1400);
}, ["/api/v1/router/status", "/api/v1/accounts", "/api/v1/providers"]);

await step("check active account", async () => {
  await page.click("#overview-check-account-btn");
  await page.waitForTimeout(1000);
}, ["/api/v1/accounts/active"]);

await step("restart Codex", async () => {
  await page.click("#overview-restart-codex-btn");
  await page.waitForTimeout(1000);
}, ["/api/v1/accounts/restart-codex"]);

// ------------------------------------------------------------- accounts -----
await step("open accounts view", () => view("view-accounts"));

await step("account cards rendered", async () => {
  const n = await page.evaluate(() => document.querySelectorAll(".m3-account-card").length);
  if (n < 3) throw new Error(`only ${n} cards`);
});

await step("refresh one account usage", async () => {
  await page.click(".check-usage-acc-btn");
  await page.waitForTimeout(1100);
}, ["/usage"]);

await step("rename account via prompt", async () => {
  await page.click(".rename-acc-btn");
  await page.waitForTimeout(500);
  const open = await page.evaluate(() => document.querySelector("#prompt-dialog").open);
  if (!open) throw new Error("prompt dialog did not open");
  await page.fill("#prompt-dialog-input", "重命名测试");
  await page.click("#prompt-dialog-confirm");
  await page.waitForTimeout(1000);
}, ["/api/v1/accounts/rename"]);

await step("prompt cancel leaves data unchanged", async () => {
  await page.click(".rename-acc-btn");
  await page.waitForTimeout(500);
  await page.click("#prompt-dialog-cancel");
  await page.waitForTimeout(500);
  const open = await page.evaluate(() => document.querySelector("#prompt-dialog").open);
  if (open) throw new Error("prompt did not close on cancel");
});

await step("switch account", async () => {
  await page.click(".switch-acc-btn");
  await page.waitForTimeout(1200);
}, ["/api/v1/accounts/switch"]);

await step("batch refresh usage", async () => {
  await page.click("#refresh-all-accounts-usage-btn");
  await page.waitForTimeout(2500);
}, ["/usage"]);

await step("import dialog rejects malformed JSON", async () => {
  await page.click("#open-import-modal-action-btn");
  await page.waitForTimeout(500);
  await page.fill("textarea[name='json_content']", "{not json}");
  await page.click("#m3-import-account-form button[type=submit]");
  await page.waitForTimeout(600);
  const err = await page.textContent("#import-dialog-error");
  if (!err.includes("JSON")) throw new Error(`no JSON validation error, got "${err}"`);
  const flagged = await page.evaluate(() =>
    document.querySelector("#import-json-field").classList.contains("m3-text-field--error"),
  );
  if (!flagged) throw new Error("field not marked as errored");
});

await step("import account accepts valid JSON", async () => {
  await page.fill("textarea[name='json_content']", '{"tokens":{"access_token":"a","id_token":"b"}}');
  await page.click("#m3-import-account-form button[type=submit]");
  await page.waitForTimeout(1000);
  const open = await page.evaluate(() => document.querySelector("#import-account-dialog").open);
  if (open) throw new Error("dialog stayed open after success");
}, ["/api/v1/accounts/import"]);

// ------------------------------------------------------------ providers -----
await step("open providers view", () => view("view-providers"));

await step("provider cards rendered", async () => {
  const n = await page.evaluate(() => document.querySelectorAll(".m3-provider-card").length);
  if (n < 3) throw new Error(`only ${n} providers`);
});

await step("discover panel expands", async () => {
  await page.click(".discover-toggle-btn");
  await page.waitForTimeout(600);
  const open = await page.evaluate(() => document.querySelector(".m3-discover")?.dataset.open);
  if (open !== "true") throw new Error("discover panel did not open");
  const expanded = await page.evaluate(() =>
    document.querySelector(".discover-toggle-btn").getAttribute("aria-expanded"),
  );
  if (expanded !== "true") throw new Error("aria-expanded not updated");
});

await step("scan upstream models", async () => {
  await page.click(".fetch-discover-btn");
  await page.waitForTimeout(1300);
  const n = await page.evaluate(() => document.querySelectorAll(".m3-discover__checkbox").length);
  if (n === 0) throw new Error("no discovered models listed");
}, ["/discover"]);

// Everything below is scoped to the provider card that was just scanned:
// `.import-selected-btn` exists once per card.
const CARD = ".m3-provider-card:has(.m3-discover[data-open='true'])";

await step("import selected models", async () => {
  const boxes = await page.$$(`${CARD} .m3-discover__checkbox`);
  if (!boxes.length) throw new Error("no discovered models in the open card");
  await boxes[0].check();
  await page.click(`${CARD} .import-selected-btn`);
  await page.waitForTimeout(1300);
}, ["/api/v1/models/import"]);

await step("import without a selection is refused", async () => {
  // Importing refreshes the provider list, which rebuilds every card and resets
  // the discover panels, so reopen and rescan before reaching for the button.
  await page.click(".m3-provider-card .discover-toggle-btn");
  await page.waitForTimeout(500);
  await page.click(`${CARD} .fetch-discover-btn`);
  await page.waitForTimeout(1400);

  // The invariant that matters is that nothing is sent. Reading the snackbar
  // text is unreliable here because messages queue by design and earlier steps
  // leave several still draining.
  const before = calls.length;
  await page.click(`${CARD} .import-selected-btn`);
  await page.waitForTimeout(900);
  const sent = calls.slice(before).filter((c) => c.includes("/api/v1/models/import"));
  if (sent.length) throw new Error(`import was sent with no selection: ${sent.join(", ")}`);
});

await step("toggle model enabled", async () => {
  await page.click(".toggle-model-switch");
  await page.waitForTimeout(1200);
}, ["/api/v1/models/enabled"]);

await step("add provider", async () => {
  await page.click("#open-add-provider-dialog-btn");
  await page.waitForTimeout(500);
  await page.fill("input[name='name']", "测试 Provider");
  await page.fill("input[name='base_url']", "https://test.example.com/v1");
  await page.click("#add-provider-submit");
  await page.waitForTimeout(1100);
  const open = await page.evaluate(() => document.querySelector("#add-provider-dialog").open);
  if (open) throw new Error("dialog stayed open");
}, ["/api/v1/providers/add"]);

await step("edit provider prefills the form", async () => {
  await page.click(".edit-provider-btn");
  await page.waitForTimeout(700);
  const title = await page.textContent("#add-provider-title");
  if (!title.includes("编辑")) throw new Error(`title reads "${title}"`);
  const name = await page.inputValue("input[name='name']");
  if (!name) throw new Error("name field not prefilled");
  await page.click("#close-add-provider-btn");
  await page.waitForTimeout(400);
});

await step("edit provider saves", async () => {
  await page.click(".edit-provider-btn");
  await page.waitForTimeout(600);
  await page.fill("input[name='name']", "改名后的 Provider");
  await page.click("#add-provider-submit");
  await page.waitForTimeout(1100);
}, ["/api/v1/providers/edit"]);

await step("delete provider uses ONE confirm with the credential opt-in", async () => {
  await page.click(".delete-provider-btn");
  await page.waitForTimeout(600);
  const state = await page.evaluate(() => {
    const d = document.querySelector("#confirm-dialog");
    return {
      open: d.open,
      hasCheckbox: !document.querySelector("#confirm-dialog-checkbox-row").classList.contains("is-hidden"),
    };
  });
  if (!state.open) throw new Error("confirm dialog did not open");
  if (!state.hasCheckbox) throw new Error("credential opt-in checkbox missing");
  await page.click("#confirm-dialog-confirm");
  await page.waitForTimeout(1100);
  const reopened = await page.evaluate(() => document.querySelector("#confirm-dialog").open);
  if (reopened) throw new Error("a second confirm dialog appeared (should be folded into one)");
}, ["/api/v1/providers/remove"]);

await step("add model manually", async () => {
  await page.click("#open-add-model-dialog-btn");
  await page.waitForTimeout(500);
  await page.fill("input[name='provider_id']", "newapi-primary");
  await page.fill("input[name='upstream_model_id']", "test-model");
  await page.fill("input[name='display_name']", "Test Model");
  await page.click("#m3-model-form button[type=submit]");
  await page.waitForTimeout(1100);
}, ["/api/v1/models/add"]);

// ------------------------------------------------------------- settings -----
await step("open settings view", () => view("view-settings"));

await step("theme segmented control", async () => {
  await page.click('#settings-theme-group [data-theme-value="light"]');
  await page.waitForTimeout(400);
  const t = await page.evaluate(() => document.documentElement.dataset.theme);
  if (t !== "light") throw new Error(`theme=${t}`);
  await page.click('#settings-theme-group [data-theme-value="system"]');
  await page.waitForTimeout(300);
});

await step("contrast segmented control", async () => {
  await page.click('#settings-contrast-group [data-contrast-value="high"]');
  await page.waitForTimeout(300);
  const c = await page.evaluate(() => document.documentElement.dataset.contrast);
  if (c !== "high") throw new Error(`contrast=${c}`);
  await page.click('#settings-contrast-group [data-contrast-value="standard"]');
  await page.waitForTimeout(300);
});

await step("motion segmented control", async () => {
  await page.click('#settings-motion-group [data-motion-value="reduced"]');
  await page.waitForTimeout(300);
  const m = await page.evaluate(() => document.documentElement.dataset.motion);
  if (m !== "reduced") throw new Error(`motion=${m}`);
  await page.click('#settings-motion-group [data-motion-value="full"]');
  await page.waitForTimeout(300);
});

await step("preferences survive a reload", async () => {
  await page.click('#settings-theme-group [data-theme-value="dark"]');
  await page.click('#settings-contrast-group [data-contrast-value="high"]');
  await page.waitForTimeout(400);
  await page.reload({ waitUntil: "networkidle" });
  await page.waitForTimeout(900);
  const s = await page.evaluate(() => ({ ...document.documentElement.dataset }));
  if (s.theme !== "dark" || s.contrast !== "high") {
    throw new Error(`after reload: ${JSON.stringify(s)}`);
  }
  await page.click('#settings-theme-group [data-theme-value="system"]');
  await page.click('#settings-contrast-group [data-contrast-value="standard"]');
  await page.waitForTimeout(300);
});

await step("password counter updates", async () => {
  await page.fill("#settings-web-password", "short");
  await page.waitForTimeout(300);
  const c = await page.textContent("#settings-password-counter");
  if (!c.includes("5")) throw new Error(`counter reads "${c}"`);
  await page.fill("#settings-web-password", "");
});

await step("security form submits", async () => {
  await page.click("#settings-security-form button[type=submit]");
  await page.waitForTimeout(1100);
}, ["/api/v1/security/update"]);

await step("desktop install", async () => {
  await page.click("#settings-desktop-install-btn");
  await page.waitForTimeout(1000);
}, ["/api/v1/desktop/install"]);

await step("desktop restore confirms first", async () => {
  await page.click("#settings-desktop-restore-btn");
  await page.waitForTimeout(600);
  const open = await page.evaluate(() => document.querySelector("#confirm-dialog").open);
  if (!open) throw new Error("no confirmation before restore");
  await page.click("#confirm-dialog-confirm");
  await page.waitForTimeout(1000);
}, ["/api/v1/desktop/restore"]);

await step("theme menu opens, selects, and closes", async () => {
  await page.click("#theme-menu-btn");
  await page.waitForTimeout(400);
  const opened = await page.evaluate(() => !document.querySelector("#theme-menu").hidden);
  if (!opened) throw new Error("menu did not open");
  await page.click('#theme-menu [data-theme-value="dark"]');
  await page.waitForTimeout(400);
  const closed = await page.evaluate(() => document.querySelector("#theme-menu").hidden);
  if (!closed) throw new Error("menu did not close after selecting");
  await page.click("#theme-menu-btn");
  await page.waitForTimeout(300);
  await page.click('#theme-menu [data-theme-value="system"]');
  await page.waitForTimeout(300);
});

await step("hash routing restores the view on load", async () => {
  await page.goto(`${(await import("./lib.mjs")).BASE}/#view-providers`, { waitUntil: "networkidle" });
  await page.waitForTimeout(1000);
  const active = await page.evaluate(() => document.querySelector(".m3-view-section.active")?.id);
  if (active !== "view-providers") throw new Error(`active view is ${active}`);
});

await step("hash routing reacts to a fragment-only change", async () => {
  await page.evaluate(() => {
    window.location.hash = "#view-settings";
  });
  await page.waitForTimeout(700);
  const active = await page.evaluate(() => document.querySelector(".m3-view-section.active")?.id);
  if (active !== "view-settings") throw new Error(`active view is ${active}`);
});

await step("logout clears the token and prompts", async () => {
  await page.evaluate(() => {
    localStorage.setItem("codex_mp_token", "stale-token");
    document.querySelector("#logout-btn").classList.remove("is-hidden");
  });
  await page.click("#logout-btn");
  await page.waitForTimeout(800);
  const state = await page.evaluate(() => ({
    stored: localStorage.getItem("codex_mp_token"),
    loginOpen: document.querySelector("#login-dialog").open,
  }));
  if (state.stored) throw new Error("token survived logout");
  if (!state.loginOpen) throw new Error("login dialog did not appear");
});

await browser.close();

const passed = results.filter((r) => r.startsWith("✓")).length;
console.log(results.join("\n"));
console.log(`\n${passed}/${results.length} steps passed`);
process.exit(report("functional", problems, notes));
