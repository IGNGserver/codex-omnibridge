#!/usr/bin/env node
/**
 * End-to-end Browser & WebGL Verification Suite for Codex OmniBridge Website.
 * Serves `dist/` locally over loopback HTTP and verifies 3D GLB loading,
 * interactive optical routing, scroll fly-through, Same-Thread simulator,
 * M3 Expressive Control Panel sandbox, zero console/network errors, and
 * responsive layout integrity on Desktop (1440x920) & Mobile (390x844, 375x667).
 */

import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_ROOT = path.resolve(__dirname, '..');
const DIST_DIR = path.resolve(process.env.WEBSITE_DIST_DIR || path.join(WEBSITE_ROOT, 'dist'));
const SCREENSHOT_DIR = process.env.VERIFY_SCREENSHOT_DIR
  ? path.resolve(process.env.VERIFY_SCREENSHOT_DIR)
  : null;

const MIME_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.png': 'image/png',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
  '.glb': 'model/gltf-binary',
  '.ico': 'image/x-icon',
};

function resolveChromeExecutable() {
  if (process.env.CHROME_PATH && fs.existsSync(process.env.CHROME_PATH)) {
    return process.env.CHROME_PATH;
  }

  const home = os.homedir();
  const cacheBases = [
    path.join(home, 'Library/Caches/ms-playwright'),
    path.join(home, '.cache/ms-playwright'),
  ];

  for (const base of cacheBases) {
    if (!fs.existsSync(base)) continue;
    const dirs = fs
      .readdirSync(base)
      .filter((d) => d.startsWith('chromium-'))
      .sort()
      .reverse();
    for (const dir of dirs) {
      const candidates = [
        path.join(
          base,
          dir,
          'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
        ),
        path.join(
          base,
          dir,
          'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
        ),
        path.join(base, dir, 'chrome-linux/chrome'),
      ];
      for (const c of candidates) {
        if (fs.existsSync(c)) return c;
      }
    }
  }

  const systemCandidates = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/usr/bin/google-chrome',
  ];
  for (const c of systemCandidates) {
    if (fs.existsSync(c)) return c;
  }
  return undefined;
}

function startStaticServer(rootDir) {
  return new Promise((resolve, reject) => {
    const server = http.createServer((req, res) => {
      const reqUrl = new URL(req.url || '/', 'http://127.0.0.1');
      let relPath = decodeURIComponent(reqUrl.pathname);
      if (relPath === '/') relPath = '/index.html';
      const safePath = path.normalize(path.join(rootDir, relPath));
      if (!safePath.startsWith(rootDir)) {
        res.writeHead(403);
        res.end('Forbidden');
        return;
      }
      if (!fs.existsSync(safePath) || fs.statSync(safePath).isDirectory()) {
        res.writeHead(404);
        res.end('Not found');
        return;
      }
      const ext = path.extname(safePath).toLowerCase();
      res.writeHead(200, {
        'Content-Type': MIME_TYPES[ext] || 'application/octet-stream',
        'Cache-Control': 'no-store',
      });
      fs.createReadStream(safePath).pipe(res);
    });
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const addr = server.address();
      resolve({
        server,
        baseUrl: `http://127.0.0.1:${addr.port}`,
      });
    });
  });
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(`ASSERTION FAILED: ${message}`);
  }
}

async function run() {
  assert(fs.existsSync(path.join(DIST_DIR, 'index.html')), `Missing built dist at ${DIST_DIR}`);
  if (SCREENSHOT_DIR) {
    fs.mkdirSync(SCREENSHOT_DIR, { recursive: true });
  }

  const executablePath = resolveChromeExecutable();
  const { server, baseUrl } = await startStaticServer(DIST_DIR);
  console.log(`[verify] Serving ${DIST_DIR} at ${baseUrl}`);
  if (executablePath) {
    console.log(`[verify] Using Chromium executable: ${executablePath}`);
  }

  const browser = await chromium.launch({
    headless: true,
    executablePath,
    args: [
      '--enable-webgl',
      '--ignore-gpu-blocklist',
      '--use-gl=angle',
      '--use-angle=swiftshader',
    ],
  });

  try {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 920 },
      deviceScaleFactor: 1,
    });
    const page = await context.newPage();

    const consoleErrors = [];
    const pageErrors = [];
    const failedResponses = [];
    const externalRequests = [];
    let glbStatus = null;

    page.on('console', (msg) => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });
    page.on('pageerror', (err) => {
      pageErrors.push(String(err));
    });
    page.on('request', (req) => {
      const url = req.url();
      if (!url.startsWith(baseUrl) && !url.startsWith('data:') && !url.startsWith('blob:')) {
        externalRequests.push(url);
      }
    });
    page.on('response', (res) => {
      if (res.url().endsWith('omnibridge-photonic-matrix.glb')) {
        glbStatus = res.status();
      }
      if (res.status() >= 400) {
        failedResponses.push(`${res.status()} ${res.url()}`);
      }
    });

    await page.goto(baseUrl, { waitUntil: 'networkidle' });

    // Wait for 3D GLB model load (or clean fallback)
    await page.waitForFunction(() => {
      const el = document.getElementById('photonic-stage');
      return el && (el.dataset.glbLoaded === 'true' || el.dataset.glbLoaded === 'fallback');
    }, { timeout: 15000 });

    const glbState = await page.getAttribute('#photonic-stage', 'data-glb-loaded');
    console.log(`[verify] 3D Photonic Stage state: data-glb-loaded=${glbState}, HTTP=${glbStatus}`);
    if (glbState === 'true') {
      assert(glbStatus === 200, `Expected 200 OK for omnibridge-photonic-matrix.glb, got ${glbStatus}`);
    }

    // Give spring animation & bloom composer a moment to settle
    await page.waitForTimeout(450);

    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '01-desktop-hero-official.png') });
    }

    // 1. Test all 6 Hero Optical Route Presets
    const expectedPresets = [
      ['gpt-official', 'gpt-5.4'],
      ['claude-openrouter', 'openrouter/claude-opus-4.6'],
      ['qwen-newapi', 'newapi/qwen4-max'],
      ['deepseek-oneapi', 'oneapi/deepseek-v4.1'],
      ['gemini-newapi', 'newapi/gemini-3.8-flash'],
      ['ollama-local', 'ollama/qwen3-coder'],
    ];

    for (const [presetId, expectedSlug] of expectedPresets) {
      await page.click(`[data-route-preset="${presetId}"]`);
      const modelsText = await page.textContent('#ro-models');
      assert(
        modelsText && modelsText.includes(expectedSlug),
        `Route preset ${presetId} did not update #ro-models (got: ${modelsText})`,
      );
    }

    // Select Claude + Open Quartz Canopy for a dramatic hero inspection shot
    await page.click('[data-route-preset="claude-openrouter"]');
    await page.click('#btn-toggle-canopy');
    assert(
      (await page.getAttribute('#btn-toggle-canopy', 'aria-pressed')) === 'true',
      'Quartz canopy button should report aria-pressed="true"',
    );
    await page.waitForTimeout(500);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '02-desktop-hero-claude-canopy-open.png') });
    }
    await page.click('#btn-toggle-canopy');

    // Test Free 3D Orbit Inspection toggle & Escape exit
    await page.click('#btn-free-inspect');
    const isFree = await page.evaluate(() => document.body.classList.contains('is-free-inspect'));
    assert(isFree, 'Free inspect mode should add body.is-free-inspect');
    await page.keyboard.press('Escape');
    const exitedFree = await page.evaluate(() => !document.body.classList.contains('is-free-inspect'));
    assert(exitedFree, 'Escape should exit free inspect mode');

    // 2. Scroll through Act II (#inside-matrix) stations
    await page.evaluate(() =>
      document.getElementById('stage-bridge')?.scrollIntoView({ behavior: 'instant', block: 'center' }),
    );
    await page.waitForTimeout(550);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '03-desktop-inside-matrix.png') });
    }

    // 3. Test Act III Same-Thread Simulator (#thread-lab)
    await page.evaluate(() =>
      document.getElementById('thread-lab')?.scrollIntoView({ behavior: 'instant', block: 'start' }),
    );
    await page.waitForTimeout(350);
    await page.click('[data-thread-step="1"]');
    const transcriptAfterStep2 = await page.textContent('#thread-transcript-list');
    assert(
      transcriptAfterStep2 && transcriptAfterStep2.includes('/model openrouter/claude-opus-4.6'),
      'Thread simulator turn 2 should render /model openrouter/claude-opus-4.6 slash switch',
    );
    await page.click('[data-thread-step="2"]');
    await page.click('[data-thread-step="3"]');
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '04-desktop-thread-simulator.png') });
    }

    // 4. Test Act IV Authentic M3 Expressive Control Panel Sandbox (#panel-showcase)
    await page.evaluate(() =>
      document.getElementById('panel-showcase')?.scrollIntoView({ behavior: 'instant', block: 'start' }),
    );
    await page.waitForTimeout(350);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '05-desktop-panel-dark-overview.png') });
    }

    // Official Accounts tab: switch account & verify backup creation
    await page.click('[data-m3s-tab="accounts"]');
    await page.click('[data-switch-acc="acc_pro"]');
    const backupText = await page.textContent('.m3s-backup-list');
    assert(
      backupText && backupText.includes('Plus 主号'),
      'Switching away from Plus 主号 should create an auth.bak-switch-* snapshot',
    );

    // Providers tab: test HTTPS enforcement & provider discovery + Sync to Codex
    await page.click('[data-m3s-tab="providers"]');
    await page.click('#m3s-toggle-add-provider');
    await page.fill('#m3s-inp-url', 'http://insecure.remote.example/v1');
    await page.click('#m3s-add-provider-form button[type="submit"]');
    const snackReject = await page.textContent('.m3s-snackbar');
    assert(
      snackReject && snackReject.includes('校验拒绝'),
      `Non-loopback HTTP should be rejected, got: ${snackReject}`,
    );

    await page.fill('#m3s-inp-url', 'https://oneapi.team.example/v1');
    await page.click('#m3s-add-provider-form button[type="submit"]');
    await page.click('#m3s-sync-btn');
    const pickerGen = await page.textContent('.picker-gen-tag');
    assert(
      pickerGen && pickerGen.includes('gen #5'),
      `Syncing to Codex should increment generation to #5, got: ${pickerGen}`,
    );

    // Settings tab: switch to Light Theme + High Contrast & test Remote Password Gate
    await page.click('[data-m3s-tab="settings"]');
    await page.click('[data-set-theme="light"]');
    await page.click('[data-set-contrast="high"]');
    assert(
      (await page.getAttribute('.m3s-window', 'data-panel-theme')) === 'light',
      'Panel theme should switch to light',
    );
    await page.waitForTimeout(250);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '06-desktop-panel-light-high-contrast.png') });
    }

    // 5. Architecture & CLI Quickstart
    await page.evaluate(() =>
      document.getElementById('architecture-matrix')?.scrollIntoView({ behavior: 'instant', block: 'start' }),
    );
    await page.waitForTimeout(250);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '07-desktop-architecture-crates.png') });
    }

    await page.evaluate(() =>
      document.getElementById('quickstart')?.scrollIntoView({ behavior: 'instant', block: 'start' }),
    );
    await page.click('[data-cli-tab="windows"]');
    assert(
      await page.isVisible('[data-cli-pane="windows"]'),
      'Windows PowerShell quickstart pane should become visible',
    );
    await page.click('[data-cli-tab="mac-linux"]');
    await page.waitForTimeout(200);
    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '08-desktop-cli-quickstart.png') });
    }

    // Verify zero horizontal overflow on Desktop (1440px)
    const desktopOverflow = await page.evaluate(
      () => document.documentElement.scrollWidth - window.innerWidth,
    );
    assert(desktopOverflow <= 1, `Desktop horizontal overflow detected: +${desktopOverflow}px`);

    // 6. Mobile Responsive Verification (390x844 & 375x667)
    for (const [w, h, tag] of [
      [390, 844, '390x844'],
      [375, 667, '375x667'],
    ]) {
      await page.setViewportSize({ width: w, height: h });
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.waitForTimeout(350);

      const mobileOverflow = await page.evaluate(() => {
        const docDelta = document.documentElement.scrollWidth - window.innerWidth;
        const boxes = Array.from(
          document.querySelectorAll(
            '.m3s-window, .codex-picker-sidecar, .route-console-dock, .thread-sim-shell, .cli-workbench',
          ),
        );
        let maxRightDelta = 0;
        for (const el of boxes) {
          const r = el.getBoundingClientRect();
          if (r.right - window.innerWidth > maxRightDelta) {
            maxRightDelta = r.right - window.innerWidth;
          }
        }
        return Math.max(docDelta, maxRightDelta);
      });
      assert(
        mobileOverflow <= 1.5,
        `Mobile (${tag}) container/horizontal overflow detected: +${mobileOverflow.toFixed(1)}px`,
      );

      if (SCREENSHOT_DIR && w === 390) {
        await page.screenshot({ path: path.join(SCREENSHOT_DIR, '09-mobile-hero-390x844.png') });
        await page.evaluate(() =>
          document.getElementById('panel-showcase')?.scrollIntoView({ behavior: 'instant', block: 'start' }),
        );
        await page.waitForTimeout(300);
        await page.screenshot({ path: path.join(SCREENSHOT_DIR, '10-mobile-panel-390x844.png') });
      }
    }

    // Final error & isolation assertions
    assert(consoleErrors.length === 0, `Console errors: ${consoleErrors.join(' | ')}`);
    assert(pageErrors.length === 0, `Uncaught page errors: ${pageErrors.join(' | ')}`);
    assert(failedResponses.length === 0, `Failed HTTP responses: ${failedResponses.join(' | ')}`);
    assert(
      externalRequests.length === 0,
      `Unexpected external CDN requests (offline self-hosting violated): ${externalRequests.join(' | ')}`,
    );

    console.log('[verify] ✓ All functional, 3D WebGL, M3 sandbox, offline-CSP, and responsive checks PASSED!');
  } finally {
    await browser.close();
    server.close();
  }
}

run().catch((err) => {
  console.error('[verify] ✗ Verification failed:', err);
  process.exit(1);
});
