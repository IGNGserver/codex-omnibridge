// Structural + accessibility verification for the panel.
//
// Walks every view at every window size class, every dialog, both colour
// schemes, both motion preferences, and asserts machine-checkable invariants:
// no console/CSP errors, no horizontal overflow, dialogs are real modals with
// accessible names that close on Escape, every control has an accessible name,
// every focusable element has a visible focus ring, and reduced motion leaves
// nothing stuck invisible.
//
// Run: node tools/panel-verify/structure.mjs [outDir]
import fs from "node:fs";
import path from "node:path";
import { BASE, launch, watch, gotoPanel, showView, report } from "./lib.mjs";

const OUT = process.argv[2] || path.resolve("panel-verify-shots");
fs.mkdirSync(OUT, { recursive: true });

const WIDTHS = [
  { name: "compact-380", w: 380, h: 800 },
  { name: "medium-600", w: 600, h: 900 },
  { name: "expanded-900", w: 900, h: 900 },
  { name: "large-1220", w: 1220, h: 840 },
  { name: "xl-1600", w: 1600, h: 900 },
];

const VIEWS = ["view-overview", "view-accounts", "view-providers", "view-settings"];

const problems = [];
const notes = [];
const fail = (m) => problems.push(m);
const note = (m) => notes.push(m);

// Each view's navigation shape, as required by the M3 window size classes.
const EXPECTED_NAV = {
  "compact-380": { orientation: "row", position: "fixed" },
  "medium-600": { orientation: "column", position: "static" },
  "expanded-900": { orientation: "column", position: "static" },
  "large-1220": { orientation: "column", position: "static" },
  "xl-1600": { orientation: "column", position: "static" },
};

const browser = await launch();

// --------------------------------------------------------- views x widths ---
for (const vp of WIDTHS) {
  const ctx = await browser.newContext({ viewport: { width: vp.w, height: vp.h } });
  const page = await ctx.newPage();
  const where = vp.name;
  watch(page, where, fail, note);
  await gotoPanel(page);

  for (const view of VIEWS) {
    await showView(page, view);
    await page.screenshot({ path: path.join(OUT, `${vp.name}--${view.replace("view-", "")}.png`), fullPage: true });

    const r = await page.evaluate(() => {
      const de = document.documentElement;
      return { scrollW: de.scrollWidth, clientW: de.clientWidth };
    });
    if (r.scrollW > r.clientW + 1) {
      fail(`${where}/${view}: horizontal overflow ${r.scrollW} > ${r.clientW}`);
    }
  }

  // Navigation must change shape with the window size class.
  const nav = await page.evaluate(() => {
    const cs = getComputedStyle(document.querySelector("#app-navigation"));
    return { direction: cs.flexDirection, position: cs.position };
  });
  const want = EXPECTED_NAV[vp.name];
  if (want && nav.direction !== want.orientation) {
    fail(`${where}: nav flex-direction is ${nav.direction}, expected ${want.orientation}`);
  }
  if (want && nav.position !== want.position) {
    fail(`${where}: nav position is ${nav.position}, expected ${want.position}`);
  }

  // Content must not hide behind a fixed bottom navigation bar.
  if (vp.w < 600) {
    // `html { scroll-behavior: smooth }` makes `scrollTo` animate. A fixed
    // timeout measured the page mid-scroll on slower machines (Chromium headless
    // shell on CI reported a 40px overlap that never existed), so wait until the
    // scroll position actually stops changing instead of guessing a delay.
    await page.evaluate(() => {
      const scroller = document.querySelector("#main-content");
      scroller?.scrollTo(0, scroller.scrollHeight);
    });
    // Poll `evaluate` rather than `waitForFunction`: the panel's CSP is
    // `script-src 'self'`, and `waitForFunction` serialises its predicate into a
    // string that is then evaluated as script, which the CSP blocks.
    {
      let previous = -1;
      for (let attempt = 0; attempt < 50; attempt += 1) {
        const y = await page.evaluate(() => Math.round(document.querySelector("#main-content")?.scrollTop || 0));
        if (y === previous) break;
        previous = y;
        await page.waitForTimeout(100);
      }
    }
    const overlap = await page.evaluate(() => {
      const navTop = document.querySelector("#app-navigation").getBoundingClientRect().top;
      const cards = [...document.querySelectorAll(".m3-view-section.active .m3-card")];
      const last = cards.at(-1)?.getBoundingClientRect();
      return last ? Math.round(last.bottom - navTop) : 0;
    });
    if (overlap > 0) fail(`${where}: last card overlaps the bottom nav by ${overlap}px`);
  }

  await ctx.close();
}

// --------------------------------------------------------------- dialogs ---
{
  const ctx = await browser.newContext({ viewport: { width: 1220, height: 950 } });
  const page = await ctx.newPage();
  watch(page, "dialogs", fail, note);
  await gotoPanel(page);

  const DIALOGS = [
    { view: "view-providers", open: "#open-add-provider-dialog-btn", id: "#add-provider-dialog", name: "add-provider" },
    { view: "view-providers", open: "#open-add-model-dialog-btn", id: "#add-model-dialog", name: "add-model" },
    { view: "view-providers", open: ".edit-model-btn", id: "#edit-model-dialog", name: "edit-model" },
    { view: "view-accounts", open: "#open-import-modal-action-btn", id: "#import-account-dialog", name: "import-account" },
  ];

  for (const d of DIALOGS) {
    await showView(page, d.view);
    await page.click(d.open);
    await page.waitForTimeout(600);

    const a11y = await page.evaluate((sel) => {
      const el = document.querySelector(sel);
      if (!el) return { found: false };
      const labelledBy = el.getAttribute("aria-labelledby");
      const label = labelledBy ? document.getElementById(labelledBy) : null;
      return {
        found: true,
        isDialog: el.tagName === "DIALOG",
        open: el.open === true,
        modal: el.matches(":modal"),
        accessibleName: (label?.textContent || "").trim(),
        labelledBy,
      };
    }, d.id);

    if (!a11y.found) fail(`dialogs/${d.name}: element missing`);
    else {
      if (!a11y.isDialog) fail(`dialogs/${d.name}: not a native <dialog>`);
      if (!a11y.open) fail(`dialogs/${d.name}: did not open`);
      if (!a11y.modal) fail(`dialogs/${d.name}: not opened as a modal`);
      if (!a11y.accessibleName) fail(`dialogs/${d.name}: no accessible name`);
    }

    await page.screenshot({ path: path.join(OUT, `dialog--${d.name}.png`) });

    // Escape must close (platform-provided for <dialog>).
    await page.keyboard.press("Escape");
    await page.waitForTimeout(400);
    const stillOpen = await page.evaluate((sel) => document.querySelector(sel)?.open === true, d.id);
    if (stillOpen) fail(`dialogs/${d.name}: does not close on Escape`);

    // Focus must return to whatever opened it.
    const restored = await page.evaluate((sel) => document.activeElement === document.querySelector(sel), d.open);
    if (!restored) note(`dialogs/${d.name}: focus not returned to the trigger`);
  }

  // The confirm dialog is JS-driven; trigger it from a provider delete.
  await showView(page, "view-providers");
  const del = await page.$(".delete-provider-btn");
  if (!del) fail("dialogs: no .delete-provider-btn rendered");
  else {
    await del.click();
    await page.waitForTimeout(600);
    const ok = await page.evaluate(() => {
      const el = document.querySelector("#confirm-dialog");
      return el?.open === true && el.matches(":modal");
    });
    if (!ok) fail("dialogs/confirm: did not open as a modal");
    await page.screenshot({ path: path.join(OUT, "dialog--confirm.png") });
    await page.keyboard.press("Escape");
    await page.waitForTimeout(350);
  }

  await ctx.close();
}

// ----------------------------------------------- theme / motion matrix ------
for (const scheme of ["dark", "light"]) {
  for (const motion of ["no-preference", "reduce"]) {
    const ctx = await browser.newContext({
      viewport: { width: 1220, height: 840 },
      colorScheme: scheme,
      reducedMotion: motion,
    });
    const page = await ctx.newPage();
    const where = `theme-${scheme}-${motion}`;
    watch(page, where, fail, note);
    await gotoPanel(page);
    await page.screenshot({ path: path.join(OUT, `${where}.png`), fullPage: true });

    if (motion === "reduce") {
      // Nothing may be left mid-animation and invisible.
      const hidden = await page.evaluate(() => {
        const bad = [];
        document.querySelectorAll(".m3-view-section.active *").forEach((el) => {
          const cs = getComputedStyle(el);
          if (cs.display === "none" || cs.visibility === "hidden") return;
          if (parseFloat(cs.opacity) < 0.99 && el.textContent.trim()) {
            bad.push(`${el.className}:${cs.opacity}`);
          }
        });
        return bad.slice(0, 5);
      });
      if (hidden.length) fail(`${where}: elements not fully visible: ${hidden.join(", ")}`);
    }
    await ctx.close();
  }
}

// ------------------------------------------------------------- keyboard -----
{
  const ctx = await browser.newContext({ viewport: { width: 1220, height: 900 } });
  const page = await ctx.newPage();
  watch(page, "keyboard", fail, note);
  await gotoPanel(page);

  // The first tab stop must be the skip link.
  await page.keyboard.press("Tab");
  const first = await page.evaluate(() => document.activeElement?.className || "");
  if (!first.includes("m3-skip-link")) fail(`keyboard: first tab stop is "${first}", expected the skip link`);

  const hasSkip = await page.evaluate(() => !!document.querySelector('a[href="#main-content"]'));
  if (!hasSkip) fail("keyboard: no skip-to-content link");

  // Every focus stop needs a visible ring.
  const stops = [];
  for (let i = 0; i < 16; i++) {
    await page.keyboard.press("Tab");
    const info = await page.evaluate(() => {
      const el = document.activeElement;
      if (!el || el === document.body) return null;
      const cs = getComputedStyle(el);
      return {
        tag: el.tagName.toLowerCase(),
        cls: (el.className || "").toString().slice(0, 50),
        ring:
          (cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) > 0) ||
          cs.boxShadow !== "none",
      };
    });
    if (info) stops.push(info);
  }
  const noRing = stops.filter((s) => !s.ring);
  if (noRing.length) fail(`keyboard: ${noRing.length}/${stops.length} focus stops have no visible ring: ${JSON.stringify(noRing.slice(0, 4))}`);

  // Controls must have accessible names (skipping hidden ones).
  const unnamed = await page.evaluate(() => {
    const bad = [];
    document.querySelectorAll("button, a[href], input, select, textarea").forEach((el) => {
      if (el.hidden || el.closest("[hidden]")) return;
      if (getComputedStyle(el).display === "none") return;
      const labelledBy = el.getAttribute("aria-labelledby");
      const name = (
        el.getAttribute("aria-label") ||
        (labelledBy && document.getElementById(labelledBy)?.textContent) ||
        el.textContent ||
        el.getAttribute("placeholder") ||
        el.getAttribute("title") ||
        ""
      ).trim();
      const hasLabelFor = el.id && document.querySelector(`label[for="${el.id}"]`);
      if (!name && !hasLabelFor && !el.closest("label")) {
        bad.push(`${el.tagName.toLowerCase()}#${el.id || ""}.${(el.className || "").toString().slice(0, 30)}`);
      }
    });
    return bad;
  });
  if (unnamed.length) fail(`keyboard: controls without accessible name: ${unnamed.join(", ")}`);

  // Nav must be arrow-key navigable and report the active view.
  await page.focus(".m3-nav-item.active");
  await page.keyboard.press("ArrowDown");
  const navState = await page.evaluate(() => ({
    focused: document.activeElement?.dataset?.target,
    view: document.querySelector(".m3-view-section.active")?.id,
    current: document.activeElement?.getAttribute("aria-current"),
  }));
  if (navState.focused !== navState.view) {
    fail(`keyboard: arrow-key nav focused ${navState.focused} but view is ${navState.view}`);
  }
  if (navState.current !== "page") fail("keyboard: active nav item lacks aria-current=page");

  await ctx.close();
}

await browser.close();
process.exit(report("structure", problems, notes));
