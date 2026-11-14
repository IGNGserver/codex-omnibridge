import './styles/tokens.css';
import './styles/main.css';
import './styles/panel-sandbox.css';

import {
  HOW_IT_WORKS_STEPS,
  MODEL_ROUTE_PRESETS,
  RELEASE_PLATFORMS,
  WORKSPACE_CRATES,
  type ModelRoutePreset,
  type RoutePathKind,
} from './data/productData';
import { PhotonicStage, type CalloutPinProjection } from './scene/PhotonicStage';
import { ThreadSimulator } from './ui/ThreadSimulator';
import { PanelSandbox } from './ui/PanelSandbox';

type SiteTheme = 'light' | 'dark';

const THEME_STORAGE_KEY = 'omnibridge-site-theme';

const SUN_ICON_SVG =
  '<svg class="omni-svg-icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M12 18a6 6 0 1 1 0-12 6 6 0 0 1 0 12Zm0-15V1h1v2h-1Zm0 20v-2h1v2h-1ZM4.22 5.64 2.8 4.22l.71-.7 1.42 1.4-.71.72Zm15.56 15.56-1.42-1.42.71-.7 1.42 1.4-.71.72ZM1 12h2v1H1v-1Zm20 0h2v1h-2v-1ZM4.22 18.36l-.71-.7 1.42-1.42.7.71-1.41 1.41Zm15.56-15.56.71.71-1.42 1.41-.7-.71 1.41-1.41Z"/></svg>';

const MOON_ICON_SVG =
  '<svg class="omni-svg-icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M12.1 22c5.52 0 10-4.48 10-10 0-.47-.04-.92-.1-1.37a7.5 7.5 0 0 1-8.63-8.63c-.45-.06-.9-.1-1.37-.1a10 10 0 0 0 .1 20Z"/></svg>';

const PIN_TO_STEP_ID: Record<string, string> = {
  'pin-ingress': 'card-step-router',
  'pin-crossbar': 'card-step-router',
  'pin-bridge': 'card-step-bridge',
  'pin-vault': 'card-step-security',
};

function escapeHtml(raw: string): string {
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

function formatInlineCode(text: string): string {
  return escapeHtml(text).replace(/`([^`]+)`/g, '<code>$1</code>');
}

function routeColorVar(kind: RoutePathKind, theme: SiteTheme): string {
  if (kind === 'official') return theme === 'light' ? '#006874' : '#36d9ef';
  if (kind === 'bridge') return theme === 'light' ? '#7b4384' : '#d6a4e8';
  return theme === 'light' ? '#2b6832' : '#7ae896';
}

function detectPlatformId(): 'macos-arm' | 'macos-x64' | 'windows-x64' | 'linux-x64' {
  const ua = navigator.userAgent.toLowerCase();
  if (ua.includes('win')) return 'windows-x64';
  if (ua.includes('mac')) return 'macos-arm';
  if (ua.includes('linux') && !ua.includes('android')) return 'linux-x64';
  return 'macos-arm';
}

function getInitialTheme(): SiteTheme {
  const attr = document.documentElement.dataset.siteTheme;
  if (attr === 'dark' || attr === 'light') return attr;
  return 'light';
}

function init() {
  let currentTheme: SiteTheme = getInitialTheme();
  let currentPreset: ModelRoutePreset = MODEL_ROUTE_PRESETS[0];
  let activePinId: string | null = null;
  let freeInspect = false;

  const presetsGrid = document.getElementById('route-presets-grid');
  const pinsLayer = document.getElementById('stage-pins-layer');
  const stageContainer = document.getElementById('photonic-stage');
  const btnFreeInspect = document.getElementById('btn-free-inspect') as HTMLButtonElement | null;
  const btnResetView = document.getElementById('btn-reset-view') as HTMLButtonElement | null;
  const btnTheme = document.getElementById('btn-site-theme') as HTMLButtonElement | null;
  const themeIcon = document.getElementById('site-theme-icon');
  const themeLabel = document.getElementById('site-theme-label');

  // 1. OS-aware hero download CTA
  const detectedOsId = detectPlatformId();
  const detectedCard = RELEASE_PLATFORMS.find((p) => p.id === detectedOsId) || RELEASE_PLATFORMS[0];
  const heroDownloadBtn = document.getElementById('hero-download-btn') as HTMLAnchorElement | null;
  const heroDownloadLabel = document.getElementById('hero-download-label');
  if (heroDownloadBtn && heroDownloadLabel && detectedCard.links[0]) {
    heroDownloadBtn.href = detectedCard.links[0].url;
    heroDownloadBtn.target = '_blank';
    heroDownloadLabel.textContent = `下载 ${detectedCard.title} v0.1.2`;
  }

  // 2. Render 6 model preset buttons
  if (presetsGrid) {
    presetsGrid.innerHTML = MODEL_ROUTE_PRESETS.map(
      (preset, index) => `
      <button
        type="button"
        role="tab"
        aria-selected="${index === 0 ? 'true' : 'false'}"
        class="route-preset-btn ${index === 0 ? 'is-active' : ''}"
        data-preset-id="${preset.id}"
        data-route-kind="${preset.routeKind}"
      >
        <div class="route-btn-top">
          <span class="route-btn-vendor">${escapeHtml(preset.vendorBadge)}</span>
          <span class="route-btn-dot" aria-hidden="true"></span>
        </div>
        <span class="route-btn-title">${escapeHtml(preset.shortLabel)}</span>
        <span class="route-btn-model">${escapeHtml(preset.logicalModelId)}</span>
      </button>
    `,
    ).join('');
  }

  // 3. Render How-It-Works 4 plain-Chinese cards
  const howGrid = document.getElementById('how-steps-grid');
  if (howGrid) {
    howGrid.innerHTML = HOW_IT_WORKS_STEPS.map(
      (step) => `
      <article class="how-card" id="card-${step.id}">
        <div class="how-card-head">
          <span class="how-card-badge">
            <span class="how-icon-wrap">
              <span class="material-symbols-outlined ${step.iconClass}" aria-hidden="true"></span>
            </span>
            <span>${escapeHtml(step.stepTag)}</span>
          </span>
          <span class="how-crate-tag">${escapeHtml(step.crateTag)}</span>
        </div>
        <h3>${escapeHtml(step.title)}</h3>
        <p class="how-card-lead">${escapeHtml(step.lead)}</p>
        <ul class="how-points">
          ${step.points.map((pt) => `<li>${formatInlineCode(pt)}</li>`).join('')}
        </ul>
      </article>
    `,
    ).join('');
  }

  // 4. Render collapsible 10 workspace crates
  const cratesList = document.getElementById('crates-list');
  if (cratesList) {
    cratesList.innerHTML = WORKSPACE_CRATES.map(
      (item) => `
      <div class="crate-item">
        <div class="crate-item-head">
          <code>${escapeHtml(item.crate)}</code>
          <span class="crate-layer">${escapeHtml(item.layer)}</span>
        </div>
        <p>${escapeHtml(item.responsibility)}</p>
        <span class="crate-highlight">${escapeHtml(item.highlight)}</span>
      </div>
    `,
    ).join('');
  }

  // 5. Render 4 platform download cards (real v0.1.2 GitHub release assets)
  const downloadGrid = document.getElementById('download-grid');
  if (downloadGrid) {
    downloadGrid.innerHTML = RELEASE_PLATFORMS.map((platform) => {
      const primaryLink = platform.links[0];
      const secondaryLinks = platform.links.slice(1);
      const isDetected = platform.id === detectedOsId;
      return `
        <article class="download-card ${isDetected ? 'is-detected' : ''}" id="download-card-${platform.id}">
          <span class="download-os-tag">${escapeHtml(platform.osTag)}</span>
          <h3>${escapeHtml(platform.title)}</h3>
          <p>${escapeHtml(platform.desc)}</p>
          <div class="download-links">
            <a
              class="m3-btn ${isDetected ? 'm3-btn--primary' : 'm3-btn--tonal'} m3-btn--sm"
              href="${escapeHtml(primaryLink.url)}"
              target="_blank"
              rel="noopener"
            >
              ${escapeHtml(primaryLink.label)}
            </a>
            ${
              secondaryLinks.length
                ? `<div class="download-secondary-row">
                    ${secondaryLinks
                      .map(
                        (link) => `
                        <a
                          class="m3-btn m3-btn--outlined m3-btn--sm"
                          href="${escapeHtml(link.url)}"
                          target="_blank"
                          rel="noopener"
                        >
                          ${escapeHtml(link.label)}
                        </a>
                      `,
                      )
                      .join('')}
                  </div>`
                : ''
            }
          </div>
        </article>
      `;
    }).join('');
  }

  // 6. Initialize 3D Router Stage
  let stage: PhotonicStage | null = null;
  if (stageContainer) {
    stage = new PhotonicStage(stageContainer, {
      onPinsUpdate: (pins) => renderPins(pins),
      onModuleSelect: (nodeName) => {
        const pinId =
          nodeName === 'Module_ProtocolBridge'
            ? 'pin-bridge'
            : nodeName === 'Module_KeychainVault'
              ? 'pin-vault'
              : nodeName === 'Module_Crossbar'
                ? 'pin-crossbar'
                : 'pin-ingress';
        activePinId = pinId;
        const cardId = PIN_TO_STEP_ID[pinId];
        if (cardId) {
          document.getElementById(cardId)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
        }
      },
    });
    stage.setSiteTheme(currentTheme);
  }

  function renderPins(pins: CalloutPinProjection[]) {
    if (!pinsLayer) return;
    if (pinsLayer.childElementCount !== pins.length) {
      pinsLayer.innerHTML = pins
        .map(
          (pin) => `
          <button
            type="button"
            class="stage-pin"
            data-pin-id="${pin.id}"
            title="${escapeHtml(`${pin.label} · ${pin.sublabel}`)}"
          >
            <span class="stage-pin-dot" aria-hidden="true"></span>
            <span>${escapeHtml(pin.label)}</span>
          </button>
        `,
        )
        .join('');

      pinsLayer.querySelectorAll<HTMLButtonElement>('[data-pin-id]').forEach((btn) => {
        btn.addEventListener('click', () => {
          const id = btn.dataset.pinId || '';
          activePinId = id;
          const cardId = PIN_TO_STEP_ID[id];
          if (cardId) {
            document.getElementById(cardId)?.scrollIntoView({ behavior: 'smooth', block: 'center' });
          }
        });
      });
    }

    pins.forEach((pin) => {
      const btn = pinsLayer.querySelector<HTMLButtonElement>(`[data-pin-id="${pin.id}"]`);
      if (!btn) return;
      btn.style.left = `${pin.x.toFixed(1)}px`;
      btn.style.top = `${pin.y.toFixed(1)}px`;
      btn.classList.toggle('is-hidden', !pin.visible);
      btn.classList.toggle('is-active', activePinId === pin.id || pin.highlighted);
    });
  }

  // 7. Route readout synchronization
  function applyPreset(presetId: string) {
    const preset = MODEL_ROUTE_PRESETS.find((p) => p.id === presetId) || MODEL_ROUTE_PRESETS[0];
    currentPreset = preset;

    document.documentElement.style.setProperty(
      '--omni-active-accent',
      routeColorVar(preset.routeKind, currentTheme),
    );

    presetsGrid?.querySelectorAll<HTMLButtonElement>('[data-preset-id]').forEach((btn) => {
      const isSelected = btn.dataset.presetId === preset.id;
      btn.classList.toggle('is-active', isSelected);
      btn.setAttribute('aria-selected', isSelected ? 'true' : 'false');
    });

    const badgeEl = document.getElementById('readout-badge');
    const protoEl = document.getElementById('readout-protocol');
    const sumEl = document.getElementById('readout-summary');
    const logicalEl = document.getElementById('readout-logical');
    const egressEl = document.getElementById('readout-egress');
    const authEl = document.getElementById('readout-auth');
    const capsEl = document.getElementById('readout-caps');

    if (badgeEl) {
      badgeEl.textContent = preset.routeBadge;
      badgeEl.dataset.routeKind = preset.routeKind;
    }
    if (protoEl) protoEl.textContent = preset.protocolLabel;
    if (sumEl) sumEl.textContent = preset.summary;
    if (logicalEl) logicalEl.textContent = preset.logicalModelId;
    if (egressEl) egressEl.textContent = `${preset.egressEndpoint} → ${preset.upstreamModelId}`;
    if (authEl) authEl.textContent = `${preset.authAction}（${preset.authDetail}）`;
    if (capsEl) {
      capsEl.innerHTML = preset.capabilities
        .map((cap) => `<span class="cap-pill">${escapeHtml(cap)}</span>`)
        .join('');
    }

    stage?.setModelPreset(preset);
  }

  presetsGrid?.querySelectorAll<HTMLButtonElement>('[data-preset-id]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const id = btn.dataset.presetId;
      if (id) applyPreset(id);
    });
  });

  // 8. 3D inspect & reset controls
  function setFreeInspect(enabled: boolean) {
    freeInspect = enabled;
    stage?.setFreeInspectMode(enabled);
    if (btnFreeInspect) {
      btnFreeInspect.classList.toggle('is-active', enabled);
      btnFreeInspect.setAttribute('aria-pressed', enabled ? 'true' : 'false');
      btnFreeInspect.textContent = enabled ? '退出旋转' : '3D 自由旋转';
    }
  }

  btnFreeInspect?.addEventListener('click', () => {
    setFreeInspect(!freeInspect);
  });

  btnResetView?.addEventListener('click', () => {
    setFreeInspect(false);
    stage?.resetCameraView();
  });

  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && freeInspect) {
      setFreeInspect(false);
      stage?.resetCameraView();
    }
  });

  // 9. Mount Interactive Panel Sandbox
  const panelRoot = document.getElementById('panel-sandbox-root');
  const panelSandbox = panelRoot
    ? new PanelSandbox(panelRoot, {
        onRoutePreview: (preset) => {
          applyPreset(preset.id);
          document.getElementById('router-stage-card')?.scrollIntoView({
            behavior: 'smooth',
            block: 'center',
          });
        },
        onSyncCatalog: () => {
          stage?.triggerCatalogSyncEffect();
        },
      })
    : null;

  // 10. Mount Single-Thread Simulator
  const threadRoot = document.getElementById('thread-simulator-root');
  if (threadRoot) {
    new ThreadSimulator(threadRoot, (preset) => {
      applyPreset(preset.id);
    });
  }

  // 11. Site Light/Dark Theme Sync across Website, 3D Stage & Sandbox
  function applyTheme(theme: SiteTheme, persist = false) {
    currentTheme = theme;
    document.documentElement.dataset.siteTheme = theme;
    const meta = document.querySelector('meta[name="theme-color"]');
    if (meta) {
      meta.setAttribute('content', theme === 'dark' ? '#0b1516' : '#f6feff');
    }
    if (themeIcon) {
      themeIcon.innerHTML = theme === 'dark' ? SUN_ICON_SVG : MOON_ICON_SVG;
    }
    if (themeLabel) {
      themeLabel.textContent = theme === 'dark' ? '浅色模式' : '深色模式';
    }
    if (persist) {
      try {
        localStorage.setItem(THEME_STORAGE_KEY, theme);
      } catch {}
    }
    document.documentElement.style.setProperty(
      '--omni-active-accent',
      routeColorVar(currentPreset.routeKind, theme),
    );
    stage?.setSiteTheme(theme);
    panelSandbox?.setTheme(theme);
  }

  btnTheme?.addEventListener('click', () => {
    applyTheme(currentTheme === 'light' ? 'dark' : 'light', true);
  });

  applyTheme(currentTheme, false);
  applyPreset(MODEL_ROUTE_PRESETS[0].id);

  // 12. CLI Workbench Tabs & Copy
  const cliWorkbench = document.getElementById('cli-workbench');
  const cliCopyBtn = document.getElementById('cli-copy-btn');
  cliWorkbench?.querySelectorAll<HTMLButtonElement>('[data-cli-tab]').forEach((tab) => {
    tab.addEventListener('click', () => {
      const target = tab.dataset.cliTab;
      cliWorkbench.querySelectorAll<HTMLButtonElement>('[data-cli-tab]').forEach((t) => {
        const active = t.dataset.cliTab === target;
        t.classList.toggle('is-active', active);
        t.setAttribute('aria-selected', active ? 'true' : 'false');
      });
      cliWorkbench.querySelectorAll<HTMLElement>('[data-cli-panel]').forEach((panel) => {
        panel.classList.toggle('is-active', panel.dataset.cliPanel === target);
      });
    });
  });

  cliCopyBtn?.addEventListener('click', async () => {
    const activeCode = cliWorkbench?.querySelector('.cli-panel.is-active code')?.textContent || '';
    if (!activeCode) return;
    try {
      await navigator.clipboard.writeText(activeCode);
      cliCopyBtn.textContent = '已复制';
      window.setTimeout(() => {
        cliCopyBtn.textContent = '复制命令';
      }, 1800);
    } catch {
      cliCopyBtn.textContent = '请手动选中复制';
      window.setTimeout(() => {
        cliCopyBtn.textContent = '复制命令';
      }, 1800);
    }
  });

  // 13. Highlight active section link in top nav
  const navLinks = Array.from(document.querySelectorAll<HTMLAnchorElement>('[data-nav-target]'));
  if ('IntersectionObserver' in window && navLinks.length > 0) {
    const navObserver = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => b.intersectionRatio - a.intersectionRatio)[0];
        if (!visible?.target?.id) return;
        navLinks.forEach((link) => {
          link.classList.toggle('is-active', link.dataset.navTarget === visible.target.id);
        });
      },
      { rootMargin: '-25% 0px -55% 0px', threshold: [0.1, 0.4, 0.7] },
    );
    navLinks.forEach((link) => {
      const id = link.dataset.navTarget;
      const el = id ? document.getElementById(id) : null;
      if (el) navObserver.observe(el);
    });
  }
}

init();
