import {
  MODEL_ROUTE_PRESETS,
  THREAD_SIMULATION_STEPS,
  type ModelRoutePreset,
} from '../data/productData';

export class ThreadSimulator {
  private readonly root: HTMLElement;
  private activeIdx = 0;
  private autoTimer: number | null = null;
  private readonly onSelectPreset: (preset: ModelRoutePreset) => void;

  constructor(root: HTMLElement, onSelectPreset: (preset: ModelRoutePreset) => void) {
    this.root = root;
    this.onSelectPreset = onSelectPreset;
    this.render();
  }

  private getPreset(presetId: string): ModelRoutePreset {
    return MODEL_ROUTE_PRESETS.find((p) => p.id === presetId) || MODEL_ROUTE_PRESETS[0];
  }

  private selectStep(idx: number, fromUser = false) {
    if (fromUser && this.autoTimer !== null) {
      this.stopAutoPlay();
    }
    this.activeIdx = (idx + THREAD_SIMULATION_STEPS.length) % THREAD_SIMULATION_STEPS.length;
    const step = THREAD_SIMULATION_STEPS[this.activeIdx];
    const preset = this.getPreset(step.presetId);
    this.onSelectPreset(preset);
    this.updateDOM();
  }

  private toggleAutoPlay() {
    if (this.autoTimer !== null) {
      this.stopAutoPlay();
    } else {
      this.selectStep((this.activeIdx + 1) % THREAD_SIMULATION_STEPS.length, false);
      this.autoTimer = window.setInterval(() => {
        this.selectStep((this.activeIdx + 1) % THREAD_SIMULATION_STEPS.length, false);
      }, 3600);
      this.updateDOM();
    }
  }

  private stopAutoPlay() {
    if (this.autoTimer !== null) {
      window.clearInterval(this.autoTimer);
      this.autoTimer = null;
      this.updateDOM();
    }
  }

  private render() {
    this.root.innerHTML = `
      <div class="thread-sim-shell">
        <div class="thread-sim-toolbar">
          <div class="thread-sim-steps" role="tablist" aria-label="同会话四轮模型热切换推演">
            ${THREAD_SIMULATION_STEPS.map(
              (s, i) => `
              <button
                type="button"
                role="tab"
                class="thread-step-btn ${i === 0 ? 'is-active' : ''}"
                aria-selected="${i === 0 ? 'true' : 'false'}"
                data-thread-step="${i}"
              >
                <span class="thread-step-num">0${s.turnIndex}</span>
                <span class="thread-step-model">${s.modelId}</span>
              </button>
            `,
            ).join('')}
          </div>
          <button type="button" class="omni-pill-btn omni-pill-btn--accent" id="thread-autoplay-btn">
            <span id="thread-autoplay-label">▶ 自动推演四轮切换</span>
          </button>
        </div>

        <div class="thread-sim-grid">
          <!-- Left: Stock Codex Interactive Session Transcript -->
          <div class="thread-terminal-card">
            <div class="thread-terminal-head">
              <div class="terminal-dots" aria-hidden="true">
                <span></span><span></span><span></span>
              </div>
              <div class="terminal-title-mono">
                codex — thread <code>thr_019a8c7e</code> · provider=<code>omnibridge</code>
              </div>
              <span class="terminal-latency-chip" id="thread-latency-chip"></span>
            </div>

            <div class="thread-transcript" id="thread-transcript-list" aria-live="polite"></div>
          </div>

          <!-- Right: Loopback Router Wire & Header Isolation Inspector -->
          <div class="thread-wire-card">
            <div class="thread-wire-head">
              <span class="omni-kicker-mono">WIRE INSPECTOR // 127.0.0.1:8787</span>
              <span class="thread-route-pill" id="thread-route-badge"></span>
            </div>

            <p class="thread-route-summary" id="thread-route-summary"></p>

            <div class="wire-columns">
              <div class="wire-box wire-box--pass">
                <div class="wire-box-title">
                  <span class="wire-dot wire-dot--pass"></span>
                  <span>转发至上游的请求头 / 凭据</span>
                </div>
                <ul class="wire-list" id="thread-headers-sent"></ul>
              </div>

              <div class="wire-box wire-box--strip">
                <div class="wire-box-title">
                  <span class="wire-dot wire-dot--strip"></span>
                  <span>Router 物理拦截与跨源净化项</span>
                </div>
                <ul class="wire-list" id="thread-headers-stripped"></ul>
              </div>
            </div>

            <div class="wire-history-ledger">
              <span class="omni-kicker-mono">CODEX_CHAT_HISTORY_STORE LEDGER</span>
              <div class="ledger-track" id="thread-ledger-track"></div>
            </div>
          </div>
        </div>
      </div>
    `;

    this.root.querySelectorAll<HTMLButtonElement>('[data-thread-step]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const idx = Number(btn.dataset.threadStep || '0');
        this.selectStep(idx, true);
      });
    });

    this.root.querySelector('#thread-autoplay-btn')?.addEventListener('click', () => {
      this.toggleAutoPlay();
    });

    this.updateDOM();
  }

  private updateDOM() {
    const step = THREAD_SIMULATION_STEPS[this.activeIdx];
    const preset = this.getPreset(step.presetId);

    this.root.querySelectorAll<HTMLButtonElement>('[data-thread-step]').forEach((btn, i) => {
      const active = i === this.activeIdx;
      btn.classList.toggle('is-active', active);
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
      if (active) {
        btn.style.setProperty('--step-accent', preset.accentHex);
      }
    });

    const autoLabel = this.root.querySelector('#thread-autoplay-label');
    if (autoLabel) {
      autoLabel.textContent = this.autoTimer !== null ? '⏸ 暂停自动推演' : '▶ 自动推演四轮切换';
    }

    const latencyEl = this.root.querySelector('#thread-latency-chip');
    if (latencyEl) {
      latencyEl.textContent = step.latencyTag;
      (latencyEl as HTMLElement).style.color = preset.accentHex;
    }

    const badgeEl = this.root.querySelector('#thread-route-badge') as HTMLElement | null;
    if (badgeEl) {
      badgeEl.textContent = step.badge;
      badgeEl.style.borderColor = `${preset.accentHex}66`;
      badgeEl.style.color = preset.accentHex;
    }

    const summaryEl = this.root.querySelector('#thread-route-summary');
    if (summaryEl) {
      summaryEl.textContent = step.routeSummary;
    }

    // Render cumulative transcript up to activeIdx so user feels genuine single-thread continuity
    const transcriptEl = this.root.querySelector('#thread-transcript-list');
    if (transcriptEl) {
      transcriptEl.innerHTML = THREAD_SIMULATION_STEPS.slice(0, this.activeIdx + 1)
        .map((item, idx) => {
          const itemPreset = this.getPreset(item.presetId);
          const isCurrent = idx === this.activeIdx;
          return `
            <div class="transcript-turn ${isCurrent ? 'is-current' : 'is-past'}" style="--turn-accent: ${itemPreset.accentHex}">
              ${
                item.slashSwitch
                  ? `<div class="transcript-slash"><code>${escapeHtml(item.slashSwitch)}</code> <span class="slash-ok">✓ Router 已就地切换该轮模型（无需重开会话）</span></div>`
                  : ''
              }
              <div class="transcript-user">
                <span class="transcript-role">USER · T0${item.turnIndex}</span>
                <p>${escapeHtml(item.commandOrPrompt)}</p>
              </div>
              <div class="transcript-assistant">
                <span class="transcript-role" style="color:${itemPreset.accentHex}">${escapeHtml(item.modelId)}</span>
                <p>${escapeHtml(item.assistantPreview)}</p>
              </div>
            </div>
          `;
        })
        .join('');
      transcriptEl.scrollTop = transcriptEl.scrollHeight;
    }

    const sentEl = this.root.querySelector('#thread-headers-sent');
    if (sentEl) {
      sentEl.innerHTML = step.headersSent.map((h) => `<li><code>${escapeHtml(h)}</code></li>`).join('');
    }

    const strippedEl = this.root.querySelector('#thread-headers-stripped');
    if (strippedEl) {
      strippedEl.innerHTML = step.headersStripped
        .map((h) => `<li><code>${escapeHtml(h)}</code></li>`)
        .join('');
    }

    const ledgerEl = this.root.querySelector('#thread-ledger-track');
    if (ledgerEl) {
      ledgerEl.innerHTML = THREAD_SIMULATION_STEPS.map((t, i) => {
        const tp = this.getPreset(t.presetId);
        const stateClass = i < this.activeIdx ? 'is-hydrated' : i === this.activeIdx ? 'is-live' : 'is-queued';
        return `
          <div class="ledger-node ${stateClass}" style="--node-accent:${tp.accentHex}">
            <span class="ledger-turn">T0${t.turnIndex}</span>
            <span class="ledger-model">${escapeHtml(t.modelId)}</span>
            <span class="ledger-state">${
              i < this.activeIdx ? '已归档可移植上下文' : i === this.activeIdx ? '当前活跃流' : '待执行'
            }</span>
          </div>
        `;
      }).join('');
    }
  }
}

function escapeHtml(raw: string): string {
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
