// Material 3 Codex OmniBridge Web 客户端逻辑
let sessionToken = localStorage.getItem("codex_mp_token") || "";

// ==========================================================================
// 1. API 客户端与鉴权
// ==========================================================================
async function api(path, options = {}) {
  const headers = {
    "Content-Type": "application/json",
    ...(options.headers || {}),
  };
  if (sessionToken) {
    headers["Authorization"] = `Bearer ${sessionToken}`;
  }
  const response = await fetch(path, {
    ...options,
    headers,
  });

  if (response.status === 401) {
    showLoginDialog();
    throw new Error("请先登录访问控制中心");
  }

  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(data.error || data.message || `请求失败 (${response.status})`);
  }
  return data;
}

// ==========================================================================
// 2. M3 模态对话框与 Snackbar 通知系统 (完全替代 prompt & confirm)
// ==========================================================================
const snackbar = document.querySelector("#m3-snackbar");
const snackbarMsg = document.querySelector("#snackbar-message");
const snackbarIcon = document.querySelector("#snackbar-icon");
let snackbarTimer = null;

function notify(text, error = false) {
  if (snackbarTimer) clearTimeout(snackbarTimer);
  snackbarMsg.textContent = text;
  snackbarIcon.textContent = error ? "error" : "check_circle";
  snackbar.classList.toggle("error", !!error);
  snackbar.classList.add("active");
  snackbarTimer = setTimeout(() => {
    snackbar.classList.remove("active");
  }, 4000);
}

// 通用 M3 异步 Prompt 对话框
function m3Prompt(title, desc = "", defaultValue = "") {
  return new Promise((resolve) => {
    const backdrop = document.querySelector("#prompt-dialog-backdrop");
    const titleEl = document.querySelector("#prompt-dialog-title");
    const descEl = document.querySelector("#prompt-dialog-desc");
    const inputEl = document.querySelector("#prompt-dialog-input");
    const formEl = document.querySelector("#prompt-dialog-form");
    const cancelBtn = document.querySelector("#prompt-dialog-cancel");

    titleEl.textContent = title;
    descEl.textContent = desc;
    inputEl.value = defaultValue;
    backdrop.style.display = "flex";
    inputEl.focus();

    const cleanup = () => {
      backdrop.style.display = "none";
      formEl.onsubmit = null;
      cancelBtn.onclick = null;
    };

    formEl.onsubmit = (e) => {
      e.preventDefault();
      const val = inputEl.value;
      cleanup();
      resolve(val);
    };

    cancelBtn.onclick = () => {
      cleanup();
      resolve(null);
    };
  });
}

// 通用 M3 异步 Confirm 对话框
function m3Confirm(title, desc = "") {
  return new Promise((resolve) => {
    const backdrop = document.querySelector("#confirm-dialog-backdrop");
    const titleEl = document.querySelector("#confirm-dialog-title");
    const descEl = document.querySelector("#confirm-dialog-desc");
    const confirmBtn = document.querySelector("#confirm-dialog-confirm");
    const cancelBtn = document.querySelector("#confirm-dialog-cancel");

    titleEl.textContent = title;
    descEl.textContent = desc;
    backdrop.style.display = "flex";

    const cleanup = () => {
      backdrop.style.display = "none";
      confirmBtn.onclick = null;
      cancelBtn.onclick = null;
    };

    confirmBtn.onclick = () => {
      cleanup();
      resolve(true);
    };

    cancelBtn.onclick = () => {
      cleanup();
      resolve(false);
    };
  });
}

// ==========================================================================
// 3. 基础工具与主题控制
// ==========================================================================
const htmlEscapes = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (character) => htmlEscapes[character]);
}

function formatRemainingTime(seconds) {
  if (!seconds || seconds <= 0) return "已重置";
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}天 ${h}小时后重置`;
  if (h > 0) return `${h}小时 ${m}分后重置`;
  return `${m}分钟后重置`;
}

// 主题切换逻辑
const themeToggleBtn = document.querySelector("#theme-toggle-btn");
const themeIcon = document.querySelector("#theme-icon");
let currentTheme = localStorage.getItem("codex_mp_theme") || "dark";
document.documentElement.setAttribute("data-theme", currentTheme);
updateThemeIcon();

themeToggleBtn.onclick = () => {
  currentTheme = currentTheme === "dark" ? "light" : "dark";
  document.documentElement.setAttribute("data-theme", currentTheme);
  localStorage.setItem("codex_mp_theme", currentTheme);
  updateThemeIcon();
};

function updateThemeIcon() {
  themeIcon.textContent = currentTheme === "dark" ? "light_mode" : "dark_mode";
}

// ==========================================================================
// 4. M3 页面单页路由导航切换 (Drawer Navigation)
// ==========================================================================
const navItems = document.querySelectorAll(".m3-nav-item");
const viewSections = document.querySelectorAll(".m3-view-section");

navItems.forEach((btn) => {
  btn.onclick = () => {
    const targetId = btn.getAttribute("data-target");
    if (!targetId) return;

    navItems.forEach((n) => n.classList.remove("active"));
    btn.classList.add("active");

    viewSections.forEach((sec) => {
      sec.classList.toggle("active", sec.id === targetId);
    });
  };
});

// ==========================================================================
// 5. 渲染组件：额度指标进度条 (M3 Linear Progress)
// ==========================================================================
function renderM3UsageMetric(title, windowData) {
  if (!windowData) {
    return `
      <div class="m3-progress-container">
        <div class="m3-progress-head">
          <span class="m3-label-medium">${escapeHtml(title)}</span>
          <span class="m3-body-small" style="color: var(--md-sys-color-outline);">无数据 / 未开启</span>
        </div>
        <div class="m3-progress-track">
          <div class="m3-progress-indicator" style="width: 0%;"></div>
        </div>
      </div>
    `;
  }

  const percent = Math.min(100, Math.max(0, windowData.used_percent || 0));
  let statusClass = "";
  if (percent >= 90) statusClass = "danger";
  else if (percent >= 70) statusClass = "warning";

  const resetDesc = formatRemainingTime(windowData.reset_after_seconds);

  return `
    <div class="m3-progress-container">
      <div class="m3-progress-head">
        <span class="m3-label-medium">${escapeHtml(title)}</span>
        <span class="m3-label-large" style="color: ${percent >= 90 ? 'var(--md-sys-color-error)' : 'var(--md-sys-color-primary)'};">${percent}%</span>
      </div>
      <div class="m3-progress-track">
        <div class="m3-progress-indicator ${statusClass}" style="width: ${percent}%;"></div>
      </div>
      <div style="display: flex; justify-content: flex-end;">
        <span class="m3-body-small" style="color: var(--md-sys-color-on-surface-variant); font-size: 11px;">${escapeHtml(resetDesc)}</span>
      </div>
    </div>
  `;
}

function renderM3ReserveMetric(reserve) {
  if (!reserve) {
    return `
      <div class="m3-progress-container">
        <div class="m3-progress-head">
          <span class="m3-label-medium">GPT Reserve 备用额度</span>
          <span class="m3-body-small" style="color: var(--md-sys-color-outline);">未检测到</span>
        </div>
        <div class="m3-progress-track">
          <div class="m3-progress-indicator" style="width: 0%;"></div>
        </div>
      </div>
    `;
  }

  const percent = reserve.used_percent != null ? Math.min(100, Math.max(0, reserve.used_percent)) : (reserve.limit_reached ? 100 : 0);
  let statusClass = "";
  if (percent >= 90 || reserve.limit_reached) statusClass = "danger";
  else if (percent >= 70) statusClass = "warning";

  const resetDesc = formatRemainingTime(reserve.reset_after_seconds);

  return `
    <div class="m3-progress-container">
      <div class="m3-progress-head">
        <span class="m3-label-medium">GPT Reserve 备用额度</span>
        <span class="m3-label-large" style="color: ${percent >= 90 ? 'var(--md-sys-color-error)' : 'var(--md-sys-color-primary)'};">${reserve.limit_reached ? "已达上限" : `${percent}%`}</span>
      </div>
      <div class="m3-progress-track">
        <div class="m3-progress-indicator ${statusClass}" style="width: ${percent}%;"></div>
      </div>
      <div style="display: flex; justify-content: flex-end;">
        <span class="m3-body-small" style="color: var(--md-sys-color-on-surface-variant); font-size: 11px;">${escapeHtml(resetDesc)}</span>
      </div>
    </div>
  `;
}

// ==========================================================================
// 6. 状态获取与界面更新 (Overview / Accounts / Providers / Settings)
// ==========================================================================
let cachedProviders = [];
let cachedAccounts = [];
let cachedActiveAccount = null;

// 6.1 刷新 Router 状态
async function refreshRouterStatus() {
  const badge = document.querySelector("#router-state-badge");
  const text = document.querySelector("#router-state-text");
  const metricVal = document.querySelector("#metric-router-val");
  const metricDesc = document.querySelector("#metric-router-desc");

  try {
    const status = await api("/api/v1/router/status");
    if (status.healthy) {
      badge.className = "m3-badge m3-badge-success";
      text.textContent = "Router 正常";
      metricVal.textContent = "在线运行中";
      metricDesc.textContent = "透传官方与自定义分流正常";
    } else if (status.running) {
      badge.className = "m3-badge m3-badge-warning";
      text.textContent = "Router 启动中";
      metricVal.textContent = "启动中…";
      metricDesc.textContent = "正在建立端点连接";
    } else {
      badge.className = "m3-badge m3-badge-error";
      text.textContent = "Router 未运行";
      metricVal.textContent = "离线";
      metricDesc.textContent = "后台服务未启动";
    }
  } catch (e) {
    badge.className = "m3-badge m3-badge-warning";
    text.textContent = "Router 状态未知";
    metricVal.textContent = "未知";
    metricDesc.textContent = "无法获取状态";
  }
}

// 6.2 刷新 Desktop 状态
async function refreshDesktopStatus() {
  const metricVal = document.querySelector("#metric-desktop-val");
  const metricDesc = document.querySelector("#metric-desktop-desc");
  const settingsBadge = document.querySelector("#settings-desktop-badge");
  const settingsDetail = document.querySelector("#settings-desktop-detail");

  try {
    const status = await api("/api/v1/desktop/status");
    const labels = { unmanaged: "未安装", managed: "已适配", drifted: "检测到漂移" };
    const label = labels[status.state] || status.state;

    metricVal.textContent = label;
    metricDesc.textContent = `版本: ${status.version || '未知'}`;

    settingsBadge.textContent = label;
    settingsBadge.className = status.state === "managed" ? "m3-badge m3-badge-success" : "m3-badge m3-badge-warning";
    settingsDetail.textContent = `${status.version} · 入口: ${status.entrypoint}${
      status.active_pids && status.active_pids.length
        ? ` · 运行中 PID: ${status.active_pids.join(", ")}`
        : " · Desktop 当前未运行"
    }`;
  } catch (e) {
    metricVal.textContent = "未检测到";
    metricDesc.textContent = "无独立运行时";
    settingsBadge.textContent = "未安装";
    settingsBadge.className = "m3-badge m3-badge-warning";
    settingsDetail.textContent = "系统未检测到官方 ChatGPT / Codex Desktop 应用。";
  }
}

// 6.3 刷新安全控制状态
async function refreshSecurityStatus() {
  const accessBadge = document.querySelector("#settings-access-state-badge");
  const allowRemoteInput = document.querySelector("#settings-allow-remote");
  const tip = document.querySelector("#settings-security-tip");
  const logoutBtn = document.querySelector("#logout-btn");

  try {
    const sec = await api("/api/v1/security/status");
    allowRemoteInput.checked = sec.allow_remote;

    if (sec.password_set) {
      accessBadge.textContent = sec.allow_remote ? "已设密码 · 允许外网" : "已设密码 · 仅本机";
      accessBadge.className = "m3-badge m3-badge-success";
      tip.textContent = `已开启密码保护。当前监听地址：${sec.bind_addr}:${sec.port}。`;
      logoutBtn.style.display = "inline-flex";
    } else {
      accessBadge.textContent = "未设密码 · 仅限本机";
      accessBadge.className = "m3-badge m3-badge-warning";
      tip.textContent = "未设置访问密码。为了系统安全，外网访问已自动锁定，仅允许 127.0.0.1 访问。";
      logoutBtn.style.display = "none";
    }
  } catch (e) {
    accessBadge.textContent = "获取失败";
    accessBadge.className = "m3-badge m3-badge-error";
  }
}

// 6.4 刷新账号列表与活跃账号
async function refreshAccounts() {
  const container = document.querySelector("#accounts-card-container");
  const metricVal = document.querySelector("#metric-account-val");
  const metricPlan = document.querySelector("#metric-account-plan");
  const overviewUsageContainer = document.querySelector("#overview-active-usage-container");

  try {
    const [accounts, active] = await Promise.all([
      api("/api/v1/accounts"),
      api("/api/v1/accounts/active").catch(() => null),
    ]);
    cachedAccounts = accounts || [];
    cachedActiveAccount = active;

    // 更新总览中的当前生效账号卡片
    if (active && active.is_logged_in && active.email) {
      metricVal.textContent = active.email;
      metricPlan.textContent = `方案: ${(active.plan_type || "plus").toUpperCase()}`;
    } else {
      metricVal.textContent = "当前未登录";
      metricPlan.textContent = "未检测到有效会话";
    }

    // 渲染总览里的当前账号即时额度
    const activeAccObj = cachedAccounts.find((a) => a.is_active);
    if (activeAccObj && activeAccObj.usage) {
      overviewUsageContainer.innerHTML = `
        <div style="display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 20px;">
          ${renderM3UsageMetric("5 小时窗口额度 (Plus)", activeAccObj.usage.primary_5h)}
          ${renderM3UsageMetric("7 天周额度", activeAccObj.usage.secondary_weekly)}
          ${renderM3ReserveMetric(activeAccObj.usage.reserve)}
        </div>
      `;
    } else if (active && active.is_logged_in) {
      overviewUsageContainer.innerHTML = `
        <div class="m3-body-medium" style="color: var(--md-sys-color-on-surface-variant); padding: 8px 0;">
          当前正生效账号：<strong>${escapeHtml(active.email)}</strong> (${escapeHtml((active.plan_type || "plus").toUpperCase())})。请点击“保存当前登录态”收纳并查看精准额度。
        </div>
      `;
    } else {
      overviewUsageContainer.innerHTML = `
        <div class="m3-body-medium" style="color: var(--md-sys-color-outline); padding: 8px 0;">
          当前系统未登录官方账号，无法获取额度。
        </div>
      `;
    }

    // 渲染账号管理页面卡片网格
    if (!cachedAccounts.length) {
      container.innerHTML = `
        <div class="m3-card m3-card-filled" style="grid-column: 1 / -1; text-align: center; padding: 48px 20px;">
          <span class="material-symbols-outlined" style="font-size: 48px; color: var(--md-sys-color-outline); margin-bottom: 12px;">account_circle</span>
          <p class="m3-title-medium" style="color: var(--md-sys-color-on-surface-variant); margin-bottom: 8px;">暂无托管的官方账号</p>
          <p class="m3-body-small" style="color: var(--md-sys-color-outline);">点击上方“保存当前登录态为账号”即可将当前会话纳入管理</p>
        </div>
      `;
      return;
    }

    container.innerHTML = "";
    for (const acc of cachedAccounts) {
      const card = document.createElement("div");
      card.className = `m3-card m3-card-elevated m3-account-card ${acc.is_active ? "active" : ""}`;

      const plan = acc.plan_type || "plus";
      const planClass = plan.toLowerCase();
      const usage = acc.usage;

      card.innerHTML = `
        <div class="m3-account-card-header">
          <div class="m3-account-title-group">
            <span class="m3-title-medium" style="font-size: 17px;">${escapeHtml(acc.name)}</span>
            <span class="m3-body-small" style="color: var(--md-sys-color-on-surface-variant);">${escapeHtml(acc.email || "未知邮箱")}</span>
          </div>
          <div class="m3-account-badges">
            <span class="m3-plan-badge ${planClass}">${escapeHtml(plan)}</span>
            ${acc.is_active ? `<span class="m3-badge m3-badge-success" style="font-size: 11px;">当前生效</span>` : ""}
          </div>
        </div>

        <div class="m3-account-metrics-box">
          ${renderM3UsageMetric("5 小时窗口额度", usage ? usage.primary_5h : null)}
          ${renderM3UsageMetric("7 天周额度", usage ? usage.secondary_weekly : null)}
          ${renderM3ReserveMetric(usage ? usage.reserve : null)}
        </div>

        <div class="m3-account-actions">
          ${
            !acc.is_active
              ? `<button class="m3-btn m3-btn-filled switch-acc-btn" data-id="${acc.id}" style="height: 36px; padding: 0 16px;">
                   <span class="material-symbols-outlined" style="font-size: 18px;">swap_horiz</span>
                   <span>切换并重启</span>
                 </button>`
              : `<button class="m3-btn m3-btn-tonal" disabled style="height: 36px; padding: 0 16px; opacity: 0.8;">
                   <span class="material-symbols-outlined" style="font-size: 18px;">check</span>
                   <span>正在使用中</span>
                 </button>`
          }
          <button class="m3-btn m3-btn-tonal check-usage-acc-btn" data-id="${acc.id}" style="height: 36px; padding: 0 12px;" title="刷新实时额度">
            <span class="material-symbols-outlined" style="font-size: 18px;">autorenew</span>
          </button>
          <button class="m3-btn m3-btn-outlined rename-acc-btn" data-id="${acc.id}" data-name="${escapeHtml(acc.name)}" style="height: 36px; padding: 0 12px;" title="重命名备注">
            <span class="material-symbols-outlined" style="font-size: 18px;">edit</span>
          </button>
          <button class="m3-btn m3-btn-danger delete-acc-btn" data-id="${acc.id}" style="height: 36px; padding: 0 12px; margin-left: auto;" title="移除账号">
            <span class="material-symbols-outlined" style="font-size: 18px;">delete</span>
          </button>
        </div>
      `;

      // 绑定切换按钮
      const switchBtn = card.querySelector(".switch-acc-btn");
      if (switchBtn) {
        switchBtn.onclick = async () => {
          try {
            notify(`正在切换至账号【${acc.name}】并重启 Codex…`);
            const res = await api("/api/v1/accounts/switch", {
              method: "POST",
              body: JSON.stringify({ account_id: acc.id, restart_codex: true }),
            });
            notify(`已切换至【${res.account.name}】并重启 Codex 运行时！`);
            await refreshAccounts();
          } catch (e) {
            notify(e.message, true);
          }
        };
      }

      // 绑定刷新额度按钮
      const checkUsageBtn = card.querySelector(".check-usage-acc-btn");
      checkUsageBtn.onclick = async () => {
        try {
          checkUsageBtn.disabled = true;
          await api(`/api/v1/accounts/${acc.id}/usage`);
          notify(`已更新【${acc.name}】的额度。`);
          await refreshAccounts();
        } catch (e) {
          notify(e.message, true);
        } finally {
          checkUsageBtn.disabled = false;
        }
      };

      // 绑定重命名按钮
      const renameBtn = card.querySelector(".rename-acc-btn");
      renameBtn.onclick = async () => {
        const newName = await m3Prompt("重命名账号", "请输入新的账号备注名称：", acc.name);
        if (newName && newName.trim() && newName.trim() !== acc.name) {
          try {
            await api("/api/v1/accounts/rename", {
              method: "POST",
              body: JSON.stringify({ account_id: acc.id, name: newName.trim() }),
            });
            notify("账号重命名成功！");
            await refreshAccounts();
          } catch (e) {
            notify(e.message, true);
          }
        }
      };

      // 绑定删除按钮
      const deleteBtn = card.querySelector(".delete-acc-btn");
      deleteBtn.onclick = async () => {
        const ok = await m3Confirm("确认删除账号", `确定要从托管列表中移除账号【${acc.name}】吗？`);
        if (!ok) return;
        try {
          await api("/api/v1/accounts/delete", {
            method: "POST",
            body: JSON.stringify({ account_id: acc.id }),
          });
          notify("账号已移除。");
          await refreshAccounts();
        } catch (e) {
          notify(e.message, true);
        }
      };

      container.appendChild(card);
    }
  } catch (e) {
    container.innerHTML = `<div class="m3-body-medium" style="color: var(--md-sys-color-error);">加载账号失败: ${escapeHtml(e.message)}</div>`;
  }
}

// 6.5 渲染单个模型 Row
function renderModelRow(provider, model) {
  const row = document.createElement("div");
  row.className = "m3-model-row";

  const contextText = model.context_window ? `${model.context_window} tokens` : "默认上下文";
  row.innerHTML = `
    <div class="m3-model-info">
      <div style="display: flex; align-items: center; gap: 8px;">
        <strong class="m3-title-medium" style="font-size: 14px;">${escapeHtml(model.display_name || model.logical_model_id)}</strong>
        <span class="m3-body-small" style="color: var(--md-sys-color-outline); font-family: monospace;">${escapeHtml(model.logical_model_id)}</span>
      </div>
      <div class="m3-model-tags" style="margin-top: 4px;">
        <span class="m3-badge m3-badge-success" style="font-size: 10px; padding: 1px 8px;">${escapeHtml(contextText)}</span>
        ${model.capabilities && model.capabilities.images ? '<span class="m3-badge" style="font-size: 10px; padding: 1px 8px; background: rgba(130,213,229,0.12);">图片</span>' : ''}
        ${model.capabilities && model.capabilities.tools ? '<span class="m3-badge" style="font-size: 10px; padding: 1px 8px; background: rgba(218,226,255,0.18);">Tools</span>' : ''}
      </div>
    </div>
    <div class="m3-model-actions">
      <label class="m3-switch" title="${model.enabled ? '已启用，点击停用' : '已停用，点击启用'}">
        <input type="checkbox" class="toggle-model-switch" ${model.enabled ? "checked" : ""} />
        <span class="m3-switch-slider"></span>
      </label>
      <button class="m3-icon-btn edit-model-btn" title="编辑模型属性">
        <span class="material-symbols-outlined" style="font-size: 18px;">edit</span>
      </button>
      <button class="m3-icon-btn delete-model-btn" title="删除模型" style="color: var(--md-sys-color-error);">
        <span class="material-symbols-outlined" style="font-size: 18px;">delete</span>
      </button>
    </div>
  `;

  // 开关切换
  const toggleSwitch = row.querySelector(".toggle-model-switch");
  toggleSwitch.onchange = async () => {
    try {
      await api("/api/v1/models/enabled", {
        method: "POST",
        body: JSON.stringify({ logical_model_id: model.logical_model_id, enabled: toggleSwitch.checked }),
      });
      notify(`模型【${model.logical_model_id}】已${toggleSwitch.checked ? '启用' : '停用'}`);
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
      toggleSwitch.checked = !toggleSwitch.checked;
    }
  };

  // 编辑模型
  const editBtn = row.querySelector(".edit-model-btn");
  editBtn.onclick = async () => {
    const newName = await m3Prompt("编辑显示名称", "在 Codex 客户端列表中的友好名称：", model.display_name || "");
    if (newName === null) return;
    const ctxVal = await m3Prompt("编辑上下文窗口", "上下文 Token 上限（输入 0 清除限制，留空保持现状）：", model.context_window || "");
    const args = {
      logical_model_id: model.logical_model_id,
      display_name: newName.trim(),
      clear_context_window: false,
      context_window: null,
    };
    if (ctxVal !== null && ctxVal.trim() === "0") {
      args.clear_context_window = true;
    } else if (ctxVal !== null && ctxVal.trim() !== "") {
      const parsed = Number(ctxVal);
      if (!Number.isSafeInteger(parsed) || parsed < 1) {
        return notify("上下文必须是正整数或 0", true);
      }
      args.context_window = parsed;
    }
    try {
      await api("/api/v1/models/edit", { method: "POST", body: JSON.stringify(args) });
      notify("模型已更新");
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
    }
  };

  // 删除模型
  const deleteBtn = row.querySelector(".delete-model-btn");
  deleteBtn.onclick = async () => {
    const ok = await m3Confirm("确认删除模型", `确定要删除模型【${model.logical_model_id}】吗？`);
    if (!ok) return;
    try {
      await api("/api/v1/models/remove", {
        method: "POST",
        body: JSON.stringify({ logical_model_id: model.logical_model_id }),
      });
      notify("模型已删除，请同步到 Codex。");
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
    }
  };

  return row;
}

// 6.6 渲染发现模型与导入区
function renderDiscoveredBox(provider) {
  const box = document.createElement("div");
  box.className = "m3-card m3-card-filled";
  box.style.marginTop = "14px";
  box.style.padding = "16px";

  box.innerHTML = `
    <div style="display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px;">
      <div>
        <h4 class="m3-title-medium">发现模型并导入</h4>
        <p class="m3-body-small" style="color: var(--md-sys-color-on-surface-variant);">从 ${escapeHtml(provider.base_url)} 上游自动扫描可用模型</p>
      </div>
      <button class="m3-btn m3-btn-tonal fetch-discover-btn" style="height: 34px; padding: 0 14px;">
        <span class="material-symbols-outlined" style="font-size: 18px;">travel_explore</span>
        <span>扫描模型</span>
      </button>
    </div>
    <div class="discovered-items-container" style="display: flex; flex-direction: column; gap: 8px; max-height: 260px; overflow-y: auto;">
      <span class="m3-body-small" style="color: var(--md-sys-color-outline);">点击“扫描模型”开始抓取上游清单…</span>
    </div>
    <div class="discovered-actions" style="display: none; justify-content: flex-end; gap: 10px; margin-top: 12px; border-top: 1px solid var(--md-sys-color-outline-variant); padding-top: 12px;">
      <button class="m3-btn m3-btn-filled import-selected-btn">
        <span class="material-symbols-outlined" style="font-size: 18px;">add_task</span>
        <span>导入所选模型</span>
      </button>
    </div>
  `;

  const fetchBtn = box.querySelector(".fetch-discover-btn");
  const listContainer = box.querySelector(".discovered-items-container");
  const actionsBar = box.querySelector(".discovered-actions");
  const importBtn = box.querySelector(".import-selected-btn");
  let discoveredList = [];

  fetchBtn.onclick = async () => {
    try {
      fetchBtn.disabled = true;
      listContainer.innerHTML = `<span class="m3-body-small" style="color: var(--md-sys-color-primary);">正在抓取上游模型清单…</span>`;
      discoveredList = await api(`/api/v1/providers/${encodeURIComponent(provider.id)}/discover`);
      listContainer.innerHTML = "";

      if (!discoveredList.length) {
        listContainer.innerHTML = `<span class="m3-body-small" style="color: var(--md-sys-color-outline);">未发现可用模型</span>`;
        actionsBar.style.display = "none";
        return;
      }

      for (const m of discoveredList) {
        const item = document.createElement("label");
        item.style.display = "flex";
        item.style.alignItems = "center";
        item.style.justifyContent = "space-between";
        item.style.padding = "8px 12px";
        item.style.backgroundColor = "var(--md-sys-color-surface-container-lowest)";
        item.style.borderRadius = "var(--md-shape-corner-small)";
        item.style.cursor = "pointer";

        item.innerHTML = `
          <div style="display: flex; flex-direction: column;">
            <strong class="m3-body-medium">${escapeHtml(m.display_name || m.upstream_model_id)}</strong>
            <span class="m3-body-small" style="color: var(--md-sys-color-outline); font-family: monospace;">${escapeHtml(m.upstream_model_id)}</span>
          </div>
          <input type="checkbox" value="${escapeHtml(m.upstream_model_id)}" style="width: 18px; height: 18px; accent-color: var(--md-sys-color-primary);" />
        `;
        listContainer.appendChild(item);
      }
      actionsBar.style.display = "flex";
      notify(`成功扫描到 ${discoveredList.length} 个可用模型`);
    } catch (e) {
      listContainer.innerHTML = `<span class="m3-body-small" style="color: var(--md-sys-color-error);">${escapeHtml(e.message)}</span>`;
      notify(e.message, true);
    } finally {
      fetchBtn.disabled = false;
    }
  };

  importBtn.onclick = async () => {
    const selected = [...listContainer.querySelectorAll("input:checked")].map((i) => i.value);
    if (!selected.length) return notify("请勾选需要导入的模型", true);

    try {
      await api("/api/v1/models/import", {
        method: "POST",
        body: JSON.stringify({
          provider_id: provider.id,
          discovered: discoveredList,
          selected_ids: selected,
        }),
      });
      notify(`已成功导入 ${selected.length} 个模型！`);
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
    }
  };

  return box;
}

// 6.7 渲染 Provider 卡片
function renderProviderCard(provider) {
  const card = document.createElement("div");
  card.className = "m3-card m3-card-elevated m3-provider-card";

  card.innerHTML = `
    <div class="m3-provider-head">
      <div style="display: flex; flex-direction: column; gap: 4px;">
        <div style="display: flex; align-items: center; gap: 10px;">
          <h3 class="m3-title-large">${escapeHtml(provider.name)}</h3>
          <span class="m3-badge m3-badge-success" style="font-family: monospace;">${escapeHtml(provider.id)}</span>
          <span class="m3-badge" style="background: var(--md-sys-color-surface-container-highest);">${escapeHtml(provider.protocol)}</span>
        </div>
        <span class="m3-body-small" style="color: var(--md-sys-color-outline); font-family: monospace;">${escapeHtml(provider.base_url)}</span>
      </div>
      <div style="display: flex; align-items: center; gap: 8px;">
        <button class="m3-btn m3-btn-tonal discover-toggle-btn" style="height: 36px; padding: 0 14px;">
          <span class="material-symbols-outlined" style="font-size: 18px;">travel_explore</span>
          <span>发现模型</span>
        </button>
        <button class="m3-btn m3-btn-tonal add-model-to-provider-btn" style="height: 36px; padding: 0 14px;">
          <span class="material-symbols-outlined" style="font-size: 18px;">add</span>
          <span>添加模型</span>
        </button>
        <button class="m3-icon-btn edit-provider-btn" title="编辑供应商">
          <span class="material-symbols-outlined">edit</span>
        </button>
        <button class="m3-icon-btn delete-provider-btn" title="删除供应商" style="color: var(--md-sys-color-error);">
          <span class="material-symbols-outlined">delete</span>
        </button>
      </div>
    </div>

    <div class="m3-model-list"></div>
  `;

  const modelListContainer = card.querySelector(".m3-model-list");
  if (provider.models && provider.models.length) {
    for (const m of provider.models) {
      modelListContainer.appendChild(renderModelRow(provider, m));
    }
  } else {
    modelListContainer.innerHTML = `<div class="m3-body-small" style="color: var(--md-sys-color-outline); padding: 8px 0;">暂无模型，可点击“发现模型”或“添加模型”。</div>`;
  }

  const discoveredBox = renderDiscoveredBox(provider);
  discoveredBox.style.display = "none";
  card.appendChild(discoveredBox);

  // 展开/折叠发现面板
  const toggleDiscoverBtn = card.querySelector(".discover-toggle-btn");
  toggleDiscoverBtn.onclick = () => {
    discoveredBox.style.display = discoveredBox.style.display === "none" ? "block" : "none";
  };

  // 快捷添加模型给该 Provider
  const addModelBtn = card.querySelector(".add-model-to-provider-btn");
  addModelBtn.onclick = () => {
    document.querySelector("#add-model-provider-id").value = provider.id;
    document.querySelector("#add-model-dialog-backdrop").style.display = "flex";
  };

  // 编辑 Provider
  const editBtn = card.querySelector(".edit-provider-btn");
  editBtn.onclick = async () => {
    const name = await m3Prompt("编辑供应商名称", "显示名称：", provider.name);
    if (name === null) return;
    const baseUrl = await m3Prompt("编辑 Base URL", "API 根地址：", provider.base_url);
    if (baseUrl === null) return;

    try {
      await api("/api/v1/providers/edit", {
        method: "POST",
        body: JSON.stringify({
          id: provider.id,
          name: name.trim(),
          base_url: baseUrl.trim(),
          protocol: null,
          enabled: null,
          api_key: null,
        }),
      });
      notify("Provider 已更新");
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
    }
  };

  // 删除 Provider
  const deleteBtn = card.querySelector(".delete-provider-btn");
  deleteBtn.onclick = async () => {
    const ok = await m3Confirm("确认删除 Provider", `确定要删除【${provider.name} (${provider.id})】及其关联的所有模型吗？`);
    if (!ok) return;
    const purgeKey = await m3Confirm("凭证删除确认", "是否同时在系统安全钥匙箱中清除该 Provider 存储的 API Key？");

    try {
      await api("/api/v1/providers/remove", {
        method: "POST",
        body: JSON.stringify({ id: provider.id, purge_credential: purgeKey }),
      });
      notify("Provider 已移除，请点击同步到 Codex。");
      await refreshProviders();
    } catch (e) {
      notify(e.message, true);
    }
  };

  return card;
}

// 6.8 刷新所有 Provider
async function refreshProviders() {
  const container = document.querySelector("#providers-container");
  const metricProvVal = document.querySelector("#metric-providers-val");
  const metricModelVal = document.querySelector("#metric-models-val");

  try {
    const providers = await api("/api/v1/providers");
    cachedProviders = providers || [];

    let totalModels = 0;
    for (const p of cachedProviders) {
      totalModels += (p.models || []).length;
    }
    metricProvVal.textContent = cachedProviders.length;
    metricModelVal.textContent = `共注册 ${totalModels} 个模型`;

    if (!cachedProviders.length) {
      container.innerHTML = `
        <div class="m3-card m3-card-filled" style="text-align: center; padding: 48px 20px;">
          <span class="material-symbols-outlined" style="font-size: 48px; color: var(--md-sys-color-outline); margin-bottom: 12px;">neurology</span>
          <p class="m3-title-medium" style="color: var(--md-sys-color-on-surface-variant); margin-bottom: 8px;">未添加任何模型服务商</p>
          <p class="m3-body-small" style="color: var(--md-sys-color-outline);">点击上方“添加 Provider”接入 NewAPI 或 OneAPI 等代理</p>
        </div>
      `;
      return;
    }

    container.innerHTML = "";
    for (const p of cachedProviders) {
      container.appendChild(renderProviderCard(p));
    }
  } catch (e) {
    container.innerHTML = `<div class="m3-body-medium" style="color: var(--md-sys-color-error);">加载服务商失败: ${escapeHtml(e.message)}</div>`;
  }
}

// ==========================================================================
// 7. 全量数据刷新总线
// ==========================================================================
async function refreshAll() {
  try {
    await Promise.all([
      refreshRouterStatus(),
      refreshDesktopStatus(),
      refreshSecurityStatus(),
      refreshAccounts(),
      refreshProviders(),
    ]);
  } catch (e) {
    if (e.message.includes("请先登录")) {
      showLoginDialog();
    } else {
      notify(e.message, true);
    }
  }
}

// ==========================================================================
// 8. 模态框显隐与事件绑定
// ==========================================================================
const loginDialog = document.querySelector("#login-dialog-backdrop");
const loginForm = document.querySelector("#m3-login-form");
const loginError = document.querySelector("#login-dialog-error");
const logoutBtn = document.querySelector("#logout-btn");

function showLoginDialog() {
  loginDialog.style.display = "flex";
  logoutBtn.style.display = "none";
}

function hideLoginDialog() {
  loginDialog.style.display = "none";
  if (sessionToken) {
    logoutBtn.style.display = "inline-flex";
  }
}

// 登录提交
loginForm.onsubmit = async (e) => {
  e.preventDefault();
  const pwd = document.querySelector("#m3-login-password").value;
  try {
    const res = await fetch("/api/v1/security/login", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ password: pwd }),
    });
    const data = await res.json();
    if (!res.ok) throw new Error(data.error || "登录失败，密码错误");

    sessionToken = data.token;
    localStorage.setItem("codex_mp_token", sessionToken);
    document.querySelector("#m3-login-password").value = "";
    loginError.textContent = "";
    hideLoginDialog();
    notify("登录成功！");
    await refreshAll();
  } catch (err) {
    loginError.textContent = err.message;
  }
};

// 退出登录
logoutBtn.onclick = () => {
  sessionToken = "";
  localStorage.removeItem("codex_mp_token");
  showLoginDialog();
  notify("已安全退出登录。");
};

// 顶部栏同步到 Codex
document.querySelector("#topbar-sync-btn").onclick = async () => {
  try {
    const res = await api("/api/v1/catalog/sync", { method: "POST" });
    notify(`Codex Catalog 同步成功：${res.catalog_path}`);
  } catch (e) {
    notify(e.message, true);
  }
};

// 总览刷新按钮
document.querySelector("#overview-refresh-btn").onclick = () => {
  notify("正在刷新数据…");
  refreshAll();
};

document.querySelector("#overview-check-account-btn").onclick = async () => {
  try {
    const status = await api("/api/v1/accounts/active");
    if (status.is_logged_in && status.email) {
      notify(`当前生效: ${status.email} (${(status.plan_type || "plus").toUpperCase()})`);
    } else {
      notify("~/.codex/auth.json 未检测到有效登录", true);
    }
    await refreshAccounts();
  } catch (e) {
    notify(e.message, true);
  }
};

document.querySelector("#overview-restart-codex-btn").onclick = async () => {
  try {
    const res = await api("/api/v1/accounts/restart-codex", { method: "POST" });
    notify(`Codex 已重启 (已清理 ${res.terminated_pids.length} 个残留进程)`);
    await refreshAccounts();
  } catch (e) {
    notify(e.message, true);
  }
};

// 保存当前登录态为账号
async function handleCaptureAccount() {
  const name = await m3Prompt("保存当前登录态", "为当前 ~/.codex/auth.json 账号指定一个备注名称：", "");
  if (name === null) return;
  try {
    const res = await api("/api/v1/accounts/capture", {
      method: "POST",
      body: JSON.stringify({ name: name.trim() || null }),
    });
    notify(`已收纳账号【${res.name}】！`);
    await refreshAccounts();
  } catch (e) {
    notify(e.message, true);
  }
}

document.querySelector("#capture-account-action-btn").onclick = handleCaptureAccount;
document.querySelector("#overview-capture-account-btn").onclick = handleCaptureAccount;

// 导入账号模态框
const importDialog = document.querySelector("#import-account-dialog-backdrop");
const importForm = document.querySelector("#m3-import-account-form");
const importError = document.querySelector("#import-dialog-error");

document.querySelector("#open-import-modal-action-btn").onclick = () => {
  importDialog.style.display = "flex";
  importError.textContent = "";
};

document.querySelector("#close-import-account-dialog-btn").onclick = () => {
  importDialog.style.display = "none";
};

importForm.onsubmit = async (e) => {
  e.preventDefault();
  const form = new FormData(importForm);
  const name = form.get("name");
  const jsonContent = form.get("json_content");

  try {
    let parsed;
    try {
      parsed = JSON.parse(jsonContent);
    } catch {
      throw new Error("JSON 格式不合法，请核对后粘贴");
    }

    const payload = {
      name: name ? String(name).trim() : null,
      auth_json: parsed.tokens ? parsed : null,
      tokens: parsed.tokens ? null : (parsed.access_token || parsed.id_token ? parsed : null),
    };

    if (!payload.auth_json && !payload.tokens) {
      throw new Error("JSON 中既未包含 tokens 对象，也未包含 access_token/id_token 字段");
    }

    const res = await api("/api/v1/accounts/import", {
      method: "POST",
      body: JSON.stringify(payload),
    });

    importDialog.style.display = "none";
    importForm.reset();
    notify(`成功导入账号【${res.name}】！`);
    await refreshAccounts();
  } catch (err) {
    importError.textContent = err.message;
  }
};

// 批量刷新所有账号额度
document.querySelector("#refresh-all-accounts-usage-btn").onclick = async () => {
  const btn = document.querySelector("#refresh-all-accounts-usage-btn");
  try {
    btn.disabled = true;
    notify("正在批量查询各账号实时额度…");
    const accounts = await api("/api/v1/accounts");
    for (const acc of accounts) {
      try {
        await api(`/api/v1/accounts/${acc.id}/usage`);
      } catch (err) {
        console.warn(`刷新账号 ${acc.name} 额度失败:`, err);
      }
    }
    notify("所有托管账号额度已刷新！");
    await refreshAccounts();
  } catch (e) {
    notify(e.message, true);
  } finally {
    btn.disabled = false;
  }
};

// 添加 Provider 模态框
const addProviderDialog = document.querySelector("#add-provider-dialog-backdrop");
const addProviderForm = document.querySelector("#m3-provider-form");

function openAddProviderModal() {
  addProviderDialog.style.display = "flex";
}

document.querySelector("#open-add-provider-dialog-btn").onclick = openAddProviderModal;
document.querySelector("#overview-open-add-provider-btn").onclick = openAddProviderModal;

document.querySelector("#close-add-provider-btn").onclick = () => {
  addProviderDialog.style.display = "none";
};

addProviderForm.onsubmit = async (e) => {
  e.preventDefault();
  const form = new FormData(addProviderForm);
  try {
    await api("/api/v1/providers/add", {
      method: "POST",
      body: JSON.stringify({
        name: form.get("name"),
        base_url: form.get("base_url"),
        protocol: form.get("protocol"),
        api_key: form.get("api_key") || null,
      }),
    });
    addProviderForm.reset();
    addProviderDialog.style.display = "none";
    notify("Provider 已保存至本地安全存储，请点击同步到 Codex。");
    await refreshProviders();
  } catch (err) {
    notify(err.message, true);
  }
};

// 手动添加模型模态框
const addModelDialog = document.querySelector("#add-model-dialog-backdrop");
const addModelForm = document.querySelector("#m3-model-form");

document.querySelector("#open-add-model-dialog-btn").onclick = () => {
  addModelDialog.style.display = "flex";
};

document.querySelector("#close-add-model-btn").onclick = () => {
  addModelDialog.style.display = "none";
};

addModelForm.onsubmit = async (e) => {
  e.preventDefault();
  const form = new FormData(addModelForm);
  const context = form.get("context_window");
  try {
    await api("/api/v1/models/add", {
      method: "POST",
      body: JSON.stringify({
        provider_id: form.get("provider_id"),
        upstream_model_id: form.get("upstream_model_id"),
        display_name: form.get("display_name"),
        context_window: context ? Number(context) : null,
        images: form.get("images") === "on",
        tools: form.get("tools") === "on",
      }),
    });
    addModelForm.reset();
    addModelDialog.style.display = "none";
    notify("模型已添加，请同步到 Codex。");
    await refreshProviders();
  } catch (err) {
    notify(err.message, true);
  }
};

document.querySelector("#providers-refresh-btn").onclick = () => {
  notify("正在刷新服务商…");
  refreshProviders();
};

// 安全设置提交
document.querySelector("#settings-security-form").onsubmit = async (e) => {
  e.preventDefault();
  const password = document.querySelector("#settings-web-password").value;
  const allow_remote = document.querySelector("#settings-allow-remote").checked;

  try {
    const res = await api("/api/v1/security/update", {
      method: "POST",
      body: JSON.stringify({
        password: password.trim() ? password.trim() : null,
        allow_remote,
      }),
    });
    document.querySelector("#settings-web-password").value = "";
    if (res.token) {
      sessionToken = res.token;
      localStorage.setItem("codex_mp_token", sessionToken);
    }
    notify("安全与网络设置已更新！");
    await refreshSecurityStatus();
  } catch (err) {
    notify(err.message, true);
  }
};

// Desktop 适配器操作
document.querySelector("#settings-desktop-install-btn").onclick = async () => {
  try {
    await api("/api/v1/desktop/install", { method: "POST" });
    notify("Desktop 适配已安装！请完全退出并重新启动 ChatGPT Desktop。");
    await refreshDesktopStatus();
  } catch (err) {
    notify(err.message, true);
  }
};

document.querySelector("#settings-desktop-restore-btn").onclick = async () => {
  const ok = await m3Confirm("恢复官方入口确认", "恢复官方 Desktop 入口前，请先完全退出 ChatGPT Desktop。是否继续？");
  if (!ok) return;
  try {
    await api("/api/v1/desktop/restore", { method: "POST" });
    notify("Desktop 已恢复为官方纯净 runtime。");
    await refreshDesktopStatus();
  } catch (err) {
    notify(err.message, true);
  }
};

// ==========================================================================
// 9. 启动自检
// ==========================================================================
refreshAll();
