import './styles/tokens.css';
import './styles/main.css';
import './styles/panel-sandbox.css';

import {
  INTERNAL_MODULE_STAGES,
  MODEL_ROUTE_PRESETS,
  WORKSPACE_CRATES,
  type ModelRoutePreset,
} from './data/productData';
import { PhotonicStage, type CalloutPinProjection } from './scene/PhotonicStage';
import { ThreadSimulator } from './ui/ThreadSimulator';
import { PanelSandbox } from './ui/PanelSandbox';

/* ==========================================================================
   Codex OmniBridge Official Website — Orchestrator
   Synchronizes 3D Photonic Hardware Stage, Scroll Fly-Through & Interactive UI
   ========================================================================== */

const NODE_TO_STAGE_ID: Record<string, string> = {
  Module_Ingress: 'stage-router',
  Module_Registry: 'stage-catalog',
  Module_Crossbar: 'stage-router',
  Module_ProtocolBridge: 'stage-bridge',
  Module_KeychainVault: 'stage-vault',
  Manifold_Egress: 'stage-bridge',
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

function bootstrap() {
  let activePreset: ModelRoutePreset = MODEL_ROUTE_PRESETS[0];
  let freeInspect = false;

  // 1. Render static-data-driven DOM sections synchronously before first frame
  renderHeroRouteButtons();
  renderInsideStations();
  renderWorkspaceCrates();

  // 2. Prepare persistent 3D Callout Reticle Pin elements (zero per-frame DOM churn)
  const pinsLayer = document.getElementById('photonic-pins-layer');
  const pinElements = new Map<string, HTMLButtonElement>();

  const ensurePinElement = (pin: CalloutPinProjection): HTMLButtonElement => {
    let el = pinElements.get(pin.id);
    if (!el) {
      el = document.createElement('button');
      el.type = 'button';
      el.className = 'callout-pin';
      el.dataset.pinId = pin.id;
      el.innerHTML = `
        <span class="callout-pin__reticle" aria-hidden="true"></span>
        <span class="callout-pin__text">
          <span class="callout-pin__label">${escapeHtml(pin.label)}</span>
          <span class="callout-pin__sub">${escapeHtml(pin.sublabel)}</span>
        </span>
      `;
      el.addEventListener('click', () => {
        const map: Record<string, string> = {
          'pin-ingress': 'stage-router',
          'pin-registry': 'stage-catalog',
          'pin-crossbar': 'stage-router',
          'pin-bridge': 'stage-bridge',
          'pin-vault': 'stage-vault',
          'pin-egress': 'stage-bridge',
        };
        const targetId = map[pin.id];
        const targetEl = targetId ? document.getElementById(targetId) : null;
        if (freeInspect) setFreeInspect(false);
        targetEl?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      });
      pinsLayer?.appendChild(el);
      pinElements.set(pin.id, el);
    }
    return el;
  };

  // 3. Mount 3D Photonic Hardware Stage
  const stageContainer = document.getElementById('photonic-stage');
  const stage = stageContainer
    ? new PhotonicStage(stageContainer, {
        onPinsUpdate: (pins) => {
          for (const p of pins) {
            const el = ensurePinElement(p);
            if (!p.visible) {
              el.style.opacity = '0';
              el.style.pointerEvents = 'none';
            } else {
              el.style.opacity = '1';
              el.style.pointerEvents = 'auto';
              el.style.transform = `translate3d(${p.x.toFixed(1)}px, ${p.y.toFixed(1)}px, 0) translate(-50%, -50%)`;
            }
            el.classList.toggle('is-highlighted', p.highlighted);
          }
        },
        onModuleSelect: (nodeName) => {
          const targetStageId = NODE_TO_STAGE_ID[nodeName];
          const card = targetStageId ? document.getElementById(targetStageId) : null;
          if (card) {
            if (freeInspect) setFreeInspect(false);
            card.scrollIntoView({ behavior: 'smooth', block: 'center' });
          }
        },
      })
    : null;

  // 4. Unified Model Route Selection Handler (syncs Hero Dock + CSS spectral accent + 3D Stage)
  const applyModelRoute = (preset: ModelRoutePreset) => {
    activePreset = preset;
    document.documentElement.style.setProperty('--omni-active-accent', preset.accentHex);
    document.documentElement.style.setProperty('--omni-active-secondary', preset.secondaryHex);

    document.querySelectorAll<HTMLButtonElement>('[data-route-preset]').forEach((btn) => {
      const isSelected = btn.dataset.routePreset === preset.id;
      btn.classList.toggle('is-active', isSelected);
      btn.setAttribute('aria-selected', isSelected ? 'true' : 'false');
    });

    updateHeroWireReadout(preset);
    stage?.setModelPreset(preset);
  };

  // Wire Hero 6-Model buttons
  document.querySelectorAll<HTMLButtonElement>('[data-route-preset]').forEach((btn) => {
    btn.addEventListener('click', () => {
      const found = MODEL_ROUTE_PRESETS.find((p) => p.id === btn.dataset.routePreset);
      if (found) applyModelRoute(found);
    });
  });

  // Initialize initial route readout
  applyModelRoute(activePreset);

  // 5. Mount Same-Thread 4-Turn Simulator
  const threadMount = document.getElementById('thread-simulator-mount');
  if (threadMount) {
    new ThreadSimulator(threadMount, (preset) => {
      applyModelRoute(preset);
    });
  }

  // 6. Mount Authentic Material 3 Expressive Control Panel Sandbox
  const panelMount = document.getElementById('panel-sandbox-mount');
  if (panelMount) {
    new PanelSandbox(panelMount, {
      onRoutePreview: (preset) => {
        applyModelRoute(preset);
      },
      onSyncCatalog: () => {
        stage?.triggerCatalogSyncEffect();
      },
    });
  }

  // 7. Wire Top Hardware Controls (Quartz Canopy & 3D Free Orbit Inspect)
  const canopyBtn = document.getElementById('btn-toggle-canopy');
  const canopyLabel = document.getElementById('label-toggle-canopy');
  canopyBtn?.addEventListener('click', () => {
    if (!stage) return;
    const isOpen = stage.toggleCanopyManual();
    canopyBtn.setAttribute('aria-pressed', isOpen ? 'true' : 'false');
    if (canopyLabel) {
      canopyLabel.textContent = isOpen ? '闭合石英机盖' : '掀开石英机盖';
    }
  });

  const freeBtn = document.getElementById('btn-free-inspect');
  const exitFreeBtn = document.getElementById('btn-exit-free-inspect');

  function setFreeInspect(next: boolean) {
    freeInspect = next;
    document.body.classList.toggle('is-free-inspect', freeInspect);
    freeBtn?.setAttribute('aria-pressed', freeInspect ? 'true' : 'false');
    stage?.setFreeInspectMode(freeInspect);
  }

  freeBtn?.addEventListener('click', () => setFreeInspect(!freeInspect));
  exitFreeBtn?.addEventListener('click', () => setFreeInspect(false));
  window.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && freeInspect) {
      setFreeInspect(false);
    }
  });

  // 8. Wire CLI Quickstart Tabs & Copy Buttons
  document.querySelectorAll<HTMLButtonElement>('[data-cli-tab]').forEach((tabBtn) => {
    tabBtn.addEventListener('click', () => {
      const target = tabBtn.dataset.cliTab;
      document.querySelectorAll<HTMLButtonElement>('[data-cli-tab]').forEach((b) => {
        const active = b.dataset.cliTab === target;
        b.classList.toggle('is-active', active);
        b.setAttribute('aria-selected', active ? 'true' : 'false');
      });
      document.querySelectorAll<HTMLElement>('[data-cli-pane]').forEach((pane) => {
        pane.classList.toggle('is-active', pane.dataset.cliPane === target);
      });
    });
  });

  document.querySelectorAll<HTMLButtonElement>('[data-copy-target]').forEach((copyBtn) => {
    copyBtn.addEventListener('click', async () => {
      const id = copyBtn.dataset.copyTarget;
      const codeEl = id ? document.getElementById(id) : null;
      if (!codeEl) return;
      const raw = codeEl.textContent || '';
      try {
        await navigator.clipboard.writeText(raw);
        const orig = copyBtn.textContent;
        copyBtn.textContent = '✓ 已复制';
        window.setTimeout(() => {
          copyBtn.textContent = orig;
        }, 1800);
      } catch {
        copyBtn.textContent = '请手动复制';
      }
    });
  });

  // 9. Scroll-Driven Camera Choreography & Active Section / Module Highlighting
  const heroEl = document.getElementById('hero-matrix');
  const insideEl = document.getElementById('inside-matrix');
  const threadEl = document.getElementById('thread-lab');
  const panelEl = document.getElementById('panel-showcase');
  const archEl = document.getElementById('architecture-matrix');
  const quickEl = document.getElementById('quickstart');

  const stageCards = INTERNAL_MODULE_STAGES.map((s) => document.getElementById(s.id));
  const hudIndexEl = document.getElementById('inside-hud-index');
  const hudTitleEl = document.getElementById('inside-hud-title');

  let scrollTicking = false;
  const syncScrollChoreography = () => {
    scrollTicking = false;
    const vh = Math.max(1, window.innerHeight);

    const heroRect = heroEl?.getBoundingClientRect();
    const insideRect = insideEl?.getBoundingClientRect();
    const threadRect = threadEl?.getBoundingClientRect();
    const panelRect = panelEl?.getBoundingClientRect();

    // Hero progress 0..1
    const heroProgress = heroRect
      ? Math.max(0, Math.min(1, -heroRect.top / Math.max(1, heroRect.height * 0.85)))
      : 0;

    // Inside Matrix active & continuous progress 0..1 across the 4 module cards
    const insideActive = Boolean(
      insideRect && insideRect.top < vh * 0.58 && insideRect.bottom > vh * 0.32,
    );
    let insideProgress = 0;
    if (insideRect) {
      const span = Math.max(1, insideRect.height - vh * 0.65);
      insideProgress = Math.max(0, Math.min(1, (vh * 0.42 - insideRect.top) / span));
    }

    // Highlight active module card & sticky HUD by nearest viewport center
    let activeStageIdx = 0;
    let bestDist = Number.POSITIVE_INFINITY;
    stageCards.forEach((card, idx) => {
      if (!card) return;
      const r = card.getBoundingClientRect();
      const center = (r.top + r.bottom) * 0.5;
      const dist = Math.abs(center - vh * 0.45);
      if (dist < bestDist) {
        bestDist = dist;
        activeStageIdx = idx;
      }
    });
    stageCards.forEach((card, idx) => {
      card?.classList.toggle('is-active', insideActive && idx === activeStageIdx);
    });
    document.querySelectorAll<HTMLButtonElement>('[data-jump-stage]').forEach((btn, idx) => {
      btn.classList.toggle('is-active', idx === activeStageIdx);
    });
    if (insideActive) {
      const curStage = INTERNAL_MODULE_STAGES[activeStageIdx];
      if (hudIndexEl) hudIndexEl.textContent = curStage.indexLabel;
      if (hudTitleEl) {
        hudTitleEl.textContent = `当前聚焦：${curStage.title}（${curStage.crateName}）`;
      }
    }

    // Thread simulator active check
    const threadActive = Boolean(
      threadRect && threadRect.top < vh * 0.55 && threadRect.bottom > vh * 0.25,
    );

    // Dim 3D background slightly when user reaches dense M3 Panel / Architecture / CLI sections
    const dimBackground = Boolean(panelRect && panelRect.top < vh * 0.48);
    document.body.dataset.stageDim = dimBackground ? 'true' : 'false';

    stage?.updateScrollChoreography({
      heroProgress,
      insideActive,
      insideProgress,
      threadActive,
    });

    // Update top nav active pill
    const sections: Array<[string, HTMLElement | null]> = [
      ['hero-matrix', heroEl],
      ['inside-matrix', insideEl],
      ['thread-lab', threadEl],
      ['panel-showcase', panelEl],
      ['architecture-matrix', archEl],
      ['quickstart', quickEl],
    ];
    let currentSecId = 'hero-matrix';
    for (const [id, sec] of sections) {
      if (!sec) continue;
      const r = sec.getBoundingClientRect();
      if (r.top <= vh * 0.42) {
        currentSecId = id;
      }
    }
    document.querySelectorAll<HTMLAnchorElement>('[data-nav-sec]').forEach((link) => {
      link.classList.toggle('is-active', link.dataset.navSec === currentSecId);
    });
  };

  window.addEventListener(
    'scroll',
    () => {
      if (!scrollTicking) {
        scrollTicking = true;
        requestAnimationFrame(syncScrollChoreography);
      }
    },
    { passive: true },
  );
  syncScrollChoreography();
}

/* -------------------------------------------------------------------------- */
/*  DOM Section Builders                                                      */
/* -------------------------------------------------------------------------- */

function renderHeroRouteButtons() {
  const grid = document.getElementById('hero-route-grid');
  if (!grid) return;

  grid.innerHTML = MODEL_ROUTE_PRESETS.map(
    (p, idx) => `
      <button
        type="button"
        role="tab"
        class="route-preset-btn ${idx === 0 ? 'is-active' : ''}"
        aria-selected="${idx === 0 ? 'true' : 'false'}"
        data-route-preset="${p.id}"
        style="--preset-accent: ${p.accentHex}"
      >
        <span class="route-preset-btn__vendor">${escapeHtml(p.vendorBadge)}</span>
        <span class="route-preset-btn__name">${escapeHtml(p.shortLabel)}</span>
        <span class="route-preset-btn__slug">${escapeHtml(p.logicalModelId)}</span>
      </button>
    `,
  ).join('');
}

function updateHeroWireReadout(preset: ModelRoutePreset) {
  const badgeEl = document.getElementById('ro-badge');
  const protoEl = document.getElementById('ro-protocol');
  const summaryEl = document.getElementById('ro-summary');
  const capsEl = document.getElementById('ro-caps');
  const modelsEl = document.getElementById('ro-models');
  const authEl = document.getElementById('ro-auth');
  const endpointsEl = document.getElementById('ro-endpoints');
  const pipelineEl = document.getElementById('ro-pipeline');
  const benchKicker = document.getElementById('hero-bench-kicker');
  const benchDesc = document.getElementById('hero-bench-desc');

  if (badgeEl) badgeEl.textContent = preset.routeBadge;
  if (protoEl) protoEl.textContent = preset.protocolLabel;
  if (summaryEl) summaryEl.textContent = preset.summary;
  if (capsEl) {
    capsEl.innerHTML = preset.capabilities.map((c) => `<span>${escapeHtml(c)}</span>`).join('');
  }
  if (modelsEl) {
    modelsEl.textContent = `${preset.logicalModelId} ➔ ${preset.upstreamModelId}`;
  }
  if (authEl) {
    authEl.textContent = `${preset.authAction}（${preset.authDetail}）`;
  }
  if (endpointsEl) {
    endpointsEl.textContent = `127.0.0.1:8787/v1 ➔ ${preset.egressEndpoint}`;
  }
  if (pipelineEl) {
    pipelineEl.textContent = `${preset.bridgePipeline} · [${preset.contextWindow}]`;
  }
  if (benchKicker) {
    benchKicker.textContent = `OPTICAL BENCH // ${preset.routeBadge}`;
    benchKicker.style.color = preset.accentHex;
  }
  if (benchDesc) {
    benchDesc.textContent = `MEMS 偏航角 ${(preset.gimbalYawRad * (180 / Math.PI)).toFixed(1)}° · 钥匙箱状态：${
      preset.vaultActive ? '已啮合注入 Keyring 密钥' : '物理旁路（无需第三方密钥）'
    }`;
  }
}

function renderInsideStations() {
  const mount = document.getElementById('inside-stations-mount');
  const hudNav = document.getElementById('inside-hud-nav');
  if (!mount) return;

  mount.innerHTML = INTERNAL_MODULE_STAGES.map(
    (s, idx) => `
      <article
        class="module-stage-card ${idx === 0 ? 'is-active' : ''}"
        id="${s.id}"
        style="--stage-accent: ${s.accentHex}"
      >
        <div class="module-stage-meta">
          <span class="omni-kicker-mono" style="color:${s.accentHex}">${escapeHtml(s.indexLabel)}</span>
          <span class="module-crate-tag">${escapeHtml(s.crateName)}</span>
        </div>
        <h3>${escapeHtml(s.title)}</h3>
        <p class="module-stage-subtitle">${escapeHtml(s.subtitle)}</p>

        <div class="module-telemetry-bar">
          <code>${escapeHtml(s.telemetryCode)}</code>
        </div>

        <div class="module-spec-grid">
          ${s.specs
            .map(
              (sp) => `
            <div class="module-spec-item">
              <span>${escapeHtml(sp.label)}</span>
              <strong>${escapeHtml(sp.value)}</strong>
            </div>
          `,
            )
            .join('')}
        </div>

        <ul class="module-bullet-list">
          ${s.bullets.map((b) => `<li>${formatInlineCode(b)}</li>`).join('')}
        </ul>
      </article>
    `,
  ).join('');

  if (hudNav) {
    const shortNames = ['01 目录合成', '02 逐轮分流', '03 协议棱镜', '04 钥匙箱隔离'];
    hudNav.innerHTML = INTERNAL_MODULE_STAGES.map(
      (s, idx) => `
        <button
          type="button"
          class="inside-jump-btn ${idx === 0 ? 'is-active' : ''}"
          data-jump-stage="${s.id}"
        >
          ${escapeHtml(shortNames[idx] || s.title)}
        </button>
      `,
    ).join('');

    hudNav.querySelectorAll<HTMLButtonElement>('[data-jump-stage]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const id = btn.dataset.jumpStage;
        const el = id ? document.getElementById(id) : null;
        el?.scrollIntoView({ behavior: 'smooth', block: 'center' });
      });
    });
  }
}

function renderWorkspaceCrates() {
  const mount = document.getElementById('crates-grid-mount');
  if (!mount) return;

  mount.innerHTML = WORKSPACE_CRATES.map(
    (c) => `
      <article class="crate-card">
        <span class="crate-card__layer">${escapeHtml(c.layer)}</span>
        <strong class="crate-card__name">${escapeHtml(c.crate)}</strong>
        <p class="crate-card__desc">${escapeHtml(c.responsibility)}</p>
        <div class="crate-card__highlight">✦ ${escapeHtml(c.highlight)}</div>
      </article>
    `,
  ).join('');
}

bootstrap();
