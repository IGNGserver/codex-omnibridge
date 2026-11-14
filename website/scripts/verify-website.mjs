import assert from 'node:assert/strict';
import fsSync from 'node:fs';
import fs from 'node:fs/promises';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright-core';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const WEBSITE_DIR = path.resolve(__dirname, '..');
const DIST_DIR = path.resolve(process.env.WEBSITE_DIST_DIR || path.join(WEBSITE_ROOT(), 'dist'));
const SCREENSHOT_DIR = process.env.VERIFY_SCREENSHOT_DIR
  ? path.resolve(process.env.VERIFY_SCREENSHOT_DIR)
  : null;

function WEBSITE_ROOT() {
  return WEBSITE_DIR;
}

const MIME_TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'application/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.glb': 'model/gltf-binary',
  '.woff2': 'font/woff2',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.svg': 'image/svg+xml',
  '.xml': 'application/xml; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
  '.webmanifest': 'application/manifest+json; charset=utf-8',
};

function resolveChromeExecutable() {
  if (process.env.CHROME_PATH && fsSync.existsSync(process.env.CHROME_PATH)) {
    return process.env.CHROME_PATH;
  }

  try {
    const pwPath = chromium.executablePath();
    if (pwPath && fsSync.existsSync(pwPath)) {
      return pwPath;
    }
  } catch {
    // Fall through to cache scanning
  }

  const home = os.homedir();
  const cacheBases = [
    path.join(home, 'Library/Caches/ms-playwright'),
    path.join(home, '.cache/ms-playwright'),
  ];

  for (const base of cacheBases) {
    if (!fsSync.existsSync(base)) continue;
    const dirs = fsSync
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
        path.join(base, dir, 'chrome-linux64/chrome'),
      ];
      for (const candidate of candidates) {
        if (fsSync.existsSync(candidate)) return candidate;
      }
    }
  }

  const systemCandidates = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/usr/bin/chromium',
    '/usr/bin/chromium-browser',
    '/usr/bin/google-chrome',
  ];
  for (const candidate of systemCandidates) {
    if (fsSync.existsSync(candidate)) return candidate;
  }
  return undefined;
}

function startStaticServer(rootDir) {
  return new Promise((resolve, reject) => {
    const server = http.createServer(async (req, res) => {
      try {
        const reqUrl = new URL(req.url || '/', 'http://127.0.0.1');
        let relPath = decodeURIComponent(reqUrl.pathname);
        if (relPath === '/') relPath = '/index.html';
        const safeRel = path.posix.normalize(relPath).replace(/^(\.\.\/)+/, '');
        let filePath = path.join(rootDir, safeRel);

        let stat;
        try {
          stat = await fs.stat(filePath);
          if (stat.isDirectory()) {
            filePath = path.join(filePath, 'index.html');
            stat = await fs.stat(filePath);
          }
        } catch {
          const notFoundPath = path.join(rootDir, '404.html');
          try {
            const body = await fs.readFile(notFoundPath);
            res.writeHead(404, { 'Content-Type': 'text/html; charset=utf-8' });
            res.end(body);
            return;
          } catch {
            res.writeHead(404);
            res.end('Not Found');
            return;
          }
        }

        const ext = path.extname(filePath).toLowerCase();
        const mime = MIME_TYPES[ext] || 'application/octet-stream';
        const content = await fs.readFile(filePath);
        res.writeHead(200, { 'Content-Type': mime, 'Cache-Control': 'no-store' });
        res.end(content);
      } catch (err) {
        res.writeHead(500);
        res.end(String(err));
      }
    });

    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const addr = server.address();
      if (!addr || typeof addr === 'string') {
        reject(new Error('Failed to bind ephemeral port'));
        return;
      }
      resolve({ server, baseUrl: `http://127.0.0.1:${addr.port}` });
    });
  });
}

async function main() {
  // Static production hosting asset checks before Chromium launch
  for (const requiredStatic of ['index.html', '_headers', 'robots.txt', 'sitemap.xml', '404.html', 'og/og.png']) {
    const assetPath = path.join(DIST_DIR, requiredStatic);
    assert.ok(fsSync.existsSync(assetPath), `Missing production hosting asset in dist/: ${requiredStatic}`);
  }
  const headersRaw = await fs.readFile(path.join(DIST_DIR, '_headers'), 'utf8');
  assert.ok(
    headersRaw.includes('/assets/*') &&
      headersRaw.includes('immutable') &&
      headersRaw.includes('X-Content-Type-Options: nosniff'),
    'dist/_headers is missing immutable asset cache or X-Content-Type-Options header',
  );

  const { server, baseUrl } = await startStaticServer(DIST_DIR);
  const consoleErrors = [];
  const pageErrors = [];
  const failedRequests = [];
  const externalRequests = [];
  let glbStatus = null;

  if (SCREENSHOT_DIR) {
    await fs.mkdir(SCREENSHOT_DIR, { recursive: true });
  }

  const executablePath = resolveChromeExecutable();
  let browser;
  try {
    browser = await chromium.launch({
      headless: true,
      executablePath,
      args: [
        '--enable-webgl',
        '--ignore-gpu-blocklist',
        '--use-gl=angle',
        '--use-angle=swiftshader',
      ],
    });

    const context = await browser.newContext({
      viewport: { width: 1440, height: 920 },
      deviceScaleFactor: 2,
      colorScheme: 'light',
    });

    const page = await context.newPage();

    page.on('console', (msg) => {
      if (msg.type() === 'error') {
        consoleErrors.push(msg.text());
      }
    });
    page.on('pageerror', (err) => {
      pageErrors.push(err.message);
    });
    page.on('request', (req) => {
      const url = req.url();
      if (
        !url.startsWith(baseUrl) &&
        !url.startsWith('data:') &&
        !url.startsWith('blob:')
      ) {
        externalRequests.push(url);
      }
    });
    page.on('response', (res) => {
      if (res.url().endsWith('omnibridge-photonic-matrix.glb')) {
        glbStatus = res.status();
      }
      if (res.status() >= 400) {
        failedRequests.push(`${res.status()} ${res.url()}`);
      }
    });

    await page.goto(baseUrl, { waitUntil: 'networkidle' });
    await page.evaluate(() => document.fonts.ready);

    await page.waitForFunction(
      () => {
        const el = document.getElementById('photonic-stage');
        return el && el.dataset.glbLoaded && el.dataset.glbLoaded !== 'loading';
      },
      { timeout: 15000 },
    );

    const glbState = await page.getAttribute('#photonic-stage', 'data-glb-loaded');
    assert.equal(glbState, 'true', `Expected 3D GLB stage to load cleanly, got ${glbState}`);
    assert.equal(glbStatus, 200, `Expected GLB response status 200, got ${glbStatus}`);
    await page.waitForTimeout(450);

    // 1. Default Light M3 theme check & screenshot
    const initialTheme = await page.evaluate(() => document.documentElement.dataset.siteTheme);
    assert.equal(initialTheme, 'light', 'Expected default site theme to be light');

    const pinCount = await page.locator('#stage-pins-layer .stage-pin').count();
    assert.equal(pinCount, 4, `Expected 4 3D callout pins, got ${pinCount}`);

    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '01-hero-light.png') });
    }

    // 2. Dark M3 theme toggle & screenshot
    await page.click('#btn-site-theme');
    await page.waitForTimeout(350);
    const darkTheme = await page.evaluate(() => document.documentElement.dataset.siteTheme);
    assert.equal(darkTheme, 'dark', 'Expected site theme toggle to switch to dark');

    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '02-hero-dark.png') });
    }

    await page.click('#btn-site-theme');
    await page.waitForTimeout(250);
    assert.equal(
      await page.evaluate(() => document.documentElement.dataset.siteTheme),
      'light',
      'Expected site theme toggle back to light',
    );

    // 3. Test all 6 route presets & readout updates
    const presetButtons = page.locator('#route-presets-grid [data-preset-id]');
    const presetCount = await presetButtons.count();
    assert.equal(presetCount, 6, `Expected 6 model route presets, got ${presetCount}`);
    for (let i = 0; i < presetCount; i++) {
      await presetButtons.nth(i).click();
      await page.waitForTimeout(160);
      const selected = await presetButtons.nth(i).getAttribute('aria-selected');
      assert.equal(selected, 'true', `Preset index ${i} should become aria-selected=true`);
    }
    const lastLogical = await page.textContent('#readout-logical');
    assert.ok(
      lastLogical && lastLogical.includes('ollama/'),
      `Expected ollama model in readout after clicking preset 6, got ${lastLogical}`,
    );

    await presetButtons.nth(1).click();
    await page.waitForTimeout(180);

    // 4. Test 3D free inspect drag & camera reset
    await page.click('#btn-free-inspect');
    assert.equal(await page.getAttribute('#btn-free-inspect', 'aria-pressed'), 'true');
    const stageBox = await page.locator('.stage-viewport').boundingBox();
    assert.ok(stageBox, 'Stage viewport must have bounding box');
    await page.mouse.move(stageBox.x + stageBox.width * 0.5, stageBox.y + stageBox.height * 0.5);
    await page.mouse.down();
    await page.mouse.move(stageBox.x + stageBox.width * 0.65, stageBox.y + stageBox.height * 0.45, {
      steps: 8,
    });
    await page.mouse.up();
    await page.waitForTimeout(150);
    await page.click('#btn-reset-view');
    assert.equal(await page.getAttribute('#btn-free-inspect', 'aria-pressed'), 'false');

    // 5. Interactive Control Panel Sandbox tests
    await page.evaluate(() => {
      const el = document.getElementById('panel-showcase');
      if (el) window.scrollTo({ top: Math.max(0, el.offsetTop - 76), behavior: 'instant' });
    });
    await page.waitForTimeout(250);

    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '03-panel-sandbox.png') });
    }

    await page.click('[data-m3s-tab="accounts"]');
    await page.waitForSelector('.m3s-account-grid');

    await page.click('[data-m3s-tab="providers"]');
    await page.click('#m3s-toggle-add-provider');
    await page.fill('#m3s-inp-name', 'Test Gateway');
    await page.fill('#m3s-inp-url', 'http://api.example.com/v1');
    await page.click('#m3s-add-provider-form button[type="submit"]');
    const httpErrText = await page.textContent('.m3s-snackbar');
    assert.ok(
      httpErrText && (httpErrText.includes('HTTPS') || httpErrText.includes('https')),
      `Expected non-loopback HTTP rejection toast, got "${httpErrText}"`,
    );

    await page.fill('#m3s-inp-url', 'https://api.example.com/v1');
    await page.click('#m3s-add-provider-form button[type="submit"]');
    await page.click('#m3s-sync-btn');
    const genTag = await page.textContent('.picker-gen-tag');
    assert.ok(genTag && genTag.includes('gen #5'), `Expected gen #5 after sync, got "${genTag}"`);

    await page.click('[data-m3s-tab="settings"]');
    await page.click('[data-set-contrast="high"]');
    assert.equal(await page.getAttribute('.m3s-window', 'data-panel-contrast'), 'high');
    await page.click('[data-set-contrast="standard"]');

    // 6. Thread Simulator 4 turns + autoplay toggle
    const threadLab = page.locator('#thread-lab');
    await threadLab.scrollIntoViewIfNeeded();
    await page.waitForTimeout(200);
    const stepButtons = page.locator('[data-thread-step]');
    assert.equal(await stepButtons.count(), 4, 'Expected 4 turns in ThreadSimulator');
    for (let t = 0; t < 4; t++) {
      await stepButtons.nth(t).click();
      await page.waitForTimeout(120);
      const turnCount = await page.locator('#thread-transcript-list .transcript-turn').count();
      assert.equal(turnCount, t + 1, `Expected ${t + 1} cumulative turns rendered`);
    }
    await page.click('#thread-autoplay-btn');
    await page.waitForTimeout(180);
    await page.click('#thread-autoplay-btn');

    // 7. Downloads, CLI tabs, Metadata & Clean Copy Assertions
    await page.evaluate(() => {
      const el = document.getElementById('quickstart');
      if (el) window.scrollTo({ top: Math.max(0, el.offsetTop - 76), behavior: 'instant' });
    });
    await page.waitForTimeout(250);

    if (SCREENSHOT_DIR) {
      await page.screenshot({ path: path.join(SCREENSHOT_DIR, '04-downloads-cli.png') });
    }

    const cliTabs = page.locator('[data-cli-tab]');
    assert.equal(await cliTabs.count(), 4, 'Expected 4 CLI quickstart tabs');
    for (let c = 0; c < 4; c++) {
      await cliTabs.nth(c).click();
      const tabKey = await cliTabs.nth(c).getAttribute('data-cli-tab');
      const isVisible = await page.locator(`[data-cli-panel="${tabKey}"]`).isVisible();
      assert.equal(isVisible, true, `Expected CLI panel ${tabKey} to be visible`);
    }

    const seoCheck = await page.evaluate(() => {
      const canonical = document.querySelector('link[rel="canonical"]')?.getAttribute('href');
      const msvalidate = document.querySelector('meta[name="msvalidate.01"]')?.getAttribute('content');
      const ogImage = document.querySelector('meta[property="og:image"]')?.getAttribute('content');
      const links = Array.from(document.querySelectorAll('a[href]')).map((a) => a.getAttribute('href') || '');
      const text = document.body.innerText;
      return { canonical, msvalidate, ogImage, links, text };
    });

    assert.equal(seoCheck.canonical, 'https://omnibridge.igng.net/');
    assert.equal(seoCheck.msvalidate, 'B411826ACAE5C5CD20EA5574C3F8A6E4');
    assert.equal(seoCheck.ogImage, 'https://omnibridge.igng.net/og/og.png');

    for (const expectedAsset of [
      'codex-omnibridge-macos-aarch64-0.1.2.dmg',
      'codex-omnibridge-macos-x86_64-0.1.2.dmg',
      'codex-omnibridge-windows-x86_64-0.1.2-setup.exe',
      'codex-omnibridge_0.1.2_amd64.deb',
      'SHA256SUMS',
    ]) {
      assert.ok(
        seoCheck.links.some((href) => href.includes(expectedAsset)),
        `Expected real v0.1.2 release link containing ${expectedAsset}`,
      );
    }

    for (const forbidden of ['--discover', '--protocol chat_completions', 'web set-password', '浏']) {
      assert.equal(
        seoCheck.text.includes(forbidden),
        false,
        `Page body text must NOT contain forbidden string "${forbidden}"`,
      );
    }

    // 8. Responsive Overflow Check + Mobile Screenshot
    const viewports = [
      { width: 1440, height: 920 },
      { width: 1024, height: 768 },
      { width: 768, height: 1024 },
      { width: 390, height: 844 },
      { width: 375, height: 667 },
    ];

    for (const vp of viewports) {
      await page.setViewportSize(vp);
      await page.evaluate(() => window.scrollTo(0, 0));
      await page.waitForTimeout(220);
      const overflow = await page.evaluate(() => ({
        scrollWidth: document.documentElement.scrollWidth,
        innerWidth: window.innerWidth,
      }));
      assert.ok(
        overflow.scrollWidth <= overflow.innerWidth,
        `Horizontal overflow at ${vp.width}x${vp.height}: scrollWidth=${overflow.scrollWidth} > innerWidth=${overflow.innerWidth}`,
      );
      if (SCREENSHOT_DIR && vp.width === 390) {
        await page.screenshot({ path: path.join(SCREENSHOT_DIR, '05-mobile-light.png') });
      }
    }

    assert.deepEqual(externalRequests, [], `Unexpected external requests: ${externalRequests.join(', ')}`);
    assert.deepEqual(failedRequests, [], `HTTP >= 400 responses: ${failedRequests.join(', ')}`);
    assert.deepEqual(pageErrors, [], `Uncaught page errors: ${pageErrors.join(' | ')}`);
    assert.deepEqual(consoleErrors, [], `Console errors: ${consoleErrors.join(' | ')}`);

    console.log('✓ All website verification checks passed (Light/Dark M3, 3D GLB, Sandbox, ThreadSim, v0.1.2 links, 0 overflow, 0 external/console errors).');
  } finally {
    await browser?.close();
    server.close();
  }
}

main().catch((err) => {
  console.error('✗ Website verification failed:', err);
  process.exit(1);
});
