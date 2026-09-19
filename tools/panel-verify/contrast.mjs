// Rendered-contrast audit for the panel.
//
// Checks WCAG AA against what the browser actually paints, not against the
// token table. That distinction matters: two token values can each look fine
// while their *composite* (e.g. a 12% container tint under 38% text alpha)
// lands at 1:1. Every text-bearing node in every view is sampled in both
// colour schemes.
//
// Run: node tools/panel-verify/contrast.mjs
import { launch, gotoPanel, showView, parseRgb, contrastRatio, report } from "./lib.mjs";

const VIEWS = ["view-overview", "view-accounts", "view-providers", "view-settings"];
const problems = [];
const notes = [];

const browser = await launch();

for (const scheme of ["light", "dark"]) {
  const ctx = await browser.newContext({ viewport: { width: 1220, height: 900 }, colorScheme: scheme });
  const page = await ctx.newPage();
  await gotoPanel(page);

  const samples = [];
  for (const view of VIEWS) {
    await showView(page, view);
    const part = await page.evaluate(() => {
      const out = [];
      // Walk up for the first painted background, since most text sits on a
      // transparent element inside a tinted container.
      const bgOf = (el) => {
        let node = el;
        while (node) {
          const bg = getComputedStyle(node).backgroundColor;
          const alpha = (bg.match(/[\d.]+/g) || [])[3];
          if (bg && bg !== "rgba(0, 0, 0, 0)" && alpha !== "0") return bg;
          node = node.parentElement;
        }
        return "rgb(0,0,0)";
      };
      document.querySelectorAll(".m3-view-section.active *").forEach((el) => {
        if (!el.childNodes.length) return;
        const text = [...el.childNodes]
          .filter((n) => n.nodeType === 3)
          .map((n) => n.textContent.trim())
          .join("");
        if (!text) return;
        const cs = getComputedStyle(el);
        if (cs.display === "none" || cs.visibility === "hidden") return;
        if (parseFloat(cs.opacity) === 0) return;
        const fontSize = parseFloat(cs.fontSize);
        const weight = parseInt(cs.fontWeight, 10) || 400;
        out.push({
          text: text.slice(0, 24),
          fg: cs.color,
          bg: bgOf(el),
          // WCAG "large text": >=24px, or >=18.66px when bold.
          large: fontSize >= 24 || (fontSize >= 18.66 && weight >= 700),
          fontSize,
          cls: (el.className || "").toString().slice(0, 40),
        });
      });
      return out;
    });
    samples.push(...part);
  }

  for (const s of samples) {
    const ratio = contrastRatio(parseRgb(s.fg), parseRgb(s.bg));
    const required = s.large ? 3.0 : 4.5;
    if (ratio < required) {
      problems.push(
        `${scheme}: ${ratio.toFixed(2)} < ${required}  "${s.text}" [${s.cls}] ${s.fontSize}px`,
      );
    }
  }
  console.log(`${scheme}: ${samples.length} text nodes checked`);
  notes.push(`${scheme}: ${samples.length} text nodes`);
  await ctx.close();
}

await browser.close();
process.exit(report("contrast", problems, notes));
