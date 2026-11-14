import { MODEL_ROUTE_PRESETS, type ModelRoutePreset } from '../data/productData';

interface SandboxAccount {
  id: string;
  name: string;
  email: string;
  plan: 'plus' | 'pro' | 'team' | 'free';
  isActive: boolean;
  primary5h: number;
  weekly7d: number;
  reserve: number | null;
}

interface SandboxModel {
  logicalId: string;
  displayName: string;
  upstreamId: string;
  contextWindow: string;
  enabled: boolean;
  presetId: string;
  caps: string[];
  reasoningLevels: string[];
}

interface SandboxProvider {
  id: string;
  name: string;
  baseUrl: string;
  protocol: 'chat_completions' | 'responses';
  authStrategy: 'bearer' | 'api_key' | 'none';
  models: SandboxModel[];
}

interface AuthBackupItem {
  id: string;
  filename: string;
  previousAccountName: string;
  createdAt: string;
}

const QUICK_PROVIDER_TEMPLATES: Array<{
  label: string;
  name: string;
  baseUrl: string;
  protocol: 'chat_completions' | 'responses';
  authStrategy: 'bearer' | 'api_key' | 'none';
  discoveredModels: SandboxModel[];
}> = [
  {
    label: '填入 OneAPI 团队网关示例',
    name: 'OneAPI 研发中枢',
    baseUrl: 'https://oneapi.team.example/v1',
    protocol: 'chat_completions',
    authStrategy: 'bearer',
    discoveredModels: [
      {
        logicalId: 'oneapi/deepseek-v4.1',
        displayName: 'OneAPI / DeepSeek V4.1',
        upstreamId: 'deepseek-v4.1',
        contextWindow: '128K',
        enabled: true,
        presetId: 'deepseek-oneapi',
        caps: ['文本', '工具调用', '流式输出', '支持思考'],
        reasoningLevels: ['medium', 'high'],
      },
      {
        logicalId: 'oneapi/qwen4-max',
        displayName: 'OneAPI / Qwen4 Max',
        upstreamId: 'qwen4-max',
        contextWindow: '128K',
        enabled: true,
        presetId: 'qwen-newapi',
        caps: ['文本', '图片', '工具调用', '流式输出', '支持思考'],
        reasoningLevels: ['low', 'medium', 'high'],
      },
    ],
  },
  {
    label: '填入本地 vLLM (:8000) 示例',
    name: 'vLLM 本地推理集群',
    baseUrl: 'http://127.0.0.1:8000/v1',
    protocol: 'chat_completions',
    authStrategy: 'none',
    discoveredModels: [
      {
        logicalId: 'vllm/qwen3-coder-32b',
        displayName: 'vLLM / Qwen3 Coder 32B',
        upstreamId: 'Qwen/Qwen3-Coder-32B-Instruct',
        contextWindow: '128K',
        enabled: true,
        presetId: 'ollama-local',
        caps: ['文本', '工具调用', '流式输出'],
        reasoningLevels: [],
      },
    ],
  },
];

export class PanelSandbox {
  private readonly root: HTMLElement;
  private activeTab: 'overview' | 'accounts' | 'providers' | 'settings' = 'overview';
  private panelTheme: 'dark' | 'light' = 'dark';
  private panelContrast: 'standard' | 'high' = 'standard';
  private panelMotion: 'full' | 'reduced' = 'full';
  private filterQuery = '';
  private filterEnabledOnly = false;
  private catalogPending = false;
  private generation = 4;
  private addDrawerOpen = false;
  private remoteAccessEnabled = false;
  private passwordConfigured = true;
  private snackbarText = '';
  private snackbarTimer: number | null = null;

  private accounts: SandboxAccount[] = [
    {
      id: 'acc_plus',
      name: 'Plus 主号',
      email: 'architect.plus@example.com',
      plan: 'plus',
      isActive: true,
      primary5h: 38,
      weekly7d: 64,
      reserve: 12,
    },
    {
      id: 'acc_pro',
      name: 'Pro 深度号',
      email: 'research.pro@example.com',
      plan: 'pro',
      isActive: false,
      primary5h: 14,
      weekly7d: 29,
      reserve: 0,
    },
    {
      id: 'acc_team',
      name: 'Team 协作号',
      email: 'eng.team@example.com',
      plan: 'team',
      isActive: false,
      primary5h: 9,
      weekly7d: 22,
      reserve: null,
    },
  ];

  private backups: AuthBackupItem[] = [
    {
      id: 'bak_init',
      filename: 'auth.bak-switch-1763090112-8a4f.json',
      previousAccountName: 'Pro 深度号',
      createdAt: '09:42:18 · 权限 0600',
    },
  ];

  private providers: SandboxProvider[] = [
    {
      id: 'openrouter',
      name: 'OpenRouter 主线路',
      baseUrl: 'https://openrouter.ai/api/v1',
      protocol: 'chat_completions',
      authStrategy: 'bearer',
      models: [
        {
          logicalId: 'openrouter/claude-opus-4.6',
          displayName: 'OpenRouter / Claude Opus 4.6',
          upstreamId: 'anthropic/claude-opus-4.6',
          contextWindow: '200K',
          enabled: true,
          presetId: 'claude-openrouter',
          caps: ['文本', '图片', '文件', '工具调用', '流式输出', '支持思考'],
          reasoningLevels: ['low', 'medium', 'high'],
        },
      ],
    },
    {
      id: 'newapi',
      name: 'NewAPI 聚合网关',
      baseUrl: 'https://api.newapi.internal/v1',
      protocol: 'chat_completions',
      authStrategy: 'bearer',
      models: [
        {
          logicalId: 'newapi/qwen4-max',
          displayName: 'NewAPI / Qwen4 Max',
          upstreamId: 'qwen4-max',
          contextWindow: '128K',
          enabled: true,
          presetId: 'qwen-newapi',
          caps: ['文本', '图片', '工具调用', '流式输出', '支持思考'],
          reasoningLevels: ['low', 'medium', 'high', 'xhigh'],
        },
        {
          logicalId: 'newapi/gemini-3.8-flash',
          displayName: 'NewAPI / Gemini 3.8 Flash',
          upstreamId: 'gemini-3.8-flash',
          contextWindow: '1,048K',
          enabled: true,
          presetId: 'gemini-newapi',
          caps: ['文本', '图片', '文件', '流式输出'],
          reasoningLevels: [],
        },
      ],
    },
    {
      id: 'ollama',
      name: 'Ollama 本机推理',
      baseUrl: 'http://127.0.0.1:11434/v1',
      protocol: 'chat_completions',
      authStrategy: 'none',
      models: [
        {
          logicalId: 'ollama/qwen3-coder',
          displayName: 'Ollama / Qwen3 Coder',
          upstreamId: 'qwen3-coder:30b',
          contextWindow: '128K',
          enabled: true,
          presetId: 'ollama-local',
          caps: ['文本', '工具调用', '流式输出'],
          reasoningLevels: [],
        },
      ],
    },
  ];

  private readonly onRoutePreview: (preset: ModelRoutePreset) => void;
  private readonly onSyncCatalog: () => void;

  constructor(
    root: HTMLElement,
    callbacks: {
      onRoutePreview: (preset: ModelRoutePreset) => void;
      onSyncCatalog: () => void;
    },
  ) {
    this.root = root;
    this.onRoutePreview = callbacks.onRoutePreview;
    this.onSyncCatalog = callbacks.onSyncCatalog;
    this.render();
  }

  private notify(msg: string) {
    this.snackbarText = msg;
    if (this.snackbarTimer !== null) window.clearTimeout(this.snackbarTimer);
    this.render();
    this.snackbarTimer = window.setTimeout(() => {
      this.snackbarText = '';
      const bar = this.root.querySelector('.m3s-snackbar');
      bar?.classList.remove('is-visible');
    }, 3200);
  }

  private getEnabledModels(): SandboxModel[] {
    return this.providers.flatMap((p) => p.models.filter((m) => m.enabled));
  }

  private getTotalModelsCount(): number {
    return this.providers.reduce((acc, p) => acc + p.models.length, 0);
  }

  private getActiveAccount(): SandboxAccount {
    return this.accounts.find((a) => a.isActive) || this.accounts[0];
  }

  private syncToCodex() {
    this.catalogPending = false;
    this.generation += 1;
    this.onSyncCatalog();
    this.notify(
      `✓ 已生成并原子写入 models.json（generation #${this.generation}，共 ${this.getEnabledModels().length} 个自定义模型 + 官方内置目录）`,
    );
  }

  private switchAccount(accId: string) {
    const prev = this.getActiveAccount();
    if (prev.id === accId) return;
    const next = this.accounts.find((a) => a.id === accId);
    if (!next) return;

    for (const a of this.accounts) a.isActive = a.id === accId;
    const now = new Date();
    const hh = String(now.getHours()).padStart(2, '0');
    const mm = String(now.getMinutes()).padStart(2, '0');
    const ss = String(now.getSeconds()).padStart(2, '0');
    this.backups.unshift({
      id: `bak_${Date.now()}`,
      filename: `auth.bak-switch-${Date.now()}-f21c.json`,
      previousAccountName: prev.name,
      createdAt: `${hh}:${mm}:${ss} · 权限 0600`,
    });
    this.backups = this.backups.slice(0, 3); // MAX_AUTH_BACKUPS = 3
    this.notify(`✓ 已原子切换至「${next.name}」，原 auth.json 已自动留存 0600 备份`);
  }

  private restoreBackup(bakId: string) {
    const bak = this.backups.find((b) => b.id === bakId);
    if (!bak) return;
    const match = this.accounts.find((a) => a.name === bak.previousAccountName);
    if (match) {
      for (const a of this.accounts) a.isActive = a.id === match.id;
    }
    this.backups = this.backups.filter((b) => b.id !== bakId);
    this.notify(`↺ 已从备份恢复「${bak.previousAccountName}」登录态`);
  }

  private render() {
    const activeAcc = this.getActiveAccount();
    const enabledModels = this.getEnabledModels();
    const totalModels = this.getTotalModelsCount();

    this.root.innerHTML = `
      <div class="panel-showcase-grid">
        <!-- Authentic Embedded M3 Expressive Control Panel -->
        <div
          class="m3s-window"
          data-panel-theme="${this.panelTheme}"
          data-panel-contrast="${this.panelContrast}"
          data-panel-motion="${this.panelMotion}"
        >
          <!-- Top App Bar (64dp) -->
          <header class="m3s-topbar">
            <div class="m3s-topbar__lead">
              <img src="./app-icon.png" alt="" width="34" height="34" class="m3s-brand-icon" />
              <div class="m3s-brand-meta">
                <strong>Codex OmniBridge</strong>
                <span>v0.1.2 · 127.0.0.1:31828</span>
              </div>
            </div>

            <div class="m3s-topbar__trail">
              <span class="m3s-chip m3s-chip--ok" role="status">
                <span class="material-symbols-outlined m3-i-router" aria-hidden="true"></span>
                <span class="m3s-hide-xs">后台路由正常 · :8787</span>
              </span>

              <button
                type="button"
                class="m3s-btn ${this.catalogPending ? 'm3s-btn--filled is-pulsing' : 'm3s-btn--tonal'}"
                id="m3s-sync-btn"
              >
                <span class="material-symbols-outlined m3-i-sync" aria-hidden="true"></span>
                <span>${this.catalogPending ? '有待应用变更 · 应用到 Codex' : '应用到 Codex'}</span>
              </button>

              <button
                type="button"
                class="m3s-icon-btn"
                id="m3s-quick-theme-btn"
                aria-label="切换面板浅色或深色主题"
                title="切换面板主题（浅色 / 深色）"
              >
                <span
                  class="material-symbols-outlined ${this.panelTheme === 'dark' ? 'm3-i-light-mode' : 'm3-i-dark-mode'}"
                  aria-hidden="true"
                ></span>
              </button>
            </div>
          </header>

          <div class="m3s-body">
            <!-- Adaptive Navigation Rail / Drawer / Bottom Bar -->
            <nav class="m3s-nav" aria-label="控制中心演示导航">
              <button
                type="button"
                class="m3s-nav-item ${this.activeTab === 'overview' ? 'is-active' : ''}"
                data-m3s-tab="overview"
              >
                <span class="material-symbols-outlined m3-i-dashboard" aria-hidden="true"></span>
                <span>开始使用</span>
              </button>
              <button
                type="button"
                class="m3s-nav-item ${this.activeTab === 'accounts' ? 'is-active' : ''}"
                data-m3s-tab="accounts"
              >
                <span class="material-symbols-outlined m3-i-manage-accounts" aria-hidden="true"></span>
                <span>官方账号</span>
              </button>
              <button
                type="button"
                class="m3s-nav-item ${this.activeTab === 'providers' ? 'is-active' : ''}"
                data-m3s-tab="providers"
              >
                <span class="material-symbols-outlined m3-i-neurology" aria-hidden="true"></span>
                <span>模型</span>
              </button>
              <button
                type="button"
                class="m3s-nav-item ${this.activeTab === 'settings' ? 'is-active' : ''}"
                data-m3s-tab="settings"
              >
                <span class="material-symbols-outlined m3-i-tune" aria-hidden="true"></span>
                <span>设置</span>
              </button>

              <div class="m3s-nav-foot">
                <span>Core gen #${this.generation}</span>
                <span>无头多模型桥接路由器</span>
              </div>
            </nav>

            <!-- Main Panel Viewport -->
            <div class="m3s-main">
              ${this.activeTab === 'overview' ? this.renderOverviewTab(activeAcc, enabledModels.length, totalModels) : ''}
              ${this.activeTab === 'accounts' ? this.renderAccountsTab() : ''}
              ${this.activeTab === 'providers' ? this.renderProvidersTab() : ''}
              ${this.activeTab === 'settings' ? this.renderSettingsTab() : ''}
            </div>
          </div>

          <div class="m3s-snackbar ${this.snackbarText ? 'is-visible' : ''}" role="status" aria-live="polite">
            ${escapeHtml(this.snackbarText)}
          </div>
        </div>

        <!-- Live Synced Codex /model Picker Sidecar -->
        <aside class="codex-picker-sidecar" aria-label="同步后的 Codex /model 模型选择器实时预览">
          <div class="picker-sidecar-head">
            <span class="omni-kicker-mono">LIVE CODEX CLI · /model PICKER</span>
            <span class="picker-gen-tag">models.json · gen #${this.generation}</span>
          </div>
          <p class="picker-sidecar-desc">
            在左侧面板开关任意模型或添加服务商，点击「应用到 Codex」即可实时刷新下方选择器；点击任一模型将联动上方 3D 光学矩阵切换光路：
          </p>

          <div class="picker-list" role="list">
            <div class="picker-group-label">官方内置模型（OAuth 原生透传）</div>
            <button type="button" class="picker-model-row" data-preview-preset="gpt-official">
              <span class="picker-dot" style="background:#00daf2"></span>
              <span class="picker-slug">gpt-5.4</span>
              <span class="picker-meta">Official · 400K · Tools + Image</span>
            </button>

            <div class="picker-group-label">OmniBridge 注入模型（共 ${enabledModels.length} 个已启用）</div>
            ${enabledModels
              .map((m) => {
                const preset =
                  MODEL_ROUTE_PRESETS.find((p) => p.id === m.presetId) || MODEL_ROUTE_PRESETS[1];
                return `
                  <button type="button" class="picker-model-row" data-preview-preset="${m.presetId}">
                    <span class="picker-dot" style="background:${preset.accentHex}"></span>
                    <span class="picker-slug">${escapeHtml(m.logicalId)}</span>
                    <span class="picker-meta">${escapeHtml(m.contextWindow)}</span>
                  </button>
                `;
              })
              .join('')}
          </div>

          <div class="picker-toml-preview">
            <div class="picker-toml-head">
              <span>~/.codex/config.toml（toml_edit 受管片段）</span>
              <code>auth.json 零触碰</code>
            </div>
            <pre><code>model_provider = "omnibridge"
model_catalog_json = "~/.config/codex-multiprovider/models.json"

[model_providers.omnibridge]
name = "OpenAI"
base_url = "http://127.0.0.1:8787/v1"
wire_api = "responses"
requires_openai_auth = true</code></pre>
          </div>
        </aside>
      </div>
    `;

    this.bindSandboxEvents();
  }

  private renderOverviewTab(
    activeAcc: SandboxAccount,
    enabledCount: number,
    totalCount: number,
  ): string {
    return `
      <div class="m3s-view">
        <div class="m3s-view-head">
          <div>
            <h3>开始使用</h3>
            <p>完成一次三步配置后，所有已启用模型直接出现在 Codex 的 <code>/model</code> 选择列表中。</p>
          </div>
        </div>

        <div class="m3s-card m3s-card--filled">
          <div class="m3s-card-row">
            <div>
              <span class="m3s-kicker">接入进度</span>
              <h4>三步将第三方与本地模型接入官方 Codex</h4>
            </div>
            <span class="m3s-chip ${this.catalogPending ? 'm3s-chip--warn' : 'm3s-chip--ok'}">
              ${this.catalogPending ? '有待同步变更' : '目录已同步'}
            </span>
          </div>

          <ol class="m3s-steps">
            <li>
              <span class="m3s-step-badge is-done">✓</span>
              <div class="m3s-step-text">
                <strong>1. 添加服务商</strong>
                <span>支持 NewAPI、OneAPI、OpenRouter 或本机 Ollama / vLLM，密钥仅入系统钥匙串。</span>
              </div>
              <button type="button" class="m3s-btn m3s-btn--tonal m3s-btn--xs" data-goto-add-provider>添加服务商</button>
            </li>
            <li>
              <span class="m3s-step-badge is-done">✓</span>
              <div class="m3s-step-text">
                <strong>2. 选择并配置模型</strong>
                <span>自动发现 <code>/models</code> 清单，按需调整上下文窗口与思考档位（low~xhigh）。</span>
              </div>
              <button type="button" class="m3s-btn m3s-btn--text m3s-btn--xs" data-m3s-jump="providers">管理模型 (${enabledCount}/${totalCount})</button>
            </li>
            <li>
              <span class="m3s-step-badge ${this.catalogPending ? '' : 'is-done'}">${this.catalogPending ? '3' : '✓'}</span>
              <div class="m3s-step-text">
                <strong>3. 应用到 Codex</strong>
                <span>合成 Schema 兼容的 <code>models.json</code> 并热重载本地 Router 凭据代际缓存。</span>
              </div>
              <button type="button" class="m3s-btn m3s-btn--filled m3s-btn--xs" data-trigger-sync>立即应用</button>
            </li>
          </ol>
        </div>

        <div class="m3s-metrics">
          <div class="m3s-metric m3s-metric--primary">
            <span class="m3s-metric-label">后台路由状态</span>
            <strong class="m3s-metric-val">运行中 · :8787</strong>
            <small>Loopback 常量时间鉴权正常</small>
          </div>
          <div class="m3s-metric m3s-metric--primary">
            <span class="m3s-metric-label">当前生效账号</span>
            <strong class="m3s-metric-val">${escapeHtml(activeAcc.name)}</strong>
            <small>${activeAcc.plan.toUpperCase()} 方案 · 登录态未受干扰</small>
          </div>
          <div class="m3s-metric">
            <span class="m3s-metric-label">已连接服务商</span>
            <strong class="m3s-metric-val">${this.providers.length}</strong>
            <small>已启用 ${enabledCount} / 共 ${totalCount} 个模型</small>
          </div>
          <div class="m3s-metric">
            <span class="m3s-metric-label">集成模式</span>
            <strong class="m3s-metric-val">Stock 原厂</strong>
            <small>未修改官方 codex 可执行文件</small>
          </div>
        </div>

        <div class="m3s-card m3s-card--filled">
          <div class="m3s-card-row">
            <div>
              <h4>当前官方账号实时额度（${escapeHtml(activeAcc.name)}）</h4>
              <p class="m3s-muted">直连 <code>chatgpt.com/backend-api/wham/usage</code> 查询 5 小时、7 天与 Reserve 窗口。</p>
            </div>
            <button type="button" class="m3s-btn m3s-btn--outlined m3s-btn--xs" data-m3s-jump="accounts">切换账号</button>
          </div>
          <div class="m3s-quota-bars">
            <div class="m3s-quota-item">
              <div class="m3s-quota-top"><span>5 小时主窗口</span><strong class="m3-numeric">${activeAcc.primary5h}%</strong></div>
              <div class="m3s-progress"><span style="width:${activeAcc.primary5h}%"></span></div>
            </div>
            <div class="m3s-quota-item">
              <div class="m3s-quota-top"><span>7 天周期窗口</span><strong class="m3-numeric">${activeAcc.weekly7d}%</strong></div>
              <div class="m3s-progress"><span style="width:${activeAcc.weekly7d}%"></span></div>
            </div>
            <div class="m3s-quota-item">
              <div class="m3s-quota-top"><span>Reserve 备用池</span><strong class="m3-numeric">${activeAcc.reserve !== null ? `${activeAcc.reserve}%` : '无配额项'}</strong></div>
              <div class="m3s-progress"><span style="width:${activeAcc.reserve ?? 0}%"></span></div>
            </div>
          </div>
        </div>
      </div>
    `;
  }

  private renderAccountsTab(): string {
    return `
      <div class="m3s-view">
        <div class="m3s-view-head">
          <div>
            <h3>官方账号（可选托管）</h3>
            <p>不保存账号也可正常使用多模型路由；托管多个 Plus / Pro / Team 账号后支持一键原子切换与额度巡检。</p>
          </div>
        </div>

        <div class="m3s-account-grid">
          ${this.accounts
            .map(
              (acc) => `
            <div class="m3s-card ${acc.isActive ? 'm3s-card--active' : 'm3s-card--outlined'}">
              <div class="m3s-card-row">
                <div>
                  <strong>${escapeHtml(acc.name)}</strong>
                  <div class="m3s-muted m3s-mono-sm">${escapeHtml(acc.email)}</div>
                </div>
                <span class="m3s-plan-chip m3s-plan-chip--${acc.plan}">${acc.plan.toUpperCase()}</span>
              </div>

              <div class="m3s-quota-mini">
                <span>5h 已用 <strong>${acc.primary5h}%</strong></span>
                <span>7d 已用 <strong>${acc.weekly7d}%</strong></span>
              </div>

              <div class="m3s-card-actions">
                <button
                  type="button"
                  class="m3s-btn ${acc.isActive ? 'm3s-btn--tonal' : 'm3s-btn--filled'} m3s-btn--xs"
                  ${acc.isActive ? 'disabled' : ''}
                  data-switch-acc="${acc.id}"
                >
                  ${acc.isActive ? '✓ 当前正在使用' : '切换到此账号'}
                </button>
              </div>
            </div>
          `,
            )
            .join('')}
        </div>

        <div class="m3s-card m3s-card--outlined">
          <div class="m3s-card-row">
            <div>
              <h4>登录态自动备份（最多保留 3 份 0600 快照）</h4>
              <p class="m3s-muted">每次切换账号前自动备份原 <code>~/.codex/auth.json</code>，误切换或令牌轮换冲突可随时一键退回。</p>
            </div>
          </div>
          <ul class="m3s-backup-list">
            ${
              this.backups.length === 0
                ? `<li class="m3s-muted">暂无备份记录</li>`
                : this.backups
                    .map(
                      (b) => `
                  <li>
                    <div>
                      <code class="m3s-mono-sm">${escapeHtml(b.filename)}</code>
                      <span class="m3s-muted">（切换前账号：${escapeHtml(b.previousAccountName)} · ${escapeHtml(b.createdAt)}）</span>
                    </div>
                    <button type="button" class="m3s-btn m3s-btn--outlined m3s-btn--xs" data-restore-bak="${b.id}">退回此快照</button>
                  </li>
                `,
                    )
                    .join('')
            }
          </ul>
        </div>
      </div>
    `;
  }

  private renderProvidersTab(): string {
    const q = this.filterQuery.trim().toLowerCase();

    return `
      <div class="m3s-view">
        <div class="m3s-view-head">
          <div>
            <h3>模型与服务商</h3>
            <p>支持任意兼容 Chat Completions 或原生 Responses 的上游网关与本机模型推理服务。</p>
          </div>
          <button type="button" class="m3s-btn m3s-btn--filled m3s-btn--xs" id="m3s-toggle-add-provider">
            <span class="material-symbols-outlined m3-i-add" aria-hidden="true"></span>
            <span>${this.addDrawerOpen ? '收起表单' : '添加服务商'}</span>
          </button>
        </div>

        ${
          this.addDrawerOpen
            ? `
          <div class="m3s-card m3s-card--elevated m3s-add-drawer">
            <div class="m3s-card-row">
              <h4>添加服务商并自动发现 <code>/models</code></h4>
              <div class="m3s-preset-chips">
                ${QUICK_PROVIDER_TEMPLATES.map(
                  (tpl, idx) => `
                  <button type="button" class="m3s-btn m3s-btn--tonal m3s-btn--xs" data-fill-tpl="${idx}">
                    ${escapeHtml(tpl.label)}
                  </button>
                `,
                ).join('')}
              </div>
            </div>
            <form id="m3s-add-provider-form" class="m3s-form-grid">
              <label class="m3s-field">
                <span>服务商名称</span>
                <input type="text" name="name" id="m3s-inp-name" value="OneAPI 研发中枢" required />
              </label>
              <label class="m3s-field">
                <span>API 根地址（非环回强制 HTTPS）</span>
                <input type="text" name="base_url" id="m3s-inp-url" value="https://oneapi.team.example/v1" required />
              </label>
              <label class="m3s-field">
                <span>上游协议</span>
                <select name="protocol" id="m3s-inp-proto">
                  <option value="chat_completions">Chat Completions（标准 /v1/chat/completions）</option>
                  <option value="responses">Responses（原生 /v1/responses）</option>
                </select>
              </label>
              <label class="m3s-field">
                <span>认证策略（密钥仅入系统钥匙串）</span>
                <select name="auth_strategy" id="m3s-inp-auth">
                  <option value="bearer">Bearer (Authorization)</option>
                  <option value="api_key">API Key (X-API-Key)</option>
                  <option value="none">None（本机 Ollama / vLLM 免密）</option>
                </select>
              </label>
              <div class="m3s-form-actions">
                <button type="submit" class="m3s-btn m3s-btn--filled m3s-btn--xs">保存并自动发现模型</button>
              </div>
            </form>
          </div>
        `
            : ''
        }

        <div class="m3s-filter-bar">
          <input
            type="search"
            class="m3s-search-input"
            id="m3s-model-search"
            placeholder="搜索模型显示名称或逻辑 ID（如 claude / qwen / gemini）…"
            value="${escapeHtml(this.filterQuery)}"
            aria-label="搜索模型"
          />
          <label class="m3s-check-inline">
            <input type="checkbox" id="m3s-enabled-only" ${this.filterEnabledOnly ? 'checked' : ''} />
            <span>只看已启用</span>
          </label>
        </div>

        <div class="m3s-provider-stack">
          ${this.providers
            .map((prov) => {
              const visibleModels = prov.models.filter((m) => {
                if (this.filterEnabledOnly && !m.enabled) return false;
                if (!q) return true;
                return (
                  m.displayName.toLowerCase().includes(q) ||
                  m.logicalId.toLowerCase().includes(q) ||
                  prov.name.toLowerCase().includes(q)
                );
              });
              if (q && visibleModels.length === 0) return '';
              return `
                <div class="m3s-card m3s-card--filled">
                  <div class="m3s-card-row">
                    <div>
                      <strong>${escapeHtml(prov.name)}</strong>
                      <span class="m3s-muted m3s-mono-sm">${escapeHtml(prov.baseUrl)} · ${prov.protocol} · auth:${prov.authStrategy}</span>
                    </div>
                  </div>

                  <div class="m3s-model-rows">
                    ${visibleModels
                      .map(
                        (m) => `
                      <div class="m3s-model-item">
                        <div class="m3s-model-info">
                          <div class="m3s-model-title">
                            <strong>${escapeHtml(m.displayName)}</strong>
                            <code>${escapeHtml(m.logicalId)}</code>
                            <span class="m3s-ctx-tag">${escapeHtml(m.contextWindow)}</span>
                          </div>
                          <div class="m3s-cap-tags">
                            ${m.caps.map((c) => `<span>${escapeHtml(c)}</span>`).join('')}
                            ${
                              m.reasoningLevels.length
                                ? `<span class="is-reasoning">思考档位: ${m.reasoningLevels.join('/')}</span>`
                                : ''
                            }
                          </div>
                        </div>
                        <div class="m3s-model-controls">
                          <button
                            type="button"
                            class="m3s-btn m3s-btn--tonal m3s-btn--xs"
                            data-preview-preset="${m.presetId}"
                            title="在 3D 光学交换矩阵中预览此模型的光路连接"
                          >
                            3D 光路预览
                          </button>
                          <label class="m3s-switch" aria-label="启用 ${escapeHtml(m.displayName)}">
                            <input
                              type="checkbox"
                              data-toggle-model="${escapeHtml(m.logicalId)}"
                              ${m.enabled ? 'checked' : ''}
                            />
                            <span class="m3s-switch-track"></span>
                          </label>
                        </div>
                      </div>
                    `,
                      )
                      .join('')}
                  </div>
                </div>
              `;
            })
            .join('')}
        </div>
      </div>
    `;
  }

  private renderSettingsTab(): string {
    return `
      <div class="m3s-view">
        <div class="m3s-view-head">
          <div>
            <h3>设置与安全偏好</h3>
            <p>真实复刻面板的独立无障碍三轴控制（主题 / 对比度 / 弹簧动效）与局域网访问密码门禁。</p>
          </div>
        </div>

        <div class="m3s-card m3s-card--filled">
          <h4>外观与无障碍偏好（立即作用于当前控制中心）</h4>

          <div class="m3s-setting-row">
            <span>面板主题</span>
            <div class="m3s-segmented" role="group" aria-label="面板主题">
              <button type="button" class="${this.panelTheme === 'light' ? 'is-selected' : ''}" data-set-theme="light">浅色 Light</button>
              <button type="button" class="${this.panelTheme === 'dark' ? 'is-selected' : ''}" data-set-theme="dark">深色 Dark</button>
            </div>
          </div>

          <div class="m3s-setting-row">
            <span>轮廓与文本对比度</span>
            <div class="m3s-segmented" role="group" aria-label="对比度">
              <button type="button" class="${this.panelContrast === 'standard' ? 'is-selected' : ''}" data-set-contrast="standard">标准</button>
              <button type="button" class="${this.panelContrast === 'high' ? 'is-selected' : ''}" data-set-contrast="high">高对比度</button>
            </div>
          </div>

          <div class="m3s-setting-row">
            <span>M3 Expressive 弹簧动效</span>
            <div class="m3s-segmented" role="group" aria-label="动效">
              <button type="button" class="${this.panelMotion === 'full' ? 'is-selected' : ''}" data-set-motion="full">完整物理弹簧</button>
              <button type="button" class="${this.panelMotion === 'reduced' ? 'is-selected' : ''}" data-set-motion="reduced">减弱</button>
            </div>
          </div>
        </div>

        <div class="m3s-card m3s-card--outlined">
          <div class="m3s-card-row">
            <div>
              <h4>浏览器与局域网远程访问门禁</h4>
              <p class="m3s-muted">未配置 Argon2id 访问密码前，后端硬性拒绝绑定 <code>0.0.0.0:31828</code>；跨公网建议使用 <code>ssh -L 31828:127.0.0.1:31828</code> 隧道。</p>
            </div>
            <span class="m3s-chip ${this.remoteAccessEnabled ? 'm3s-chip--warn' : 'm3s-chip--ok'}">
              ${this.remoteAccessEnabled ? '0.0.0.0:31828 (内网开启)' : '127.0.0.1:31828 (仅本机)'}
            </span>
          </div>

          <div class="m3s-setting-row">
            <span>模拟已配置 Argon2id 访问密码</span>
            <label class="m3s-switch" aria-label="是否已设置访问密码">
              <input type="checkbox" id="m3s-pwd-set" ${this.passwordConfigured ? 'checked' : ''} />
              <span class="m3s-switch-track"></span>
            </label>
          </div>

          <div class="m3s-setting-row">
            <span>允许局域网其他设备访问（<code>0.0.0.0</code>）</span>
            <label class="m3s-switch" aria-label="允许其他设备访问">
              <input type="checkbox" id="m3s-remote-toggle" ${this.remoteAccessEnabled ? 'checked' : ''} />
              <span class="m3s-switch-track"></span>
            </label>
          </div>
        </div>

        <div class="m3s-card m3s-card--outlined">
          <h4>零污染卸载承诺</h4>
          <p class="m3s-muted">停止后台 Router、校验并恢复最初的 <code>~/.codex/config.toml</code>、清理本程序钥匙串条目，完整保留官方 ChatGPT 登录态与会话历史。</p>
          <div class="m3s-cmd-box">
            <code>codex-mp-uninstall</code>
          </div>
        </div>
      </div>
    `;
  }

  private bindSandboxEvents() {
    this.root.querySelectorAll<HTMLButtonElement>('[data-m3s-tab]').forEach((btn) => {
      btn.addEventListener('click', () => {
        this.activeTab = btn.dataset.m3sTab as typeof this.activeTab;
        this.render();
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-m3s-jump]').forEach((btn) => {
      btn.addEventListener('click', () => {
        this.activeTab = btn.dataset.m3sJump as typeof this.activeTab;
        this.render();
      });
    });

    this.root.querySelector('[data-goto-add-provider]')?.addEventListener('click', () => {
      this.activeTab = 'providers';
      this.addDrawerOpen = true;
      this.render();
    });

    this.root.querySelector('#m3s-sync-btn')?.addEventListener('click', () => this.syncToCodex());
    this.root.querySelector('[data-trigger-sync]')?.addEventListener('click', () => this.syncToCodex());

    this.root.querySelector('#m3s-quick-theme-btn')?.addEventListener('click', () => {
      this.panelTheme = this.panelTheme === 'dark' ? 'light' : 'dark';
      this.render();
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-switch-acc]').forEach((btn) => {
      btn.addEventListener('click', () => {
        if (btn.dataset.switchAcc) this.switchAccount(btn.dataset.switchAcc);
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-restore-bak]').forEach((btn) => {
      btn.addEventListener('click', () => {
        if (btn.dataset.restoreBak) this.restoreBackup(btn.dataset.restoreBak);
      });
    });

    this.root.querySelector('#m3s-toggle-add-provider')?.addEventListener('click', () => {
      this.addDrawerOpen = !this.addDrawerOpen;
      this.render();
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-fill-tpl]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const tpl = QUICK_PROVIDER_TEMPLATES[Number(btn.dataset.fillTpl || '0')];
        if (!tpl) return;
        const nameInp = this.root.querySelector<HTMLInputElement>('#m3s-inp-name');
        const urlInp = this.root.querySelector<HTMLInputElement>('#m3s-inp-url');
        const protoSel = this.root.querySelector<HTMLSelectElement>('#m3s-inp-proto');
        const authSel = this.root.querySelector<HTMLSelectElement>('#m3s-inp-auth');
        if (nameInp) nameInp.value = tpl.name;
        if (urlInp) urlInp.value = tpl.baseUrl;
        if (protoSel) protoSel.value = tpl.protocol;
        if (authSel) authSel.value = tpl.authStrategy;
      });
    });

    this.root.querySelector<HTMLFormElement>('#m3s-add-provider-form')?.addEventListener('submit', (ev) => {
      ev.preventDefault();
      const fd = new FormData(ev.currentTarget as HTMLFormElement);
      const name = String(fd.get('name') || '').trim();
      const baseUrl = String(fd.get('base_url') || '').trim();
      const protocol = (fd.get('protocol') as 'chat_completions' | 'responses') || 'chat_completions';
      const authStrategy = (fd.get('auth_strategy') as 'bearer' | 'api_key' | 'none') || 'bearer';

      // Enforce actual `normalize_base_url` security rule from `crates/core`!
      const isLoopback =
        baseUrl.startsWith('http://127.0.0.1') ||
        baseUrl.startsWith('http://localhost') ||
        baseUrl.startsWith('http://[::1]');
      if (!baseUrl.startsWith('https://') && !isLoopback) {
        this.notify('✗ 校验拒绝：非环回主机必须使用 https://（仅 127.0.0.1 / localhost 豁免明文 HTTP）');
        return;
      }

      const id =
        name
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, '-')
          .replace(/^-|-$/g, '') || `prov-${this.providers.length + 1}`;

      const matchedTpl = QUICK_PROVIDER_TEMPLATES.find((t) => t.baseUrl === baseUrl);
      const discovered: SandboxModel[] = matchedTpl
        ? matchedTpl.discoveredModels.map((m) => ({ ...m }))
        : [
            {
              logicalId: `${id}/deepseek-v4.1`,
              displayName: `${name} / DeepSeek V4.1`,
              upstreamId: 'deepseek-v4.1',
              contextWindow: '128K',
              enabled: true,
              presetId: isLoopback ? 'ollama-local' : 'deepseek-oneapi',
              caps: ['文本', '工具调用', '流式输出', '支持思考'],
              reasoningLevels: ['medium', 'high'],
            },
          ];

      this.providers.unshift({
        id,
        name,
        baseUrl,
        protocol,
        authStrategy,
        models: discovered,
      });
      this.addDrawerOpen = false;
      this.catalogPending = true;
      this.notify(`✓ 已连接「${name}」并自动发现 ${discovered.length} 个模型，请点击顶部「应用到 Codex」同步目录`);
    });

    const searchInp = this.root.querySelector<HTMLInputElement>('#m3s-model-search');
    searchInp?.addEventListener('input', () => {
      const pos = searchInp.selectionStart ?? searchInp.value.length;
      this.filterQuery = searchInp.value;
      this.render();
      const nextInp = this.root.querySelector<HTMLInputElement>('#m3s-model-search');
      if (nextInp) {
        nextInp.focus();
        nextInp.setSelectionRange(pos, pos);
      }
    });

    this.root.querySelector<HTMLInputElement>('#m3s-enabled-only')?.addEventListener('change', (e) => {
      this.filterEnabledOnly = (e.currentTarget as HTMLInputElement).checked;
      this.render();
    });

    this.root.querySelectorAll<HTMLInputElement>('[data-toggle-model]').forEach((chk) => {
      chk.addEventListener('change', () => {
        const id = chk.dataset.toggleModel;
        for (const p of this.providers) {
          const m = p.models.find((x) => x.logicalId === id);
          if (m) {
            m.enabled = chk.checked;
            this.catalogPending = true;
          }
        }
        this.render();
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-preview-preset]').forEach((btn) => {
      btn.addEventListener('click', () => {
        const pid = btn.dataset.previewPreset;
        const preset = MODEL_ROUTE_PRESETS.find((p) => p.id === pid);
        if (preset) {
          this.onRoutePreview(preset);
          this.notify(`⚡ 3D 光学矩阵已切换至：${preset.logicalModelId} (${preset.routeBadge})`);
        }
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-set-theme]').forEach((btn) => {
      btn.addEventListener('click', () => {
        this.panelTheme = btn.dataset.setTheme as 'light' | 'dark';
        this.render();
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-set-contrast]').forEach((btn) => {
      btn.addEventListener('click', () => {
        this.panelContrast = btn.dataset.setContrast as 'standard' | 'high';
        this.render();
      });
    });

    this.root.querySelectorAll<HTMLButtonElement>('[data-set-motion]').forEach((btn) => {
      btn.addEventListener('click', () => {
        this.panelMotion = btn.dataset.setMotion as 'full' | 'reduced';
        this.render();
      });
    });

    this.root.querySelector<HTMLInputElement>('#m3s-pwd-set')?.addEventListener('change', (e) => {
      this.passwordConfigured = (e.currentTarget as HTMLInputElement).checked;
      if (!this.passwordConfigured && this.remoteAccessEnabled) {
        this.remoteAccessEnabled = false;
        this.notify('🔒 未设置密码时无法开启 0.0.0.0 绑定，已自动退回仅本机 127.0.0.1');
        return;
      }
      this.render();
    });

    this.root.querySelector<HTMLInputElement>('#m3s-remote-toggle')?.addEventListener('change', (e) => {
      const wantRemote = (e.currentTarget as HTMLInputElement).checked;
      if (wantRemote && !this.passwordConfigured) {
        (e.currentTarget as HTMLInputElement).checked = false;
        this.notify('✗ RemoteNotAllowedWithoutPassword：必须先设置 Argon2id 访问密码才能开启局域网访问');
        return;
      }
      this.remoteAccessEnabled = wantRemote;
      this.notify(
        wantRemote
          ? '⚠️ 已开启可信内网 0.0.0.0:31828 访问（明文 HTTP，公网请使用 SSH 隧道）'
          : '✓ 已收紧至仅本机环回 127.0.0.1:31828',
      );
    });
  }
}

function escapeHtml(raw: string): string {
  return raw
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}
