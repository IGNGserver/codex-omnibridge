import {
  MODEL_ROUTE_PRESETS,
  THREAD_SIMULATION_STEPS,
  type ModelRoutePreset,
} from '../data/productData';

const PLAY_ICON_SVG =
  '<svg class="omni-svg-icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M8 5.14v14l11-7-11-7Z"/></svg>';
const PAUSE_ICON_SVG =
  '<svg class="omni-svg-icon" viewBox="0 0 24 24" aria-hidden="true"><path fill="currentColor" d="M6 19h4V5H6v14Zm8-14v14h4V5h-4Z"/></svg>';

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
          <div class="thread-sim-steps" role="tablist" aria-label="同一条对话内切换四种模型演示">
            ${THREAD_SIMULATION_STEPS.map((s, i) => {
              const stepPreset = this.getPreset(s.presetId);
              return `
                <button
                  type="button"
                  role="tab"
                  class="thread-step-btn ${i === 0 ? 'is-active' : ''}"
                  data-route-kind="${stepPreset.routeKind}"
                  aria-selected="${i === 0 ? 'true' : 'false'}"
                  data-thread-step="${i}"
                >
                  <span class="thread-step-num">第 ${s.turnIndex} 轮</span>
                  <span class="thread-step-model">${escapeHtml(s.modelId)}</span>
                </button>
              `;
            }).join('')}
          </div>
          <button type="button" class="m3-btn m3-btn--tonal" id="thread-autoplay-btn">
            <span class="thread-autoplay-icon" id="thread-autoplay-icon">${PLAY_ICON_SVG}</span>
            <span id="thread-autoplay-label">自动演示四轮切换</span>
          </button>
        </div>

        <div class="thread-sim-grid">
          <div class="thread-terminal-card">
            <div class="thread-terminal-head">
              <div class="terminal-dots" aria-hidden="true">
                <span></span><span></span><span></span>
              </div>
              <div class="terminal-title-mono">
                codex — 会话 <code>thr_019a8c7e</code> · 经由本机网关分流
              </div>
              <span class="terminal-latency-chip" id="thread-latency-chip"></span>
            </div>

            <div class="thread-transcript" id="thread-transcript-list" aria-live="polite"></div>
          </div>

          <div class="thread-wire-card">
            <div class="thread-wire-head">
              <span class="omni-kicker-mono">本机分流检测 · 127.0.0.1:8787</span>
              <span class="thread-route-pill" id="thread-route-badge"></span>
            </div>

            <p class="thread-route-summary" id="thread-route-summary"></p>

            <div class="wire-columns">
              <div class="wire-box wire-box--pass">
                <div class="wire-box-title">
                  <span class="wire-dot wire-dot--pass"></span>
                  <span>发往目标模型的凭据与信息</span>
                </div>
                <ul class="wire-list" id="thread-headers-sent"></ul>
              </div>

              <div class="wire-box wire-box--strip">
                <div class="wire-box-title">
                  <span class="wire-dot wire-dot--strip"></span>
                  <span>已自动拦截与清除的内容</span>
                </div>
                <ul class="wire-list" id="thread-headers-stripped"></ul>
              </div>
            </div>

            <div class="wire-history-ledger">
              <span class="omni-kicker-mono">跨模型上下文接力记录</span>
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
    });

    const autoIcon = this.root.querySelector('#thread-autoplay-icon');
    const autoLabel = this.root.querySelector('#thread-autoplay-label');
    if (autoIcon) {
      autoIcon.innerHTML = this.autoTimer !== null ? PAUSE_ICON_SVG : PLAY_ICON_SVG;
    }
    if (autoLabel) {
      autoLabel.textContent = this.autoTimer !== null ? '暂停自动演示' : '自动演示四轮切换';
    }

    const latencyEl = this.root.querySelector<HTMLElement>('#thread-latency-chip');
    if (latencyEl) {
      latencyEl.textContent = step.latencyTag;
      latencyEl.dataset.routeKind = preset.routeKind;
    }

    const badgeEl = this.root.querySelector<HTMLElement>('#thread-route-badge');
    if (badgeEl) {
      badgeEl.textContent = step.badge;
      badgeEl.dataset.routeKind = preset.routeKind;
    }

    const summaryEl = this.root.querySelector('#thread-route-summary');
    if (summaryEl) {
      summaryEl.textContent = step.routeSummary;
    }

    const transcriptEl = this.root.querySelector('#thread-transcript-list');
    if (transcriptEl) {
      transcriptEl.innerHTML = THREAD_SIMULATION_STEPS.slice(0, this.activeIdx + 1)
        .map((item, idx) => {
          const itemPreset = this.getPreset(item.presetId);
          const isCurrent = idx === this.activeIdx;
          return `
            <div class="transcript-turn ${isCurrent ? 'is-current' : 'is-past'}" data-route-kind="${itemPreset.routeKind}">
              ${
                item.slashSwitch
                  ? `<div class="transcript-slash"><code>${escapeHtml(item.slashSwitch)}</code> <span class="slash-ok">已就地切换本轮模型，无需重开会话</span></div>`
                  : ''
              }
              <div class="transcript-user">
                <span class="transcript-role">用户 · 第 ${item.turnIndex} 轮</span>
                <p>${escapeHtml(item.commandOrPrompt)}</p>
              </div>
              <div class="transcript-assistant">
                <span class="transcript-role is-model">${escapeHtml(item.modelId)}</span>
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
          <div class="ledger-node ${stateClass}" data-route-kind="${tp.routeKind}">
            <span class="ledger-turn">第 ${t.turnIndex} 轮</span>
            <span class="ledger-model">${escapeHtml(t.modelId)}</span>
            <span class="ledger-state">${
              i < this.activeIdx ? '已接入共享记忆' : i === this.activeIdx ? '当前正在回答' : '待演示'
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
