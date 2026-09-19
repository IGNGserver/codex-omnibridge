// Codex OmniBridge 面板客户端逻辑 (M3 Expressive)
//
// Structure:
//   1.  API client + auth            8.  Providers + models
//   2.  Dialogs, snackbar, tooltips  9.  Refresh bus
//   3.  Utilities + preferences      10. Login / logout
//   4.  Router + shell behaviours    11-13. Event bindings
//   5.  Quota / progress rendering   14. Desktop shell + bootstrap
//
// Rendering never writes inline styles for colour, size or spacing: every
// visual decision lives in the stylesheets, so a class change here cannot
// silently drift from the token layer. The only style attributes emitted are
// CSS custom properties carrying a computed progress value.

const urlParams = new URLSearchParams(window.location.search);
// Mutable: it must be clearable on logout. A `const` here meant the URL token
// survived logout and kept authenticating every request (`api()` falls back to
// it), so the panel showed a login dialog while still fully authenticated.
let queryToken = urlParams.get("local_token") || "";
if (queryToken) {
  // Keep the token in memory only. Leaving it in the address bar would leak it
  // through history, screenshots and Referer headers, so strip it immediately.
  urlParams.delete("local_token");
  const remainingQuery = urlParams.toString();
  window.history.replaceState(
    null,
    "",
    `${window.location.pathname}${remainingQuery ? `?${remainingQuery}` : ""}${window.location.hash}`,
  );
}

let sessionToken = (window.electronAPI && window.electronAPI.localToken)
  ? window.electronAPI.localToken
  : (queryToken || localStorage.getItem("codex_mp_token") || "");

const CATALOG_STATE_KEY = "codex_mp_catalog_state";
let catalogState = (() => {
  try {
    const saved = JSON.parse(localStorage.getItem(CATALOG_STATE_KEY) || "null");
    if (saved && typeof saved === "object") {
      return {
        pending: Boolean(saved.pending),
        routerNeedsRestart: Boolean(saved.routerNeedsRestart),
        reason: typeof saved.reason === "string" ? saved.reason : "",
        catalogPath: typeof saved.catalogPath === "string" ? saved.catalogPath : "",
      };
    }
  } catch {
    // A corrupt preference must never stop the panel from loading.
  }
  return { pending: false, routerNeedsRestart: false, reason: "", catalogPath: "" };
})();
let lastRouterStatus = null;

// ==========================================================================
// 1. API 客户端与鉴权
// ==========================================================================
function resolveApiUrl(path) {
  if (/^https?:\/\//i.test(path)) {
    return path;
  }
  // When the panel is loaded by Electron over file://, relative URLs have no
  // server to resolve against, so the host injects the real backend origin.
  const injectedBase = (window.electronAPI && window.electronAPI.apiBase) || "";
  if (injectedBase) {
    return `${injectedBase.replace(/\/+$/, "")}${path}`;
  }
  if (window.location.protocol === "file:") {
    return `http://localhost:31828${path}`;
  }
  return path;
}

async function api(path, options = {}) {
  const headers = {
    "Content-Type": "application/json",
    ...(options.headers || {}),
  };
  const activeToken = (window.electronAPI && window.electronAPI.localToken)
    ? window.electronAPI.localToken
    : (sessionToken || queryToken || localStorage.getItem("codex_mp_token") || "");

  if (activeToken) {
    // The desktop token is only honoured by the backend for loopback callers;
    // a remote browser session authenticates with the password-derived token.
    headers["Authorization"] = `Bearer ${activeToken}`;
    headers["X-Local-Token"] = activeToken;
  }
  const targetUrl = resolveApiUrl(path);
  const response = await fetch(targetUrl, {
    ...options,
    headers,
  });

  if (response.status === 401) {
    if (window.electronAPI && window.electronAPI.isElectron) {
      console.warn("Electron 环境免密鉴权失效，请检查后台");
    } else {
      // Discard the rejected token before prompting. Keeping it meant every
      // subsequent request repeated the same 401 and re-opened the dialog,
      // because `activeToken` kept re-reading the dead token.
      sessionToken = "";
      queryToken = "";
      localStorage.removeItem("codex_mp_token");
      showLoginDialog();
    }
    throw new Error("请先登录访问控制中心");
  }

  const data = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(formatApiError(data, response.status));
  }
  // The backend saves a change before asking a running Router to reload it. If
  // that reload fails the change is on disk but the Router keeps serving the old
  // revision, so the model the user just added would 404. The backend reports it
  // as `router_reload_warning`; without surfacing it here the user only saw
  // "已更新" and had no way to learn the Router needed a restart.
  if (data && typeof data.router_reload_warning === "string" && data.router_reload_warning) {
    markRouterNeedsRestart(data.router_reload_warning);
    notify("配置已保存，但后台路由仍在使用旧版本。", true, {
      label: "重启后台",
      onClick: () => restartRouter(),
    });
  }
  return data;
}

// ==========================================================================
// 2. 对话框、Snackbar 与工具提示
// ==========================================================================

const snackbarEl = document.querySelector("#m3-snackbar");
const snackbarMsg = document.querySelector("#snackbar-message");
const snackbarIcon = document.querySelector("#snackbar-icon");
const snackbarAction = document.querySelector("#snackbar-action");
let snackbarTimer = null;

// Snackbars queue instead of overwriting each other. A burst of operations
// (switch account -> refresh -> sync) used to leave only the last message on
// screen, which is usually the one that needed the earlier context.
const snackbarQueue = [];
let snackbarVisible = false;

function notify(text, error = false, action = null) {
  snackbarQueue.push({ text, error, action });
  if (!snackbarVisible) drainSnackbarQueue();
}

const API_ERROR_LABELS = {
  WebAccessDisabled: "浏览器访问尚未开启，请到设置中开启并保存访问密码。",
  PasswordRequired: "开启浏览器访问前，请先设置访问密码。",
  Unauthorized: "登录状态已失效，请重新登录控制台。",
  TooManyAttempts: "登录尝试过于频繁，请稍后再试。",
  NotFound: "没有找到对应的操作，请刷新页面后重试。",
};

function formatApiError(data, status) {
  const candidate = data && typeof data === "object" ? (data.message || data.error) : "";
  const raw = typeof candidate === "string" && candidate.trim() ? candidate.trim() : `请求失败 (${status})`;
  return API_ERROR_LABELS[raw] || raw;
}

function persistCatalogState() {
  try {
    localStorage.setItem(CATALOG_STATE_KEY, JSON.stringify(catalogState));
  } catch {
    // Storage is a convenience; the current view remains usable without it.
  }
}

function renderCatalogState() {
  const badge = document.querySelector("#catalog-state-badge");
  const setupSummary = document.querySelector("#setup-guide-summary");
  const applyButtons = [
    document.querySelector("#topbar-sync-btn"),
    document.querySelector("#setup-apply-btn"),
  ].filter(Boolean);
  let label = "Codex 列表已更新";
  let className = "m3-chip m3-chip--success";
  let summary = "模型已应用到 Codex，可在模型选择器或 /model 中使用。";
  if (catalogState.pending) {
    label = "有待应用变更";
    className = "m3-chip m3-chip--warning";
    summary = catalogState.reason || "模型配置已保存，还需要应用到 Codex。";
  } else if (catalogState.routerNeedsRestart) {
    label = "后台待重启";
    className = "m3-chip m3-chip--warning";
    summary = "模型列表已经更新，但后台路由需要重启后才会使用最新配置。";
  }
  if (badge) {
    badge.textContent = label;
    badge.className = className;
  }
  if (setupSummary && (catalogState.pending || catalogState.routerNeedsRestart)) {
    setupSummary.textContent = summary;
  }
  applyButtons.forEach((button) => {
    const text = button.querySelector(".m3-btn__label") || button.querySelector("span:last-child");
    if (text) {
      text.textContent = button.id === "setup-apply-btn"
        ? (catalogState.pending || catalogState.routerNeedsRestart ? "应用到 Codex" : "重新应用")
        : "应用到 Codex";
    }
    button.classList.toggle("m3-btn--attention", catalogState.pending || catalogState.routerNeedsRestart);
  });
  persistCatalogState();
  refreshSetupGuide();
}

function markCatalogPending(reason = "模型配置已保存，还需要应用到 Codex。") {
  catalogState.pending = true;
  catalogState.reason = reason;
  renderCatalogState();
}

function markCatalogApplied(catalogPath = "") {
  catalogState.pending = false;
  catalogState.reason = "";
  if (catalogPath) catalogState.catalogPath = catalogPath;
  renderCatalogState();
}

function markRouterNeedsRestart(reason = "") {
  catalogState.routerNeedsRestart = true;
  catalogState.reason = reason || catalogState.reason;
  renderCatalogState();
}

function clearRouterRestartAttention() {
  catalogState.routerNeedsRestart = false;
  catalogState.reason = "";
  renderCatalogState();
}

function drainSnackbarQueue() {
  const next = snackbarQueue.shift();
  if (!next) {
    snackbarVisible = false;
    return;
  }
  snackbarVisible = true;
  if (snackbarTimer) clearTimeout(snackbarTimer);

  snackbarMsg.textContent = next.text;
  snackbarIcon.className = `material-symbols-outlined m3-i-${next.error ? "error" : "check-circle"}`;
  snackbarEl.classList.toggle("m3-snackbar--error", !!next.error);

  if (next.action && next.action.label) {
    snackbarAction.textContent = next.action.label;
    snackbarAction.hidden = false;
    snackbarAction.onclick = () => {
      hideSnackbar();
      next.action.onClick?.();
    };
  } else {
    snackbarAction.hidden = true;
    snackbarAction.textContent = "";
    snackbarAction.onclick = null;
  }

  snackbarEl.classList.add("active");
  // Errors stay on screen longer than confirmations.
  snackbarTimer = setTimeout(hideSnackbar, next.error ? 6000 : 4000);
}

function hideSnackbar() {
  snackbarEl.classList.remove("active");
  if (snackbarTimer) {
    clearTimeout(snackbarTimer);
    snackbarTimer = null;
  }
  // Let the exit transition finish before the next message takes over.
  setTimeout(drainSnackbarQueue, 200);
}

snackbarAction.addEventListener("click", hideSnackbar);

// --- native <dialog> helpers ----------------------------------------------
// One wrapper for every modal so no caller can forget the bookkeeping.
// `dialog.showModal()` supplies the focus trap, Escape handling and an inert
// background from the platform; this only adds focus restore.

const openDialogs = new Map();

function openDialog(id) {
  const dialog = document.querySelector(`#${id}`);
  if (!dialog) {
    console.warn(`dialog not found: ${id}`);
    return null;
  }
  if (!openDialogs.has(id)) {
    openDialogs.set(id, { previousFocus: document.activeElement });
  }
  if (typeof dialog.showModal === "function") {
    if (!dialog.open) dialog.showModal();
  } else {
    dialog.setAttribute("open", "");
  }
  const autoFocus = dialog.querySelector("[autofocus], .m3-text-field__input, .m3-select-button");
  if (autoFocus) requestAnimationFrame(() => autoFocus.focus());
  return dialog;
}

function restoreDialogFocus(id) {
  const entry = openDialogs.get(id);
  openDialogs.delete(id);
  if (entry && entry.previousFocus && document.contains(entry.previousFocus)) {
    entry.previousFocus.focus();
  }
}

function closeDialog(id) {
  const dialog = document.querySelector(`#${id}`);
  if (!dialog) return;
  if (typeof dialog.close === "function" && dialog.open) {
    dialog.close();
  } else {
    dialog.removeAttribute("open");
  }
  restoreDialogFocus(id);
}

// --- M3 select / listbox --------------------------------------------------
// Keep a real <select> in the form so native validation, FormData and
// no-script fallbacks remain intact. The visible control is a themed
// listbox, which avoids handing the popup surface over to the OS/Electron
// native menu (the source of the white, unstyled protocol menu).
const m3SelectStates = new Set();
let m3SelectSequence = 0;

function syncM3Select(state) {
  const selected = state.select.options[state.select.selectedIndex];
  state.button.querySelector(".m3-select-button__label").textContent = selected
    ? selected.textContent
    : "请选择";
  state.optionButtons.forEach((optionButton, index) => {
    const isSelected = index === state.select.selectedIndex;
    optionButton.setAttribute("aria-selected", String(isSelected));
  });
  const selectedButton = state.optionButtons[state.select.selectedIndex];
  if (selectedButton) {
    state.activeIndex = state.select.selectedIndex;
    state.button.setAttribute("aria-activedescendant", selectedButton.id);
  } else {
    state.activeIndex = -1;
    state.button.removeAttribute("aria-activedescendant");
  }
}

function renderM3SelectOptions(state) {
  state.menu.replaceChildren();
  state.optionButtons = Array.from(state.select.options).map((option, index) => {
    const optionButton = document.createElement("button");
    optionButton.type = "button";
    optionButton.className = "m3-select-option";
    optionButton.id = `${state.id}-option-${index}`;
    optionButton.setAttribute("role", "option");
    optionButton.dataset.value = option.value;
    optionButton.textContent = option.textContent;
    optionButton.disabled = option.disabled;
    state.menu.appendChild(optionButton);
    return optionButton;
  });
  syncM3Select(state);
}

function closeM3Select(state) {
  state.menu.hidden = true;
  state.button.setAttribute("aria-expanded", "false");
}

function openM3Select(state) {
  m3SelectStates.forEach((other) => {
    if (other !== state) closeM3Select(other);
  });
  state.menu.hidden = false;
  state.button.setAttribute("aria-expanded", "true");
  state.activeIndex = state.select.selectedIndex;
  const selectedButton = state.optionButtons[state.activeIndex];
  selectedButton?.scrollIntoView({ block: "nearest" });
}

function chooseM3SelectOption(state, index) {
  const option = state.select.options[index];
  if (!option || option.disabled) return;
  state.select.value = option.value;
  state.select.dispatchEvent(new Event("change", { bubbles: true }));
  closeM3Select(state);
  state.button.focus();
}

function moveM3SelectActive(state, direction) {
  if (!state.optionButtons.length) return;
  let index = state.activeIndex;
  do {
    index = (index + direction + state.optionButtons.length) % state.optionButtons.length;
  } while (state.optionButtons[index].disabled && index !== state.activeIndex);
  state.activeIndex = index;
  const active = state.optionButtons[index];
  state.button.setAttribute("aria-activedescendant", active.id);
  active.scrollIntoView({ block: "nearest" });
}

function initM3Select(select) {
  if (!select || select.dataset.m3SelectReady === "true") {
    return m3SelectStates.values().find((state) => state.select === select) || null;
  }
  const control = select.closest(".m3-text-field__control");
  if (!control) return null;

  const label = control.querySelector(".m3-text-field__label");
  if (label && !label.id) label.id = `${select.id || "m3-select"}-label`;
  const id = select.id || `m3-select-${++m3SelectSequence}`;
  const button = document.createElement("button");
  button.type = "button";
  button.className = "m3-select m3-select-button";
  button.setAttribute("role", "combobox");
  button.setAttribute("aria-haspopup", "listbox");
  button.setAttribute("aria-expanded", "false");
  if (label) button.setAttribute("aria-labelledby", label.id);
  button.innerHTML = '<span class="m3-select-button__label"></span>';

  const menu = document.createElement("div");
  menu.className = "m3-select-menu";
  menu.id = `${id}-menu`;
  menu.setAttribute("role", "listbox");
  menu.hidden = true;
  button.setAttribute("aria-controls", menu.id);

  const state = {
    id,
    select,
    button,
    menu,
    optionButtons: [],
    activeIndex: -1,
    observer: null,
  };
  m3SelectStates.add(state);
  select.dataset.m3SelectReady = "true";
  select.classList.add("m3-select--native");
  select.tabIndex = -1;
  select.setAttribute("aria-hidden", "true");
  select.setAttribute("aria-controls", menu.id);
  control.insertBefore(button, select);
  control.appendChild(menu);

  renderM3SelectOptions(state);
  select.addEventListener("change", () => syncM3Select(state));
  button.addEventListener("click", () => {
    if (state.menu.hidden) openM3Select(state);
    else closeM3Select(state);
  });
  button.addEventListener("keydown", (event) => {
    if (["ArrowDown", "ArrowUp", "Home", "End", "Enter", " ", "Escape"].includes(event.key)) {
      event.preventDefault();
    }
    if (event.key === "Escape") {
      closeM3Select(state);
      return;
    }
    if (event.key === "ArrowDown") {
      if (state.menu.hidden) openM3Select(state);
      moveM3SelectActive(state, 1);
    } else if (event.key === "ArrowUp") {
      if (state.menu.hidden) openM3Select(state);
      moveM3SelectActive(state, -1);
    } else if (event.key === "Home") {
      if (state.menu.hidden) openM3Select(state);
      const firstEnabled = state.optionButtons.findIndex((optionButton) => !optionButton.disabled);
      if (firstEnabled >= 0) {
        state.activeIndex = firstEnabled;
        state.button.setAttribute("aria-activedescendant", state.optionButtons[firstEnabled].id);
        state.optionButtons[firstEnabled].scrollIntoView({ block: "nearest" });
      }
    } else if (event.key === "End") {
      if (state.menu.hidden) openM3Select(state);
      let lastEnabled = -1;
      for (let index = state.optionButtons.length - 1; index >= 0; index -= 1) {
        if (!state.optionButtons[index].disabled) {
          lastEnabled = index;
          break;
        }
      }
      if (lastEnabled >= 0) {
        state.activeIndex = lastEnabled;
        state.button.setAttribute("aria-activedescendant", state.optionButtons[lastEnabled].id);
        state.optionButtons[lastEnabled].scrollIntoView({ block: "nearest" });
      }
    } else if (event.key === "Enter" || event.key === " ") {
      if (state.menu.hidden) openM3Select(state);
      else chooseM3SelectOption(state, state.activeIndex);
    }
  });
  menu.addEventListener("click", (event) => {
    const optionButton = event.target.closest(".m3-select-option");
    if (!optionButton) return;
    chooseM3SelectOption(state, state.optionButtons.indexOf(optionButton));
  });
  state.observer = new MutationObserver(() => renderM3SelectOptions(state));
  state.observer.observe(select, { childList: true });
  return state;
}

function setM3SelectValue(select, value) {
  if (!select) return;
  select.value = value == null ? "" : String(value);
  select.dispatchEvent(new Event("change", { bubbles: true }));
}

function initM3Selects() {
  document.querySelectorAll("select.m3-select").forEach(initM3Select);
  document.querySelectorAll("form").forEach((form) => {
    if (form.dataset.m3SelectResetReady === "true") return;
    form.dataset.m3SelectResetReady = "true";
    form.addEventListener("reset", () => {
      requestAnimationFrame(() => {
        form.querySelectorAll("select.m3-select").forEach((select) => {
          const state = Array.from(m3SelectStates).find((item) => item.select === select);
          if (state) syncM3Select(state);
        });
      });
    });
  });
}

document.addEventListener("pointerdown", (event) => {
  m3SelectStates.forEach((state) => {
    if (!state.menu.hidden && !state.menu.contains(event.target) && event.target !== state.button) {
      closeM3Select(state);
    }
  });
});

document.querySelectorAll("dialog.m3-dialog").forEach((dialog) => {
  // Clicking the backdrop cancels, matching platform convention for
  // non-destructive dialogs.
  dialog.addEventListener("click", (event) => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    const inside =
      event.clientX >= rect.left &&
      event.clientX <= rect.right &&
      event.clientY >= rect.top &&
      event.clientY <= rect.bottom;
    if (!inside) closeDialog(dialog.id);
  });
  // Escape closes the dialog natively; this only handles focus restore.
  dialog.addEventListener("close", () => restoreDialogFocus(dialog.id));
});

// 通用 M3 异步 Prompt 对话框
function m3Prompt(title, desc = "", defaultValue = "") {
  return new Promise((resolve) => {
    const dialog = document.querySelector("#prompt-dialog");
    const titleEl = document.querySelector("#prompt-dialog-title");
    const descEl = document.querySelector("#prompt-dialog-desc");
    const inputEl = document.querySelector("#prompt-dialog-input");
    const formEl = document.querySelector("#prompt-dialog-form");
    const cancelBtn = document.querySelector("#prompt-dialog-cancel");

    titleEl.textContent = title;
    descEl.textContent = desc;
    inputEl.value = defaultValue;
    openDialog("prompt-dialog");
    inputEl.select?.();

    let settled = false;
    const cleanup = () => {
      formEl.onsubmit = null;
      cancelBtn.onclick = null;
      dialog.oncancel = null;
      closeDialog("prompt-dialog");
    };
    const finish = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    };

    formEl.onsubmit = (e) => {
      e.preventDefault();
      finish(inputEl.value);
    };
    cancelBtn.onclick = () => finish(null);
    // Escape closes the native dialog without running our handlers, so treat
    // it as a cancel and resolve exactly once.
    dialog.oncancel = (e) => {
      e.preventDefault();
      finish(null);
    };
  });
}

// 通用 M3 异步 Confirm 对话框。
// `options.checkbox` renders an optional opt-in, which folds the old
// two-consecutive-confirms flow (delete provider -> also purge credential?)
// into a single decision.
function m3Confirm(title, desc = "", options = {}) {
  return new Promise((resolve) => {
    const dialog = document.querySelector("#confirm-dialog");
    const titleEl = document.querySelector("#confirm-dialog-title");
    const descEl = document.querySelector("#confirm-dialog-desc");
    const confirmBtn = document.querySelector("#confirm-dialog-confirm");
    const cancelBtn = document.querySelector("#confirm-dialog-cancel");
    const checkboxRow = document.querySelector("#confirm-dialog-checkbox-row");
    const checkbox = document.querySelector("#confirm-dialog-checkbox");
    const checkboxLabel = document.querySelector("#confirm-dialog-checkbox-label");
    const iconWrap = document.querySelector("#confirm-dialog-icon");
    const iconEl = iconWrap.querySelector(".material-symbols-outlined");

    titleEl.textContent = title;
    descEl.textContent = desc;

    const destructive = options.destructive !== false;
    iconWrap.classList.toggle("m3-dialog__icon--error", destructive);
    iconEl.className = `material-symbols-outlined m3-i-${destructive ? "delete" : "help"}`;
    confirmBtn.classList.toggle("m3-btn--danger", destructive);
    confirmBtn.textContent = options.confirmLabel || "确认操作";

    if (options.checkbox) {
      checkboxRow.classList.remove("is-hidden");
      checkboxLabel.textContent = options.checkbox.label;
      checkbox.checked = !!options.checkbox.defaultChecked;
    } else {
      checkboxRow.classList.add("is-hidden");
      checkbox.checked = false;
    }

    openDialog("confirm-dialog");

    let settled = false;
    const cleanup = () => {
      confirmBtn.onclick = null;
      cancelBtn.onclick = null;
      dialog.oncancel = null;
      closeDialog("confirm-dialog");
    };
    const finish = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    };
    const cancelled = options.checkbox ? { confirmed: false, checked: false } : false;

    confirmBtn.onclick = () =>
      finish(options.checkbox ? { confirmed: true, checked: checkbox.checked } : true);
    cancelBtn.onclick = () => finish(cancelled);
    dialog.oncancel = (e) => {
      e.preventDefault();
      finish(cancelled);
    };
  });
}

// Upgrade a `data-tooltip` attribute into a themed, keyboard-visible bubble.
// Native `title` is slow, unthemed and unreachable by keyboard.
function attachTooltip(element, text) {
  if (!element || !text) return;
  element.classList.add("m3-tooltip-host");
  if (!element.hasAttribute("aria-label")) element.setAttribute("aria-label", text);
  if (element.querySelector(":scope > .m3-tooltip")) return;
  const tip = document.createElement("span");
  tip.className = "m3-tooltip";
  tip.setAttribute("role", "tooltip");
  tip.textContent = text;
  element.appendChild(tip);
}

function applyTooltips(root = document) {
  root.querySelectorAll("[data-tooltip]").forEach((el) => {
    attachTooltip(el, el.getAttribute("data-tooltip"));
  });
}

// ==========================================================================
// 3. 工具函数与偏好设置
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

function escapeAttr(value) {
  return escapeHtml(value);
}

// `plan_type` originates from an imported ID-token claim, i.e. it is
// attacker-influenced text. It is interpolated into a class attribute, where
// HTML escaping alone is not enough to stop an attribute break-out, so only a
// fixed allow-list may ever reach the class name.
const knownPlanClasses = new Set(["plus", "pro", "team", "free"]);

function sanitizePlanClass(value) {
  const normalized = String(value ?? "").trim().toLowerCase();
  return knownPlanClasses.has(normalized) ? normalized : "unknown";
}

// A null-object stand-in for a missing element. Binding through bindClick() must
// never throw, otherwise one renamed id in index.html would abort this script and
// leave the whole panel blank.
const detachedElementStub = {
  set onclick(_handler) {},
  set onsubmit(_handler) {},
  set onchange(_handler) {},
  classList: { add() {}, remove() {}, toggle() {} },
  setAttribute() {},
  removeAttribute() {},
  querySelector: () => null,
  appendChild() {},
};

function bindClick(selector) {
  const element = document.querySelector(selector);
  if (element) return element;
  console.warn(`panel element not found: ${selector}`);
  return detachedElementStub;
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

// --- preferences -----------------------------------------------------------
// Theme is three-state (system/light/dark); contrast and motion are separate
// axes that apply independently of the OS.

const THEME_KEY = "codex_mp_theme";
const CONTRAST_KEY = "codex_mp_contrast";
const MOTION_KEY = "codex_mp_motion";

const prefersDark = window.matchMedia("(prefers-color-scheme: dark)");
const prefersReducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

const VALID_THEMES = new Set(["system", "light", "dark"]);
const VALID_CONTRAST = new Set(["standard", "high"]);
const VALID_MOTION = new Set(["full", "reduced"]);

function readPref(key, valid, fallback) {
  const raw = localStorage.getItem(key);
  return valid.has(raw) ? raw : fallback;
}

// Migrate the old two-state value so an existing install keeps its choice.
{
  const legacy = localStorage.getItem(THEME_KEY);
  if (legacy === "light" || legacy === "dark") {
    // already valid under the new three-state scheme
  } else if (legacy) {
    localStorage.removeItem(THEME_KEY);
  }
}

let currentTheme = readPref(THEME_KEY, VALID_THEMES, "system");
let currentContrast = readPref(CONTRAST_KEY, VALID_CONTRAST, "standard");
// With no explicit choice, follow the OS reduced-motion setting. Seeding this
// from the media query (rather than always "full") keeps the settings control
// honest: otherwise it read "完整/Full" while the CSS media query was already
// suppressing animation.
let currentMotion = readPref(
  MOTION_KEY,
  VALID_MOTION,
  prefersReducedMotion.matches ? "reduced" : "full",
);

function effectiveTheme() {
  if (currentTheme === "system") return prefersDark.matches ? "dark" : "light";
  return currentTheme;
}

function applyPreferences() {
  const root = document.documentElement;
  root.setAttribute("data-theme", currentTheme);
  root.setAttribute("data-contrast", currentContrast);
  root.setAttribute("data-motion", currentMotion);
  updateThemeIcon();
  syncPreferenceControls();
}

function updateThemeIcon() {
  const icon = document.querySelector("#theme-icon");
  if (!icon) return;
  const name = currentTheme === "system" ? "brightness-auto"
    : (effectiveTheme() === "dark" ? "dark-mode" : "light-mode");
  icon.className = `material-symbols-outlined m3-i-${name}`;
  const btn = document.querySelector("#theme-menu-btn");
  if (btn) {
    const label = currentTheme === "system"
      ? "外观主题：跟随系统"
      : `外观主题：${effectiveTheme() === "dark" ? "深色" : "浅色"}`;
    btn.setAttribute("aria-label", label);
  }
}

function setTheme(theme) {
  if (!VALID_THEMES.has(theme)) return;
  currentTheme = theme;
  localStorage.setItem(THEME_KEY, theme);
  applyPreferences();
  const names = { system: "跟随系统", light: "浅色", dark: "深色" };
  notify(`外观主题已切换为：${names[theme] || theme}`);
}

function setContrast(contrast) {
  if (!VALID_CONTRAST.has(contrast)) return;
  currentContrast = contrast;
  localStorage.setItem(CONTRAST_KEY, contrast);
  applyPreferences();
  const names = { standard: "标准对比度", high: "高对比度" };
  notify(`对比度已设置为：${names[contrast] || contrast}`);
}

function setMotion(motion) {
  if (!VALID_MOTION.has(motion)) return;
  currentMotion = motion;
  localStorage.setItem(MOTION_KEY, motion);
  applyPreferences();
  const names = { full: "完整动效", reduced: "减弱动效" };
  notify(`动效已设置为：${names[motion] || motion}`);
}

// Reflect the stored preferences onto both the app-bar menu and the settings
// segmented controls.
function syncPreferenceControls() {
  document.querySelectorAll("[data-theme-value]").forEach((el) => {
    const active = el.getAttribute("data-theme-value") === currentTheme;
    if (el.getAttribute("role") === "menuitemradio") {
      el.setAttribute("aria-checked", String(active));
    } else {
      el.setAttribute("aria-pressed", String(active));
    }
  });
  document.querySelectorAll("[data-contrast-value]").forEach((el) => {
    el.setAttribute("aria-pressed", String(el.getAttribute("data-contrast-value") === currentContrast));
  });
  document.querySelectorAll("[data-motion-value]").forEach((el) => {
    el.setAttribute("aria-pressed", String(el.getAttribute("data-motion-value") === currentMotion));
  });
}

// Follow the OS while in `system` mode.
prefersDark.addEventListener("change", () => {
  if (currentTheme === "system") updateThemeIcon();
});
// Adopt the OS reduced-motion default only while the user has not overridden it.
prefersReducedMotion.addEventListener("change", (e) => {
  if (!localStorage.getItem(MOTION_KEY)) {
    currentMotion = e.matches ? "reduced" : "full";
    applyPreferences();
  }
});

applyPreferences();

// --- theme menu (app bar) --------------------------------------------------

const themeMenuBtn = document.querySelector("#theme-menu-btn");
const themeMenu = document.querySelector("#theme-menu");

function setThemeMenuOpen(open) {
  themeMenu.hidden = !open;
  themeMenuBtn.setAttribute("aria-expanded", String(open));
  if (open) {
    const current = themeMenu.querySelector('[aria-checked="true"]') || themeMenu.querySelector(".m3-menu__item");
    current?.focus();
  }
}

themeMenuBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  setThemeMenuOpen(themeMenu.hidden);
});

themeMenu.querySelectorAll("[data-theme-value]").forEach((item) => {
  item.addEventListener("click", () => {
    setTheme(item.getAttribute("data-theme-value"));
    setThemeMenuOpen(false);
    themeMenuBtn.focus();
  });
});

document.addEventListener("click", (e) => {
  if (!themeMenu.hidden && !themeMenu.contains(e.target) && e.target !== themeMenuBtn) {
    setThemeMenuOpen(false);
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !themeMenu.hidden) {
    setThemeMenuOpen(false);
    themeMenuBtn.focus();
  }
});

themeMenu.addEventListener("keydown", (e) => {
  const items = [...themeMenu.querySelectorAll(".m3-menu__item")];
  const index = items.indexOf(document.activeElement);
  if (e.key === "ArrowDown") {
    e.preventDefault();
    items[(index + 1) % items.length].focus();
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    items[(index - 1 + items.length) % items.length].focus();
  }
});

document.querySelectorAll("#settings-theme-group [data-theme-value]").forEach((btn) => {
  btn.addEventListener("click", () => setTheme(btn.getAttribute("data-theme-value")));
});
document.querySelectorAll("#settings-contrast-group [data-contrast-value]").forEach((btn) => {
  btn.addEventListener("click", () => setContrast(btn.getAttribute("data-contrast-value")));
});
document.querySelectorAll("#settings-motion-group [data-motion-value]").forEach((btn) => {
  btn.addEventListener("click", () => setMotion(btn.getAttribute("data-motion-value")));
});

// --- password visibility toggles ------------------------------------------

document.querySelectorAll("[data-toggle-password]").forEach((btn) => {
  btn.addEventListener("click", () => {
    const input = btn.closest(".m3-text-field")?.querySelector("input");
    if (!input) return;
    const showing = input.type === "text";
    input.type = showing ? "password" : "text";
    const icon = btn.querySelector(".material-symbols-outlined");
    if (icon) icon.className = `material-symbols-outlined m3-i-${showing ? "visibility" : "visibility-off"}`;
    btn.setAttribute("aria-label", showing ? "显示密钥" : "隐藏密钥");
  });
});

// --- busy state for async buttons -----------------------------------------
// Wraps an async handler: disables the control, swaps its leading icon for the
// Expressive loading indicator, and marks it busy for assistive tech.

async function withBusy(button, task) {
  if (!button || button === detachedElementStub) return task();
  const icon = button.querySelector(".material-symbols-outlined");
  const originalIconClass = icon ? icon.className : null;
  button.disabled = true;
  button.setAttribute("aria-busy", "true");
  if (icon) {
    icon.className = "m3-loading-indicator m3-loading-indicator--sm";
    icon.textContent = "";
  }
  try {
    return await task();
  } finally {
    button.disabled = false;
    button.removeAttribute("aria-busy");
    if (icon && originalIconClass) icon.className = originalIconClass;
  }
}

// ==========================================================================
// 4. 路由与外壳行为
// ==========================================================================

const navItems = [...document.querySelectorAll(".m3-nav-item")];
const viewSections = [...document.querySelectorAll(".m3-view-section")];
const VIEW_ORDER = navItems.map((n) => n.getAttribute("data-target"));

function activateView(targetId, { updateHash = true } = {}) {
  const index = VIEW_ORDER.indexOf(targetId);
  if (index === -1) return;

  const currentId = document.querySelector(".m3-view-section.active")?.id || "";
  const currentIndex = VIEW_ORDER.indexOf(currentId);

  navItems.forEach((n) => {
    const active = n.getAttribute("data-target") === targetId;
    n.classList.toggle("active", active);
    if (active) n.setAttribute("aria-current", "page");
    else n.removeAttribute("aria-current");
  });

  viewSections.forEach((sec) => {
    const active = sec.id === targetId;
    sec.classList.remove("m3-view--forward", "m3-view--backward");
    // Direction drives the shared-axis transition.
    if (active && currentIndex !== -1 && index !== currentIndex) {
      sec.classList.add(index > currentIndex ? "m3-view--forward" : "m3-view--backward");
    }
    sec.classList.toggle("active", active);
    if (active) sec.removeAttribute("hidden");
    else sec.setAttribute("hidden", "");
  });

  if (updateHash && window.location.hash !== `#${targetId}`) {
    window.history.replaceState(null, "", `#${targetId}`);
  }
}

navItems.forEach((btn, i) => {
  btn.addEventListener("click", () => activateView(btn.getAttribute("data-target")));

  // Roving keyboard navigation: the rail/drawer reads vertically, the bottom
  // bar horizontally.
  btn.addEventListener("keydown", (e) => {
    const vertical = window.matchMedia("(min-width: 600px)").matches;
    const nextKey = vertical ? "ArrowDown" : "ArrowRight";
    const prevKey = vertical ? "ArrowUp" : "ArrowLeft";
    let target = null;
    if (e.key === nextKey) target = navItems[(i + 1) % navItems.length];
    else if (e.key === prevKey) target = navItems[(i - 1 + navItems.length) % navItems.length];
    else if (e.key === "Home") target = navItems[0];
    else if (e.key === "End") target = navItems[navItems.length - 1];
    if (target) {
      e.preventDefault();
      target.focus();
      activateView(target.getAttribute("data-target"));
    }
  });
});

// Restore the view named in the hash, so a reload keeps the user in place.
function activateViewFromHash() {
  const target = window.location.hash.replace("#", "");
  if (!target || !VIEW_ORDER.includes(target)) return;
  const current = document.querySelector(".m3-view-section.active")?.id;
  if (current === target) return;
  activateView(target, { updateHash: false });
}

activateViewFromHash();

// A link or bookmark that differs only by fragment does not reload the
// document, so the initial read above never runs. Listen for the change too,
// otherwise the URL and the visible view disagree.
window.addEventListener("hashchange", activateViewFromHash);

// The app bar gains elevation once content scrolls under it.
{
  const appBar = document.querySelector("#top-app-bar");
  const mainContent = document.querySelector("#main-content");
  const onScroll = () => {
    const scrolled = (mainContent ? mainContent.scrollTop : 0) > 4 || window.scrollY > 4;
    appBar.classList.toggle("m3-top-app-bar--scrolled", scrolled);
  };
  if (mainContent) {
    mainContent.addEventListener("scroll", onScroll, { passive: true });
  }
  window.addEventListener("scroll", onScroll, { passive: true });
  onScroll();
}

// ==========================================================================
// 5. 额度与进度组件
// ==========================================================================

function progressStateClass(percent) {
  if (percent >= 90) return "m3-progress__indicator--danger";
  if (percent >= 70) return "m3-progress__indicator--warning";
  return "";
}

function wavyStateClass(percent) {
  if (percent >= 90) return "m3-progress-wavy__fill--danger";
  if (percent >= 70) return "m3-progress-wavy__fill--warning";
  return "";
}

// Primary quota uses the Expressive wavy indicator; secondary rows use the
// compact 4dp linear one.
function renderUsageMetric(title, windowData, { variant = "linear" } = {}) {
  if (!windowData) {
    return `
      <div class="m3-progress">
        <div class="m3-progress__head">
          <span class="m3-label-medium">${escapeHtml(title)}</span>
          <span class="m3-body-small m3-text-muted">无数据 / 未开启</span>
        </div>
        <div class="m3-progress__track">
          <div class="m3-progress__indicator" style="--m3-progress-value: 0"></div>
        </div>
      </div>
    `;
  }

  const percent = Math.min(100, Math.max(0, windowData.used_percent || 0));
  const resetDesc = formatRemainingTime(windowData.reset_after_seconds);
  const valueColor = percent >= 90 ? "m3-text-error" : "m3-text-primary";

  if (variant === "wavy") {
    return `
      <div class="m3-progress">
        <div class="m3-progress__head">
          <span class="m3-label-medium">${escapeHtml(title)}</span>
          <span class="m3-label-large m3-numeric ${valueColor}">${percent}%</span>
        </div>
        <div class="m3-progress-wavy" role="progressbar" aria-valuenow="${percent}" aria-valuemin="0" aria-valuemax="100" aria-label="${escapeAttr(title)}">
          <div class="m3-progress-wavy__track"></div>
          <div class="m3-progress-wavy__fill ${wavyStateClass(percent)}" style="--m3-progress-value: ${percent}%"></div>
        </div>
        <div class="m3-progress__meta">
          <span class="m3-body-small m3-text-variant">${escapeHtml(resetDesc)}</span>
        </div>
      </div>
    `;
  }

  return `
    <div class="m3-progress">
      <div class="m3-progress__head">
        <span class="m3-label-medium">${escapeHtml(title)}</span>
        <span class="m3-label-large m3-numeric ${valueColor}">${percent}%</span>
      </div>
      <div class="m3-progress__track" role="progressbar" aria-valuenow="${percent}" aria-valuemin="0" aria-valuemax="100" aria-label="${escapeAttr(title)}">
        <div class="m3-progress__indicator ${progressStateClass(percent)}" style="--m3-progress-value: ${percent / 100}"></div>
      </div>
      <div class="m3-progress__meta">
        <span class="m3-body-small m3-text-variant">${escapeHtml(resetDesc)}</span>
      </div>
    </div>
  `;
}

function renderReserveMetric(reserve) {
  if (!reserve) return renderUsageMetric("GPT Reserve 备用额度", null);

  const percent = reserve.used_percent != null
    ? Math.min(100, Math.max(0, reserve.used_percent))
    : (reserve.limit_reached ? 100 : 0);

  if (reserve.limit_reached) {
    const resetDesc = formatRemainingTime(reserve.reset_after_seconds);
    return `
      <div class="m3-progress">
        <div class="m3-progress__head">
          <span class="m3-label-medium">GPT Reserve 备用额度</span>
          <span class="m3-label-large m3-text-error">已达上限</span>
        </div>
        <div class="m3-progress__track" role="progressbar" aria-valuenow="100" aria-valuemin="0" aria-valuemax="100">
          <div class="m3-progress__indicator m3-progress__indicator--danger" style="--m3-progress-value: 1"></div>
        </div>
        <div class="m3-progress__meta">
          <span class="m3-body-small m3-text-variant">${escapeHtml(resetDesc)}</span>
        </div>
      </div>
    `;
  }

  return renderUsageMetric("GPT Reserve 备用额度", {
    used_percent: percent,
    reset_after_seconds: reserve.reset_after_seconds,
  });
}

// --- state placeholders ----------------------------------------------------

function skeletonCards(count = 2) {
  return Array.from({ length: count }, () => `
    <div class="m3-card m3-card-filled">
      <div class="m3-skeleton-stack">
        <div class="m3-skeleton m3-skeleton--title"></div>
        <div class="m3-skeleton m3-skeleton--text"></div>
        <div class="m3-skeleton m3-skeleton--card"></div>
      </div>
    </div>
  `).join("");
}

function emptyState({ icon, title, body, actionLabel, actionId }) {
  return `
    <div class="m3-empty-state">
      <span class="material-symbols-outlined m3-empty-state__icon m3-i-${icon}" aria-hidden="true"></span>
      <p class="m3-title-medium m3-empty-state__title">${escapeHtml(title)}</p>
      <p class="m3-body-small m3-empty-state__body">${escapeHtml(body)}</p>
      ${actionLabel ? `<button class="m3-btn m3-btn-tonal" id="${actionId}">${escapeHtml(actionLabel)}</button>` : ""}
    </div>
  `;
}

function errorState({ title, body, retryId }) {
  return `
    <div class="m3-error-state" role="alert">
      <span class="material-symbols-outlined m3-error-state__icon m3-i-error" aria-hidden="true"></span>
      <p class="m3-title-medium m3-error-state__title">${escapeHtml(title)}</p>
      <p class="m3-body-small m3-error-state__body">${escapeHtml(body)}</p>
      <button class="m3-btn m3-btn-tonal" id="${retryId}">
        <span class="material-symbols-outlined m3-i-refresh" aria-hidden="true"></span>
        <span>重试</span>
      </button>
    </div>
  `;
}

function refreshSetupGuide() {
  const guide = document.querySelector("#setup-guide");
  if (!guide) return;
  const providers = Array.isArray(cachedProviders) ? cachedProviders : [];
  const providerReady = providers.length > 0;
  const modelReady = providers.some((provider) => (provider.models || []).some((model) => model.enabled !== false));
  const routerReady = Boolean(lastRouterStatus?.healthy);
  const steps = {
    provider: guide.querySelector('[data-setup-step="provider"]'),
    model: guide.querySelector('[data-setup-step="model"]'),
    apply: guide.querySelector('[data-setup-step="apply"]'),
  };
  const markStep = (step, state) => {
    if (!step) return;
    step.classList.toggle("is-complete", state === "complete");
    step.classList.toggle("is-current", state === "current");
    step.classList.toggle("is-muted", state === "muted");
    const number = step.querySelector(".m3-setup-step__number");
    if (number) number.textContent = state === "complete" ? "✓" : number.dataset.stepNumber || number.textContent;
  };
  Object.values(steps).forEach((step) => {
    if (step) {
      const number = step.querySelector(".m3-setup-step__number");
      if (number && !number.dataset.stepNumber) number.dataset.stepNumber = number.textContent;
    }
  });

  markStep(steps.provider, providerReady ? "complete" : "current");
  markStep(steps.model, !providerReady ? "muted" : modelReady ? "complete" : "current");
  markStep(steps.apply, !modelReady ? "muted" : catalogState.pending || catalogState.routerNeedsRestart ? "current" : "complete");

  const title = document.querySelector("#setup-guide-title");
  const summary = document.querySelector("#setup-guide-summary");
  const kicker = document.querySelector("#setup-guide-kicker");
  const addProvider = document.querySelector("#setup-add-provider-btn");
  const openModels = document.querySelector("#setup-open-models-btn");
  const apply = document.querySelector("#setup-apply-btn");
  if (!providerReady) {
    if (kicker) kicker.textContent = "从这里开始";
    if (title) title.textContent = "把第一个模型接入 Codex";
    if (summary) summary.textContent = "添加一个模型服务商，选择模型，然后应用到 Codex。";
    if (addProvider) addProvider.textContent = "添加服务商";
    if (openModels) openModels.disabled = true;
    if (apply) apply.disabled = true;
  } else if (!modelReady) {
    if (kicker) kicker.textContent = "还差一步";
    if (title) title.textContent = "选择要显示在 Codex 里的模型";
    if (summary) summary.textContent = "服务商已保存。打开模型页并点击发现模型，选择需要的模型。";
    if (addProvider) addProvider.textContent = "再加一个服务商";
    if (openModels) openModels.disabled = false;
    if (apply) apply.disabled = true;
  } else if (catalogState.pending || catalogState.routerNeedsRestart) {
    if (kicker) kicker.textContent = "需要应用";
    if (title) title.textContent = catalogState.routerNeedsRestart ? "后台需要重启" : "模型配置已准备好";
    if (summary) summary.textContent = catalogState.routerNeedsRestart
      ? "模型列表已更新，但后台路由仍在使用旧配置。"
      : "点击应用，模型就会出现在 Codex 的模型选择列表中。";
    if (addProvider) addProvider.textContent = "管理服务商";
    if (openModels) openModels.disabled = false;
    if (apply) {
      apply.disabled = false;
      apply.textContent = catalogState.routerNeedsRestart ? "重启后台并应用" : "应用到 Codex";
    }
  } else {
    if (kicker) kicker.textContent = "已准备好";
    if (title) title.textContent = "模型已可在 Codex 中使用";
    if (summary) summary.textContent = routerReady
      ? "打开 Codex 的模型选择器或输入 /model，即可切换模型。"
      : "模型列表已应用；使用第三方模型前请确保后台路由正在运行。";
    if (addProvider) addProvider.textContent = "管理服务商";
    if (openModels) openModels.disabled = false;
    if (apply) {
      apply.disabled = false;
      apply.textContent = "重新应用";
    }
  }
}

// ==========================================================================
function cleanPath(p) {
  if (typeof p !== "string") return p;
  if (p.startsWith("\\\\?\\UNC\\")) return "\\\\" + p.slice(8);
  if (p.startsWith("\\\\?\\")) return p.slice(4);
  return p;
}

async function applyCatalog({ button = null, silent = false } = {}) {
  const run = async () => {
    const restartAfterApply = catalogState.routerNeedsRestart;
    if (!silent) notify("正在应用模型列表到 Codex…");
    try {
      const res = await api("/api/v1/catalog/sync", { method: "POST" });
      markCatalogApplied(res.catalog_path || "");
      if (restartAfterApply || catalogState.routerNeedsRestart) {
        if (!silent) notify("正在重启后台路由以载入最新模型…");
        await api("/api/v1/router/restart", { method: "POST" });
        clearRouterRestartAttention();
      }
      if (!silent) notify("模型列表已应用到 Codex");
      await refreshRouterStatus();
      return res;
    } catch (error) {
      markCatalogPending("模型配置已保存，但应用到 Codex 失败。请检查 Codex 路径后重试。");
      if (!silent) notify(error.message, true);
      throw error;
    }
  };
  return button ? withBusy(button, run) : run();
}

async function restartRouter() {
  const restartBtn = document.querySelector("#router-restart-btn");
  return withBusy(restartBtn, async () => {
    try {
      notify("正在重启后台路由…");
      await api("/api/v1/router/restart", { method: "POST" });
      clearRouterRestartAttention();
      notify("后台路由已重启并就绪");
      await refreshRouterStatus();
    } catch (error) {
      notify(`后台路由重启失败：${error.message}`, true);
      await refreshRouterStatus();
      throw error;
    }
  });
}

// 6. 状态刷新：Router / Desktop / 安全
// ==========================================================================

let cachedProviders = [];
let cachedAccounts = [];
let modelFilterQuery = "";
let modelFilterEnabledOnly = false;
let routerStatusPollTimer = null;
let routerStatusRequestInFlight = false;

function scheduleRouterStatusPoll(delay = 1500) {
  if (routerStatusPollTimer) clearTimeout(routerStatusPollTimer);
  routerStatusPollTimer = setTimeout(() => {
    routerStatusPollTimer = null;
    void refreshRouterStatus();
  }, delay);
}

async function refreshRouterStatus() {
  if (routerStatusRequestInFlight) return;
  routerStatusRequestInFlight = true;
  const badge = document.querySelector("#router-state-badge");
  const text = document.querySelector("#router-state-text");
  const metricVal = document.querySelector("#metric-router-val");
  const metricDesc = document.querySelector("#metric-router-desc");
  const restartBtn = document.querySelector("#router-restart-btn");
  let pollAgain = false;

  try {
    const status = await api("/api/v1/router/status");
    lastRouterStatus = status;
    if (status.healthy) {
      badge.className = "m3-chip m3-chip--success";
      text.textContent = "后台路由正常";
      metricVal.textContent = "在线运行中";
      metricDesc.textContent = "官方与第三方模型分流正常";
      if (restartBtn) restartBtn.classList.remove("is-hidden");
      pollAgain = false;
    } else if (status.running && status.starting) {
      badge.className = "m3-chip m3-chip--warning";
      text.textContent = "后台路由启动中";
      metricVal.textContent = "启动中…";
      metricDesc.textContent = "正在建立端点连接";
      if (restartBtn) restartBtn.classList.add("is-hidden");
      pollAgain = true;
    } else if (status.running) {
      badge.className = "m3-chip m3-chip--warning";
      text.textContent = "后台路由异常";
      metricVal.textContent = "运行异常";
      metricDesc.textContent = status.last_error
        ? `需要处理：${status.last_error}`
        : "后台服务未通过健康检查";
      if (restartBtn) restartBtn.classList.remove("is-hidden");
      pollAgain = !status.last_error;
    } else {
      badge.className = "m3-chip m3-chip--error";
      text.textContent = "后台路由未运行";
      metricVal.textContent = status.last_error ? "启动失败" : "离线";
      metricDesc.textContent = status.last_error
        ? `启动失败：${status.last_error}`
        : "后台服务未启动，点击重启即可尝试恢复";
      if (restartBtn) restartBtn.classList.remove("is-hidden");
      pollAgain = !status.last_error;
    }
    refreshSetupGuide();
  } catch {
    lastRouterStatus = null;
    badge.className = "m3-chip m3-chip--warning";
    text.textContent = "后台路由状态未知";
    metricVal.textContent = "未知";
    metricDesc.textContent = "无法获取后台状态";
    if (restartBtn) restartBtn.classList.remove("is-hidden");
    refreshSetupGuide();
    pollAgain = true;
  } finally {
    routerStatusRequestInFlight = false;
    if (pollAgain) scheduleRouterStatusPoll();
  }
}

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
    metricDesc.textContent = `版本: ${status.version || "未知"}`;

    settingsBadge.textContent = label;
    settingsBadge.className =
      status.state === "managed" ? "m3-chip m3-chip--success" : "m3-chip m3-chip--warning";
    settingsDetail.textContent = `${status.version} · 入口: ${cleanPath(status.entrypoint)}${
      status.active_pids && status.active_pids.length
        ? ` · 运行中 PID: ${status.active_pids.join(", ")}`
        : " · Desktop 当前未运行"
    }`;
  } catch {
    metricVal.textContent = "未检测到";
    metricDesc.textContent = "无独立运行时";
    settingsBadge.textContent = "未安装";
    settingsBadge.className = "m3-chip m3-chip--warning";
    settingsDetail.textContent = "系统未检测到官方 ChatGPT / Codex Desktop 应用。";
  }
}

let currentWebUrl = "";

async function refreshSecurityStatus() {
  const accessBadge = document.querySelector("#settings-access-state-badge");
  const webEnabledInput = document.querySelector("#settings-web-enabled");
  const allowRemoteInput = document.querySelector("#settings-allow-remote");
  const tip = document.querySelector("#settings-security-tip");
  const logoutBtn = document.querySelector("#logout-btn");
  const urlActions = document.querySelector("#settings-security-actions");

  try {
    const sec = await api("/api/v1/security/status");
    if (webEnabledInput) webEnabledInput.checked = !!sec.web_enabled;
    if (allowRemoteInput) allowRemoteInput.checked = !!sec.allow_remote;

    if (window.electronAPI && window.electronAPI.localToken) {
      logoutBtn.classList.add("is-hidden"); // 桌面应用内免密，不显示退出登录按钮
    } else {
      logoutBtn.classList.toggle("is-hidden", !sec.password_set);
    }

    if (!sec.web_enabled) {
      currentWebUrl = "";
      accessBadge.textContent = "仅应用内免密 · 网页访问已禁用";
      accessBadge.className = "m3-chip m3-chip--warning";
      tip.textContent =
        "当前网页端访问处于禁用状态。应用客户端内部可正常通信与配置；如需从浏览器访问，请设置密码并勾选启用。";
      if (urlActions) urlActions.classList.add("is-hidden");
    } else if (sec.allow_remote) {
      const port = sec.port || 31828;
      currentWebUrl = `http://${sec.bind_addr && sec.bind_addr !== "0.0.0.0" ? sec.bind_addr : "localhost"}:${port}`;
      accessBadge.textContent = "已设密码 · 允许外网/局域网网页访问";
      accessBadge.className = "m3-chip m3-chip--success";
      tip.textContent = `已开启外网访问密码保护。任何局域网设备均可访问：http://${
        sec.bind_addr === "0.0.0.0" ? "<本机IP>" : sec.bind_addr
      }:${port}。`;
      if (urlActions) urlActions.classList.remove("is-hidden");
    } else {
      const port = sec.port || 31828;
      currentWebUrl = `http://localhost:${port}`;
      accessBadge.textContent = "已设密码 · 仅本机浏览器访问";
      accessBadge.className = "m3-chip m3-chip--success";
      tip.textContent = `已启用本机网页访问。可通过浏览器访问：http://localhost:${port}，需输入密码。`;
      if (urlActions) urlActions.classList.remove("is-hidden");
    }
  } catch {
    currentWebUrl = "";
    accessBadge.textContent = "获取失败";
    accessBadge.className = "m3-chip m3-chip--error";
    if (urlActions) urlActions.classList.add("is-hidden");
  }
}

// ==========================================================================
// 7. 账号页面
// ==========================================================================

function renderAccountCard(acc) {
  const card = document.createElement("div");
  card.className = `m3-card m3-card--large m3-account-card${acc.is_active ? " active" : ""}`;

  const plan = String(acc.plan_type || "").trim().toLowerCase();
  const planLabel = knownPlanClasses.has(plan) ? plan.toUpperCase() : "方案未知";
  const planClass = sanitizePlanClass(plan);
  const usage = acc.usage;

  card.innerHTML = `
    <div class="m3-account-card-header">
      <div class="m3-account-title-group">
        <span class="m3-account-name">${escapeHtml(acc.name)}</span>
        <span class="m3-body-small m3-text-variant">${escapeHtml(acc.email || "未知邮箱")}</span>
      </div>
      <div class="m3-account-badges">
        <span class="m3-plan-chip ${planClass}">${escapeHtml(planLabel)}</span>
        ${acc.is_active ? '<span class="m3-chip m3-chip--success m3-chip--sm">当前生效</span>' : ""}
      </div>
    </div>

    <div class="m3-account-metrics-box">
      ${renderUsageMetric("5 小时窗口额度", usage ? usage.primary_5h : null)}
      ${renderUsageMetric("7 天周额度", usage ? usage.secondary_weekly : null)}
      ${renderReserveMetric(usage ? usage.reserve : null)}
    </div>

    <div class="m3-account-actions">
      ${
        !acc.is_active
          ? `<button class="m3-btn m3-btn-filled m3-btn-xs switch-acc-btn" data-id="${escapeAttr(acc.id)}">
               <span class="material-symbols-outlined m3-i-swap-horiz" aria-hidden="true"></span>
               <span>切换账号</span>
             </button>`
          : `<button class="m3-btn m3-btn-tonal m3-btn-xs" disabled>
               <span class="material-symbols-outlined m3-i-check" aria-hidden="true"></span>
               <span>正在使用中</span>
             </button>`
      }
      <button class="m3-btn m3-btn-tonal m3-btn-xs check-usage-acc-btn" data-id="${escapeAttr(acc.id)}" data-tooltip="刷新实时额度">
        <span class="material-symbols-outlined m3-i-autorenew" aria-hidden="true"></span>
      </button>
      <button class="m3-btn m3-btn-outlined m3-btn-xs rename-acc-btn" data-id="${escapeAttr(acc.id)}" data-tooltip="重命名备注">
        <span class="material-symbols-outlined m3-i-edit" aria-hidden="true"></span>
      </button>
      <button class="m3-btn m3-btn--danger-outlined m3-btn-xs delete-acc-btn m3-account-actions__spacer" data-id="${escapeAttr(acc.id)}" data-tooltip="移除账号">
        <span class="material-symbols-outlined m3-i-delete" aria-hidden="true"></span>
      </button>
    </div>
  `;

  applyTooltips(card);

  const switchBtn = card.querySelector(".switch-acc-btn");
  if (switchBtn) {
    switchBtn.onclick = () => withBusy(switchBtn, async () => {
      const choice = await m3Confirm(
        "切换官方账号",
        `切换到【${acc.name}】后，现有 Codex 进程是否立即重启？不重启时，新账号会在下次启动 Codex 后生效。`,
        {
          destructive: false,
          confirmLabel: "切换账号",
          checkbox: {
            label: "现在重启 Codex（推荐）",
            defaultChecked: true,
          },
        },
      );
      if (!choice.confirmed) return;
      try {
        notify(choice.checked
          ? "正在切换至账号【" + acc.name + "】并重启 Codex…"
          : "正在切换至账号【" + acc.name + "】…");
        const res = await api("/api/v1/accounts/switch", {
          method: "POST",
          body: JSON.stringify({ account_id: acc.id, restart_codex: choice.checked }),
        });
        notify(choice.checked
          ? "已切换至【" + res.account.name + "】并重启 Codex 运行时"
          : "已切换至【" + res.account.name + "】，下次启动 Codex 时生效");
        await refreshAccounts();
      } catch (e) {
        notify(e.message, true);
      }
    });
  }

  const checkUsageBtn = card.querySelector(".check-usage-acc-btn");
  checkUsageBtn.onclick = () => withBusy(checkUsageBtn, async () => {
    try {
      await api(`/api/v1/accounts/${acc.id}/usage`);
      notify(`已更新【${acc.name}】的额度。`);
      await refreshAccounts();
    } catch (e) {
      notify(e.message, true);
    }
  });

  const renameBtn = card.querySelector(".rename-acc-btn");
  renameBtn.onclick = async () => {
    const newName = await m3Prompt("重命名账号", "请输入新的账号备注名称：", acc.name);
    if (newName && newName.trim() && newName.trim() !== acc.name) {
      try {
        await api("/api/v1/accounts/rename", {
          method: "POST",
          body: JSON.stringify({ account_id: acc.id, name: newName.trim() }),
        });
        notify("账号重命名成功");
        await refreshAccounts();
      } catch (e) {
        notify(e.message, true);
      }
    }
  };

  const deleteBtn = card.querySelector(".delete-acc-btn");
  deleteBtn.onclick = async () => {
    const ok = await m3Confirm(
      "确认删除账号",
      `确定要从托管列表中移除账号【${acc.name}】吗？此操作不可撤销。`,
      { confirmLabel: "删除账号" },
    );
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

  return card;
}

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

    // 总览：当前生效账号
    if (active && active.is_logged_in && active.email) {
      metricVal.textContent = active.email;
      const activePlan = String(active.plan_type || "").trim().toLowerCase();
      metricPlan.textContent = "方案: " + (knownPlanClasses.has(activePlan) ? activePlan.toUpperCase() : "未知");
    } else {
      metricVal.textContent = "当前未登录";
      metricPlan.textContent = "未检测到有效会话";
    }

    // 总览：即时额度
    const activeAccObj = cachedAccounts.find((a) => a.is_active);
    if (activeAccObj && activeAccObj.usage) {
      overviewUsageContainer.innerHTML = `
        <div class="m3-usage-grid">
          ${renderUsageMetric("5 小时窗口额度", activeAccObj.usage.primary_5h, { variant: "wavy" })}
          ${renderUsageMetric("7 天周额度", activeAccObj.usage.secondary_weekly, { variant: "wavy" })}
          ${renderReserveMetric(activeAccObj.usage.reserve)}
        </div>
      `;
    } else if (active && active.is_logged_in) {
      overviewUsageContainer.innerHTML = `
        <p class="m3-body-medium m3-text-variant">
          当前正生效账号：<strong>${escapeHtml(active.email)}</strong>（${escapeHtml(knownPlanClasses.has(String(active.plan_type || "").trim().toLowerCase()) ? String(active.plan_type).toUpperCase() : "方案未知")}）。请保存当前账号后查看精准额度。
        </p>
      `;
    } else {
      overviewUsageContainer.innerHTML = `
        <p class="m3-body-medium m3-text-muted">当前系统未登录官方账号，无法获取额度。</p>
      `;
    }

    // 账号列表
    if (!cachedAccounts.length) {
      container.innerHTML = emptyState({
        icon: "account-circle",
        title: "暂无托管的官方账号",
        body: "点击上方“保存当前登录态为账号”，即可将当前会话纳入统一管理。",
        actionLabel: "保存当前登录态",
        actionId: "empty-capture-account-btn",
      });
      bindClick("#empty-capture-account-btn").onclick = handleCaptureAccount;
      return;
    }

    container.innerHTML = "";
    for (const acc of cachedAccounts) container.appendChild(renderAccountCard(acc));
  } catch (e) {
    container.innerHTML = errorState({
      title: "加载账号失败",
      body: e.message,
      retryId: "accounts-retry-btn",
    });
    bindClick("#accounts-retry-btn").onclick = () => refreshAccounts();
  }
}

// ==========================================================================
// 8. 服务商与模型页面
// ==========================================================================

async function finishCatalogMutation(message) {
  markCatalogPending();
  await refreshProviders();
  try {
    await applyCatalog({ silent: true });
    notify(message + "，已应用到 Codex");
  } catch {
    notify(message + "，但尚未应用到 Codex；可点击“应用到 Codex”重试。", true);
  }
}

function renderModelRow(model) {
  const row = document.createElement("div");
  row.className = "m3-model-row";
  row.dataset.logicalModelId = model.logical_model_id;
  row.dataset.modelSearch = [
    model.display_name,
    model.logical_model_id,
    model.upstream_model_id,
  ].filter(Boolean).join(" ").toLowerCase();
  row.dataset.modelEnabled = String(model.enabled !== false);

  const contextText = model.context_window ? `${model.context_window} tokens` : "默认上下文";
  const capabilities = model.capabilities || {};

  row.innerHTML = `
    <div class="m3-model-info">
      <div class="m3-model-title-row">
        <span class="m3-model-name">${escapeHtml(model.display_name || model.logical_model_id)}</span>
        <span class="m3-body-small m3-text-muted m3-mono">${escapeHtml(model.logical_model_id)}</span>
      </div>
      <div class="m3-model-tags">
        <span class="m3-chip m3-chip--neutral m3-chip--sm m3-numeric">${escapeHtml(contextText)}</span>
        ${capabilities.images ? '<span class="m3-chip m3-chip--tertiary m3-chip--sm">图片</span>' : ""}
        ${capabilities.tools ? '<span class="m3-chip m3-chip--assist m3-chip--sm">工具调用</span>' : ""}
      </div>
    </div>
    <div class="m3-model-actions">
      <label class="m3-switch" data-tooltip="${model.enabled ? "已启用，点击停用" : "已停用，点击启用"}">
        <input type="checkbox" class="toggle-model-switch" ${model.enabled ? "checked" : ""} aria-label="启用或停用模型 ${escapeAttr(model.logical_model_id)}" />
        <span class="m3-switch__track" aria-hidden="true"></span>
        <span class="m3-switch__handle" aria-hidden="true"><span class="material-symbols-outlined m3-i-check"></span></span>
      </label>
      <button class="m3-icon-btn m3-icon-btn--sm edit-model-btn" data-tooltip="编辑模型属性">
        <span class="material-symbols-outlined m3-i-edit" aria-hidden="true"></span>
      </button>
      <button class="m3-icon-btn m3-icon-btn--sm m3-icon-btn--danger delete-model-btn" data-tooltip="删除模型">
        <span class="material-symbols-outlined m3-i-delete" aria-hidden="true"></span>
      </button>
    </div>
  `;

  applyTooltips(row);

  const toggleSwitch = row.querySelector(".toggle-model-switch");
  toggleSwitch.onchange = async () => {
    try {
      await api("/api/v1/models/enabled", {
        method: "POST",
        body: JSON.stringify({ logical_model_id: model.logical_model_id, enabled: toggleSwitch.checked }),
      });
      await finishCatalogMutation("模型【" + model.logical_model_id + "】已" + (toggleSwitch.checked ? "启用" : "停用"));
    } catch (e) {
      notify(e.message, true);
      toggleSwitch.checked = !toggleSwitch.checked;
    }
  };

  const editBtn = row.querySelector(".edit-model-btn");
  editBtn.onclick = () => {
    const form = document.querySelector("#m3-edit-model-form");
    if (!form) return;
    form.reset();
    form.logical_model_id.value = model.logical_model_id;
    form.display_name.value = model.display_name || model.logical_model_id;
    form.context_window.value = model.context_window || "";
    form.clear_context_window.checked = false;
    form.context_window.disabled = false;
    openDialog("edit-model-dialog");
  };

  const deleteBtn = row.querySelector(".delete-model-btn");
  deleteBtn.onclick = async () => {
    const ok = await m3Confirm("确认删除模型", `确定要删除模型【${model.logical_model_id}】吗？`, {
      confirmLabel: "删除模型",
    });
    if (!ok) return;
    try {
      await api("/api/v1/models/remove", {
        method: "POST",
        body: JSON.stringify({ logical_model_id: model.logical_model_id }),
      });
      await finishCatalogMutation("模型已删除");
    } catch (e) {
      notify(e.message, true);
    }
  };

  return row;
}

function renderDiscoveredBox(provider) {
  const wrapper = document.createElement("div");
  wrapper.className = "m3-discover";
  wrapper.dataset.open = "false";
  wrapper.dataset.providerId = provider.id;

  wrapper.innerHTML = `
    <div class="m3-discover__inner">
      <div class="m3-discover__body">
        <div class="m3-discover__head">
          <div>
            <h4 class="m3-title-medium">发现模型并导入</h4>
            <p class="m3-body-small m3-text-variant">从 ${escapeHtml(provider.base_url)} 上游自动扫描可用模型</p>
          </div>
          <button class="m3-btn m3-btn-tonal m3-btn-xs fetch-discover-btn">
            <span class="material-symbols-outlined m3-i-travel-explore" aria-hidden="true"></span>
            <span>扫描模型</span>
          </button>
        </div>
        <div class="m3-discover__items discovered-items-container">
          <span class="m3-body-small m3-text-muted">点击“扫描模型”开始抓取上游清单…</span>
        </div>
        <div class="m3-discover__actions is-hidden">
          <button class="m3-btn m3-btn-filled m3-btn-xs import-selected-btn">
            <span class="material-symbols-outlined m3-i-add-task" aria-hidden="true"></span>
            <span>导入所选模型</span>
          </button>
        </div>
      </div>
    </div>
  `;

  const fetchBtn = wrapper.querySelector(".fetch-discover-btn");
  const listContainer = wrapper.querySelector(".discovered-items-container");
  const actionsBar = wrapper.querySelector(".m3-discover__actions");
  const importBtn = wrapper.querySelector(".import-selected-btn");
  let discoveredList = [];

  const scanModels = () => withBusy(fetchBtn, async () => {
    try {
      listContainer.innerHTML = `<span class="m3-body-small m3-text-primary">正在抓取上游模型清单…</span>`;
      discoveredList = await api(`/api/v1/providers/${encodeURIComponent(provider.id)}/discover`);
      listContainer.innerHTML = "";

      if (!discoveredList.length) {
        listContainer.innerHTML = `<span class="m3-body-small m3-text-muted">未发现可用模型</span>`;
        actionsBar.classList.add("is-hidden");
        return;
      }

      for (const m of discoveredList) {
        const item = document.createElement("label");
        item.className = "m3-list-item m3-list-item--interactive";
        item.innerHTML = `
          <span class="m3-list-item__content">
            <span class="m3-list-item__headline">${escapeHtml(m.display_name || m.upstream_model_id)}</span>
            <span class="m3-list-item__support m3-mono">${escapeHtml(m.upstream_model_id)}</span>
          </span>
          <input type="checkbox" class="m3-discover__checkbox" value="${escapeAttr(m.upstream_model_id)}" aria-label="选择模型 ${escapeAttr(m.upstream_model_id)}" />
        `;
        listContainer.appendChild(item);
      }
      actionsBar.classList.remove("is-hidden");
      notify(`成功扫描到 ${discoveredList.length} 个可用模型`);
    } catch (e) {
      listContainer.innerHTML = `<span class="m3-body-small m3-text-error">${escapeHtml(e.message)}</span>`;
      notify(e.message, true);
    }
  });
  fetchBtn.onclick = scanModels;
  wrapper.scan = scanModels;

  importBtn.onclick = () => withBusy(importBtn, async () => {
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
      await finishCatalogMutation("已成功导入 " + selected.length + " 个模型");
    } catch (e) {
      notify(e.message, true);
    }
  });

  return wrapper;
}

function renderProviderCard(provider) {
  const card = document.createElement("div");
  card.className = "m3-card m3-card--large m3-provider-card";
  card.dataset.providerId = provider.id;

  card.innerHTML = `
    <div class="m3-provider-head">
      <div class="m3-provider-identity">
        <div class="m3-provider-title-row">
          <h3 class="m3-provider-name">${escapeHtml(provider.name)}</h3>
          <span class="m3-chip m3-chip--neutral m3-chip--sm m3-mono">${escapeHtml(provider.id)}</span>
          <span class="m3-chip m3-chip--neutral m3-chip--sm">${escapeHtml(provider.protocol)}</span>
        </div>
        <span class="m3-body-small m3-text-muted m3-mono">${escapeHtml(provider.base_url)}</span>
      </div>
      <div class="m3-provider-actions">
        <button class="m3-btn m3-btn-tonal m3-btn-xs discover-toggle-btn" aria-expanded="false">
          <span class="material-symbols-outlined m3-i-travel-explore" aria-hidden="true"></span>
          <span>发现模型</span>
        </button>
        <button class="m3-btn m3-btn-tonal m3-btn-xs add-model-to-provider-btn">
          <span class="material-symbols-outlined m3-i-add" aria-hidden="true"></span>
          <span>添加模型</span>
        </button>
        <button class="m3-icon-btn m3-icon-btn--sm edit-provider-btn" data-tooltip="编辑供应商">
          <span class="material-symbols-outlined m3-i-edit" aria-hidden="true"></span>
        </button>
        <button class="m3-icon-btn m3-icon-btn--sm m3-icon-btn--danger delete-provider-btn" data-tooltip="删除供应商">
          <span class="material-symbols-outlined m3-i-delete" aria-hidden="true"></span>
        </button>
      </div>
    </div>

    <div class="m3-model-list"></div>
  `;

  const modelListContainer = card.querySelector(".m3-model-list");
  if (provider.models && provider.models.length) {
    for (const m of provider.models) modelListContainer.appendChild(renderModelRow(m));
  } else {
    modelListContainer.innerHTML =
      `<div class="m3-body-small m3-text-muted">暂无模型，可点击“发现模型”或“添加模型”。</div>`;
  }

  const discoveredBox = renderDiscoveredBox(provider);
  card.appendChild(discoveredBox);

  // Expand/collapse the discover panel with a grid-rows transition rather than
  // a display toggle, so the spring actually animates.
  const toggleDiscoverBtn = card.querySelector(".discover-toggle-btn");
  toggleDiscoverBtn.onclick = () => {
    const open = discoveredBox.dataset.open === "true";
    discoveredBox.dataset.open = String(!open);
    toggleDiscoverBtn.setAttribute("aria-expanded", String(!open));
  };

  const addModelBtn = card.querySelector(".add-model-to-provider-btn");
  addModelBtn.onclick = () => {
    document.querySelector("#m3-model-form")?.reset();
    populateProviderSelect(provider.id);
    setM3SelectValue(document.querySelector("#add-model-provider-id"), provider.id);
    openDialog("add-model-dialog");
  };

  // Edit: one prefilled form dialog, replacing the old pair of chained prompts.
  const editBtn = card.querySelector(".edit-provider-btn");
  editBtn.onclick = () => {
    pendingProviderEdit = provider.id;
    const dialog = document.querySelector("#add-provider-dialog");
    dialog.querySelector("#add-provider-title").textContent = "编辑服务商";
    dialog.querySelector("#add-provider-subtitle").textContent = "修改服务商名称、接入地址与高级连接方式";
    dialog.querySelector("#add-provider-submit").textContent = "保存修改";
    const form = dialog.querySelector("#m3-provider-form");
    form.name.value = provider.name;
    form.base_url.value = provider.base_url;
    setM3SelectValue(form.protocol, provider.protocol);
    setM3SelectValue(form.auth_strategy, authStrategyKind(provider.auth_strategy));
    form.auth_header.value = authStrategyHeader(provider.auth_strategy);
    syncProviderAuthField();
    form.api_key.value = "";
    // The API key is not returned by the list endpoint, so editing never
    // rewrites it; hide the field instead of inviting an accidental blank.
    form.api_key.closest(".m3-text-field").classList.add("is-hidden");
    openDialog("add-provider-dialog");
  };

  // Delete: one confirmation carrying the credential opt-in, replacing the old
  // two consecutive confirm dialogs.
  const deleteBtn = card.querySelector(".delete-provider-btn");
  deleteBtn.onclick = async () => {
    const res = await m3Confirm(
      "确认删除服务商",
      `即将删除【${provider.name}】及其关联的所有模型。此操作不可撤销。`,
      {
        confirmLabel: "删除服务商",
        checkbox: {
          label: "同时在系统安全钥匙箱中清除该 Provider 存储的 API Key",
          defaultChecked: true,
        },
      },
    );
    if (!res || !res.confirmed) return;
    try {
      await api("/api/v1/providers/remove", {
        method: "POST",
        body: JSON.stringify({ id: provider.id, purge_credential: res.checked }),
      });
      notify("服务商已移除，正在应用模型列表…");
      await refreshProviders();
      try {
        await applyCatalog({ silent: true });
        notify("服务商已移除，模型列表已应用到 Codex");
      } catch {
        markCatalogPending("服务商已移除，但模型列表尚未应用到 Codex。");
        notify("服务商已移除，但模型列表尚未应用到 Codex。", true);
      }
    } catch (e) {
      notify(e.message, true);
    }
  };

  applyTooltips(card);
  return card;
}

async function refreshProviders() {
  const container = document.querySelector("#providers-container");
  const metricProvVal = document.querySelector("#metric-providers-val");
  const metricModelVal = document.querySelector("#metric-models-val");

  try {
    const providers = await api("/api/v1/providers");
    cachedProviders = providers || [];

    populateProviderSelect();

    let totalModels = 0;
    for (const p of cachedProviders) totalModels += (p.models || []).length;
    metricProvVal.textContent = cachedProviders.length;
    metricModelVal.textContent = `共注册 ${totalModels} 个模型`;

    if (!cachedProviders.length) {
      container.innerHTML = emptyState({
        icon: "neurology",
        title: "未添加任何模型服务商",
        body: "点击“添加服务商”，填写地址和密钥；保存后即可发现可用模型。",
      });
      applyModelFilter();
      refreshSetupGuide();
      return;
    }

    container.innerHTML = "";
    for (const p of cachedProviders) container.appendChild(renderProviderCard(p));
    applyModelFilter();
    refreshSetupGuide();
  } catch (e) {
    container.innerHTML = errorState({
      title: "加载服务商失败",
      body: e.message,
      retryId: "providers-retry-btn",
    });
    bindClick("#providers-retry-btn").onclick = () => refreshProviders();
    applyModelFilter();
    refreshSetupGuide();
  }
}

function populateProviderSelect(selectedId = "") {
  const select = document.querySelector("#add-model-provider-id");
  if (!select) return;
  const current = selectedId || select.value;
  select.innerHTML = '<option value="">请选择服务商</option>';
  for (const provider of cachedProviders || []) {
    const option = document.createElement("option");
    option.value = provider.id;
    option.textContent = `${provider.name}（${provider.id}）`;
    select.appendChild(option);
  }
  const nextValue = current && [...select.options].some((option) => option.value === current)
    ? current
    : "";
  setM3SelectValue(select, nextValue);
  const state = [...m3SelectStates].find((item) => item.select === select);
  if (state) renderM3SelectOptions(state);
}

function applyModelFilter() {
  const query = modelFilterQuery.trim().toLowerCase();
  let total = 0;
  let visible = 0;
  let visibleEnabled = 0;
  let visibleDisabled = 0;
  document.querySelectorAll(".m3-provider-card").forEach((card) => {
    const rows = [...card.querySelectorAll(".m3-model-row")];
    let cardVisible = false;
    for (const row of rows) {
      total += 1;
      const matchesQuery = !query || row.dataset.modelSearch.includes(query);
      const matchesEnabled = !modelFilterEnabledOnly || row.dataset.modelEnabled === "true";
      const matches = matchesQuery && matchesEnabled;
      row.classList.toggle("is-filtered", !matches);
      if (matches) {
        cardVisible = true;
        visible += 1;
        if (row.dataset.modelEnabled === "true") visibleEnabled += 1;
        else visibleDisabled += 1;
      }
    }
    card.classList.toggle("is-filtered", rows.length > 0 && !cardVisible);
    if (!rows.length) card.classList.toggle("is-filtered", Boolean(query || modelFilterEnabledOnly));
  });

  const count = document.querySelector("#model-filter-count");
  if (count) {
    count.textContent = query || modelFilterEnabledOnly
      ? visible + "/" + total + " 个模型"
      : "共 " + total + " 个模型";
  }
  const enable = document.querySelector("#enable-visible-models-btn");
  const disable = document.querySelector("#disable-visible-models-btn");
  if (enable) enable.disabled = visibleDisabled === 0;
  if (disable) disable.disabled = visibleEnabled === 0;
}

// ==========================================================================
// 9. 全量数据刷新总线
// ==========================================================================

async function refreshAll() {
  // Every refresher renders its own inline error state and `api()` already opens
  // the login dialog on a 401, so nothing rejects out of here. allSettled keeps
  // one failing panel from blocking the others.
  await Promise.allSettled([
    refreshRouterStatus(),
    refreshDesktopStatus(),
    refreshSecurityStatus(),
    refreshAccounts(),
    refreshProviders(),
  ]);
}

// ==========================================================================
// 10. 登录 / 登出
// ==========================================================================

const loginForm = document.querySelector("#m3-login-form");
const loginError = document.querySelector("#login-dialog-error");
const loginPasswordField = document.querySelector("#login-password-field");
const logoutBtn = document.querySelector("#logout-btn");

function showLoginDialog() {
  logoutBtn.classList.add("is-hidden");
  openDialog("login-dialog");
}

function hideLoginDialog() {
  closeDialog("login-dialog");
  if (sessionToken) logoutBtn.classList.remove("is-hidden");
}

loginForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const pwd = document.querySelector("#m3-login-password").value;
  const submitBtn = document.querySelector("#login-submit-btn");
  loginError.textContent = "";
  loginPasswordField.classList.remove("m3-text-field--error");

  let loginSuccess = false;
  await withBusy(submitBtn, async () => {
    try {
      // Must go through resolveApiUrl(): under Electron the page is file://, so a
      // bare relative path resolves to file:///api/... and always fails.
      const res = await fetch(resolveApiUrl("/api/v1/security/login"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password: pwd }),
      });
      const data = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(data.message || data.error || "登录失败，密码错误");

      sessionToken = data.token;
      localStorage.setItem("codex_mp_token", sessionToken);
      document.querySelector("#m3-login-password").value = "";
      hideLoginDialog();
      notify("登录成功");
      loginSuccess = true;
    } catch (err) {
      loginError.textContent = err.message;
      loginPasswordField.classList.add("m3-text-field--error");
      document.querySelector("#m3-login-password").focus();
    }
  });
  if (loginSuccess) {
    await refreshAll();
  }
});

logoutBtn.onclick = async () => {
  const revoked = sessionToken;
  sessionToken = "";
  // Drop every token source, not just the password-derived session: the
  // URL-provided token is a privileged loopback credential and would otherwise
  // keep authenticating requests after the user logged out.
  queryToken = "";
  localStorage.removeItem("codex_mp_token");
  showLoginDialog();
  if (revoked && !(window.electronAPI && window.electronAPI.isElectron)) {
    // Ask the backend to drop the session server-side. Clearing only the local
    // copy would leave a still-valid token in the server's session set.
    try {
      await fetch(resolveApiUrl("/api/v1/security/logout"), {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${revoked}` },
      });
    } catch {
      // Best effort; the session also expires on its own.
    }
  }
  notify("已安全退出登录。");
};

// ==========================================================================
// 11. 事件绑定：顶栏与总览
// ==========================================================================

const syncBtn = bindClick("#topbar-sync-btn");
syncBtn.onclick = () => applyCatalog({ button: syncBtn });

const routerRestartBtn = document.querySelector("#router-restart-btn");
if (routerRestartBtn) {
  routerRestartBtn.onclick = () => restartRouter();
}

bindClick("#setup-add-provider-btn").onclick = openAddProviderDialog;
bindClick("#setup-open-models-btn").onclick = () => activateView("view-providers");
bindClick("#setup-apply-btn").onclick = (event) => applyCatalog({ button: event.currentTarget });
bindClick("#setup-save-account-btn").onclick = handleCaptureAccount;

const overviewRefreshBtn = bindClick("#overview-refresh-btn");
overviewRefreshBtn.onclick = () => withBusy(overviewRefreshBtn, async () => {
  notify("正在刷新数据…");
  await refreshAll();
  notify("数据已刷新");
});

const checkAccountBtn = bindClick("#overview-check-account-btn");
checkAccountBtn.onclick = () => withBusy(checkAccountBtn, async () => {
  try {
    const status = await api("/api/v1/accounts/active");
    if (status.is_logged_in && status.email) {
      notify(`当前生效: ${status.email} (${(status.plan_type || "plus").toUpperCase()})`);
    } else {
      notify("~/.codex/auth.json 未检测到有效登录", true);
    }
    await refreshAccounts();
  } catch (err) {
    notify(err.message, true);
  }
});

const restartCodexBtn = bindClick("#overview-restart-codex-btn");
restartCodexBtn.onclick = () => withBusy(restartCodexBtn, async () => {
  try {
    const res = await api("/api/v1/accounts/restart-codex", { method: "POST" });
    notify(`Codex 已重启（已清理 ${res.terminated_pids.length} 个残留进程）`);
    await refreshAccounts();
  } catch (err) {
    notify(err.message, true);
  }
});

async function handleCaptureAccount() {
  const name = await m3Prompt("保存当前官方账号", "给当前登录的官方账号起个备注名称（可留空，系统会自动生成）：", "");
  if (name === null) return;
  try {
    const res = await api("/api/v1/accounts/capture", {
      method: "POST",
      body: JSON.stringify({ name: name.trim() || null }),
    });
    notify(`已收纳账号【${res.name}】`);
    await refreshAccounts();
  } catch (e) {
    notify(e.message, true);
  }
}

bindClick("#capture-account-action-btn").onclick = handleCaptureAccount;
bindClick("#overview-capture-account-btn").onclick = handleCaptureAccount;

const refreshAllUsageBtn = bindClick("#refresh-all-accounts-usage-btn");
refreshAllUsageBtn.onclick = () => withBusy(refreshAllUsageBtn, async () => {
  try {
    notify("正在批量查询各账号实时额度…");
    const accounts = await api("/api/v1/accounts");
    const results = await Promise.allSettled(
      accounts.map((acc) => api(`/api/v1/accounts/${acc.id}/usage`)),
    );
    const failed = results.filter((result) => result.status === "rejected");
    if (failed.length) {
      notify((accounts.length - failed.length) + "/" + accounts.length + " 个账号额度已更新，" + failed.length + " 个失败，请稍后重试。", true);
    } else {
      notify("所有托管账号额度已更新");
    }
    await refreshAccounts();
  } catch (err) {
    notify(err.message, true);
  }
});

// ==========================================================================
// 12. 事件绑定：对话框表单
// ==========================================================================

// Holds the provider id being edited, or null when the dialog is in add mode.
let pendingProviderEdit = null;

function authStrategyKind(strategy) {
  if (typeof strategy === "string") return strategy;
  if (strategy && typeof strategy === "object" && strategy.header) return "header";
  return "bearer";
}

function authStrategyHeader(strategy) {
  if (strategy && typeof strategy === "object" && strategy.header) {
    return strategy.header.name || "";
  }
  return "";
}

function readProviderAuthStrategy(formData) {
  const kind = String(formData.get("auth_strategy") || "bearer");
  if (kind === "header") {
    const name = String(formData.get("auth_header") || "").trim();
    if (!name) throw new Error("自定义认证方式需要填写请求头名称");
    return { header: { name } };
  }
  return kind;
}

function syncProviderAuthField() {
  const form = document.querySelector("#m3-provider-form");
  const strategy = form?.querySelector("[name='auth_strategy']");
  const field = document.querySelector("#provider-auth-header-field");
  if (!strategy || !field) return;
  field.classList.toggle("is-hidden", strategy.value !== "header");
  const input = field.querySelector("input");
  if (input) input.required = strategy.value === "header";
  const apiKey = form.querySelector("[name='api_key']");
  if (apiKey) apiKey.required = strategy.value !== "none" && !pendingProviderEdit;
}

function resetProviderDialog() {
  const dialog = document.querySelector("#add-provider-dialog");
  dialog.querySelector("#add-provider-title").textContent = "添加服务商";
  dialog.querySelector("#add-provider-subtitle").textContent = "填入服务商地址和密钥，下一步自动发现模型";
  dialog.querySelector("#add-provider-submit").textContent = "保存并发现模型";
  const form = dialog.querySelector("#m3-provider-form");
  form.reset();
  form.api_key.closest(".m3-text-field").classList.remove("is-hidden");
  syncProviderAuthField();
  pendingProviderEdit = null;
}

function openAddProviderDialog() {
  resetProviderDialog();
  openDialog("add-provider-dialog");
}

bindClick("#open-add-provider-dialog-btn").onclick = openAddProviderDialog;
bindClick("#overview-open-add-provider-btn").onclick = openAddProviderDialog;

bindClick("#close-add-provider-btn").onclick = () => {
  closeDialog("add-provider-dialog");
  pendingProviderEdit = null;
};

const addProviderForm = document.querySelector("#m3-provider-form");
addProviderForm.querySelector("[name='auth_strategy']")?.addEventListener("change", syncProviderAuthField);
addProviderForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = new FormData(addProviderForm);
  const submitBtn = document.querySelector("#add-provider-submit");
  await withBusy(submitBtn, async () => {
    try {
      const authStrategy = readProviderAuthStrategy(form);
      if (pendingProviderEdit) {
        const provider = await api("/api/v1/providers/edit", {
          method: "POST",
          body: JSON.stringify({
            id: pendingProviderEdit,
            name: String(form.get("name")).trim(),
            base_url: String(form.get("base_url")).trim(),
            protocol: String(form.get("protocol")) || null,
            enabled: null,
            api_key: null,
            auth_strategy: authStrategy,
          }),
        });
        notify("服务商已更新，正在应用模型列表…");
        closeDialog("add-provider-dialog");
        resetProviderDialog();
        await refreshProviders();
        try {
          await applyCatalog({ silent: true });
          notify("服务商已更新，模型列表已应用到 Codex");
        } catch {
          markCatalogPending("服务商已更新，但模型列表尚未应用到 Codex。");
          notify("服务商已更新，但模型列表尚未应用到 Codex。", true);
        }
        return provider;
      } else {
        const provider = await api("/api/v1/providers/add", {
          method: "POST",
          body: JSON.stringify({
            name: form.get("name"),
            base_url: form.get("base_url"),
            protocol: form.get("protocol"),
            api_key: form.get("api_key") || null,
            auth_strategy: authStrategy,
          }),
        });
        notify("服务商已保存，正在发现模型…");
        closeDialog("add-provider-dialog");
        resetProviderDialog();
        await refreshProviders();
        const card = [...document.querySelectorAll("[data-provider-id]")].find((item) => item.dataset.providerId === provider.id);
        const discover = card?.querySelector(".discover-toggle-btn");
        if (discover && discover.getAttribute("aria-expanded") !== "true") discover.click();
        const discoverBox = card?.querySelector(".m3-discover");
        if (discoverBox?.scan) await discoverBox.scan();
        return provider;
      }
    } catch (err) {
      notify(err.message, true);
    }
  });
});

bindClick("#open-add-model-dialog-btn").onclick = () => {
  populateProviderSelect();
  document.querySelector("#m3-model-form").reset();
  openDialog("add-model-dialog");
};

bindClick("#close-add-model-btn").onclick = () => closeDialog("add-model-dialog");

const addModelForm = document.querySelector("#m3-model-form");
addModelForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = new FormData(addModelForm);
  const submitBtn = addModelForm.querySelector("button[type='submit']");
  await withBusy(submitBtn, async () => {
    try {
      const providerId = String(form.get("provider_id") || "");
      const upstreamModelId = String(form.get("upstream_model_id") || "").trim();
      const provider = cachedProviders.find((item) => item.id === providerId);
      if (!provider) throw new Error("请先选择模型服务商");
      if (!upstreamModelId) throw new Error("请填写上游模型名称");
      const context = form.get("context_window");
      const displayName = String(form.get("display_name") || "").trim()
        || provider.name + " / " + upstreamModelId;
      await api("/api/v1/models/add", {
        method: "POST",
        body: JSON.stringify({
          provider_id: providerId,
          upstream_model_id: upstreamModelId,
          display_name: displayName,
          context_window: context ? Number(context) : null,
          images: form.get("images") === "on",
          tools: form.get("tools") === "on",
        }),
      });
      addModelForm.reset();
      closeDialog("add-model-dialog");
      await finishCatalogMutation("模型已添加");
    } catch (err) {
      notify(err.message, true);
    }
  });
});

const editModelForm = document.querySelector("#m3-edit-model-form");
if (editModelForm) {
  const contextInput = editModelForm.querySelector("[name='context_window']");
  const clearContext = editModelForm.querySelector("[name='clear_context_window']");
  clearContext?.addEventListener("change", () => {
    if (contextInput) contextInput.disabled = clearContext.checked;
  });
  editModelForm.addEventListener("submit", async (e) => {
    e.preventDefault();
    const form = new FormData(editModelForm);
    const submitBtn = document.querySelector("#edit-model-submit");
    await withBusy(submitBtn, async () => {
      try {
        const rawContext = String(form.get("context_window") || "").trim();
        const parsedContext = rawContext ? Number(rawContext) : null;
        if (rawContext && (!Number.isSafeInteger(parsedContext) || parsedContext < 1)) {
          throw new Error("上下文长度必须是正整数");
        }
        await api("/api/v1/models/edit", {
          method: "POST",
          body: JSON.stringify({
            logical_model_id: form.get("logical_model_id"),
            display_name: String(form.get("display_name") || "").trim(),
            context_window: clearContext?.checked ? null : parsedContext,
            clear_context_window: Boolean(clearContext?.checked),
          }),
        });
        closeDialog("edit-model-dialog");
        await finishCatalogMutation("模型已更新");
      } catch (err) {
        notify(err.message, true);
      }
    });
  });
}

bindClick("#close-edit-model-btn").onclick = () => closeDialog("edit-model-dialog");

const importForm = document.querySelector("#m3-import-account-form");
const importError = document.querySelector("#import-dialog-error");
const importJsonField = document.querySelector("#import-json-field");
const importJsonInput = importForm.querySelector("textarea[name='json_content']");
const importJsonCounter = document.querySelector("#import-json-counter");

importJsonInput.addEventListener("input", () => {
  importJsonCounter.textContent = String(importJsonInput.value.length);
});

bindClick("#open-import-modal-action-btn").onclick = () => {
  importError.textContent = "";
  importJsonField.classList.remove("m3-text-field--error");
  importJsonCounter.textContent = String(importJsonInput.value.length);
  openDialog("import-account-dialog");
};

bindClick("#close-import-account-dialog-btn").onclick = () => closeDialog("import-account-dialog");

importForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = new FormData(importForm);
  const name = form.get("name");
  const jsonContent = form.get("json_content");
  importError.textContent = "";
  importJsonField.classList.remove("m3-text-field--error");

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

    closeDialog("import-account-dialog");
    importForm.reset();
    importJsonCounter.textContent = "0";
    notify(`成功导入账号【${res.name}】`);
    await refreshAccounts();
  } catch (err) {
    importError.textContent = err.message;
    importJsonField.classList.add("m3-text-field--error");
    importJsonInput.focus();
  }
});

// ==========================================================================
// 13. 事件绑定：设置页
// ==========================================================================

const providersRefreshBtn = bindClick("#providers-refresh-btn");
providersRefreshBtn.onclick = () => withBusy(providersRefreshBtn, async () => {
  notify("正在刷新服务商…");
  await refreshProviders();
  notify("服务商列表已刷新");
});

const modelFilterInput = document.querySelector("#model-filter-input");
const modelFilterEnabledOnlyInput = document.querySelector("#model-filter-enabled-only");
modelFilterInput?.addEventListener("input", () => {
  modelFilterQuery = modelFilterInput.value;
  applyModelFilter();
});
modelFilterEnabledOnlyInput?.addEventListener("change", () => {
  modelFilterEnabledOnly = modelFilterEnabledOnlyInput.checked;
  applyModelFilter();
});

async function bulkSetVisibleModels(enabled, button) {
  const rows = [...document.querySelectorAll("#providers-container .m3-model-row:not(.is-filtered)")];
  const ids = rows
    .filter((row) => (row.dataset.modelEnabled === "true") !== enabled)
    .map((row) => row.dataset.logicalModelId)
    .filter(Boolean);
  if (!ids.length) {
    notify(enabled ? "没有需要启用的匹配模型" : "没有需要停用的匹配模型");
    return;
  }
  if (!enabled) {
    const ok = await m3Confirm(
      "停用匹配模型",
      `即将停用 ${ids.length} 个匹配模型；它们会从 Codex 的可用列表中隐藏。`,
      { confirmLabel: "停用模型" },
    );
    if (!ok) return;
  }
  await withBusy(button, async () => {
    const results = await Promise.allSettled(ids.map((logical_model_id) => api("/api/v1/models/enabled", {
      method: "POST",
      body: JSON.stringify({ logical_model_id, enabled }),
    })));
    const failed = results.filter((result) => result.status === "rejected");
    if (failed.length) {
      notify((ids.length - failed.length) + "/" + ids.length + " 个模型已更新，" + failed.length + " 个失败。", true);
    }
    await finishCatalogMutation(enabled ? "匹配模型已启用" : "匹配模型已停用");
  });
}

bindClick("#enable-visible-models-btn").onclick = (event) =>
  bulkSetVisibleModels(true, event.currentTarget);
bindClick("#disable-visible-models-btn").onclick = (event) =>
  bulkSetVisibleModels(false, event.currentTarget);

// Live feedback for the password field.
{
  const pwdInput = document.querySelector("#settings-web-password");
  const pwdCounter = document.querySelector("#settings-password-counter");
  const pwdSupport = document.querySelector("#settings-password-support");
  if (pwdInput && pwdCounter && pwdSupport) {
    pwdInput.addEventListener("input", () => {
      const len = pwdInput.value.length;
      pwdCounter.textContent = len ? `${len} 字符` : "";
      if (!len) pwdSupport.textContent = "留空则保持现状；输入新密码即进行修改或设置";
      else if (len < 8) pwdSupport.textContent = "密码过短：建议至少 8 个字符";
      else pwdSupport.textContent = "提交后将更新网页端访问密码";
    });
  }

  const openBrowserBtn = document.querySelector("#security-open-browser-btn");
  if (openBrowserBtn) {
    openBrowserBtn.onclick = () => {
      if (!currentWebUrl) return;
      window.open(currentWebUrl, "_blank");
    };
  }

  const copyUrlBtn = document.querySelector("#security-copy-url-btn");
  if (copyUrlBtn) {
    copyUrlBtn.onclick = async () => {
      if (!currentWebUrl) return;
      try {
        await navigator.clipboard.writeText(currentWebUrl);
        notify("已复制控制地址到剪贴板");
      } catch {
        // Fallback for clipboard
        const input = document.createElement("input");
        input.value = currentWebUrl;
        document.body.appendChild(input);
        input.select();
        document.execCommand("copy");
        document.body.removeChild(input);
        notify("已复制控制地址到剪贴板");
      }
    };
  }
}

const copyUninstallCommandBtn = bindClick("#copy-uninstall-command-btn");
copyUninstallCommandBtn.onclick = async () => {
  const command = document.querySelector("#uninstall-command")?.textContent?.trim();
  if (!command) return;
  try {
    await navigator.clipboard.writeText(command);
  } catch {
    const input = document.createElement("input");
    input.value = command;
    document.body.appendChild(input);
    input.select();
    document.execCommand("copy");
    input.remove();
  }
  notify("已复制卸载命令，请在终端执行");
};

bindClick("#settings-security-form").onsubmit = async (e) => {
  e.preventDefault();
  const password = document.querySelector("#settings-web-password").value;
  const web_enabled = document.querySelector("#settings-web-enabled").checked;
  const allow_remote = document.querySelector("#settings-allow-remote").checked;
  const submitBtn = document.querySelector("#settings-security-form button[type='submit']");

  await withBusy(submitBtn, async () => {
    try {
      const res = await api("/api/v1/security/update", {
        method: "POST",
        body: JSON.stringify({
          password: password.trim() ? password.trim() : null,
          web_enabled,
          allow_remote,
        }),
      });
      document.querySelector("#settings-web-password").value = "";
      document.querySelector("#settings-password-counter").textContent = "";
      if (res.token) {
        sessionToken = res.token;
        localStorage.setItem("codex_mp_token", sessionToken);
      }
      notify("访问安全配置已更新");
      await refreshSecurityStatus();
    } catch (err) {
      notify(err.message, true);
    }
  });
};

const desktopInstallBtn = bindClick("#settings-desktop-install-btn");
desktopInstallBtn.onclick = () => withBusy(desktopInstallBtn, async () => {
  try {
    await api("/api/v1/desktop/install", { method: "POST" });
    notify("实验性 Desktop 适配已安装，请完全退出并重新启动 ChatGPT Desktop。");
    await refreshDesktopStatus();
  } catch (err) {
    notify(err.message, true);
  }
});

const desktopRestoreBtn = bindClick("#settings-desktop-restore-btn");
desktopRestoreBtn.onclick = async () => {
  const ok = await m3Confirm(
    "恢复官方入口确认",
    "恢复官方 Desktop 入口前，请先完全退出 ChatGPT Desktop。是否继续？",
    { destructive: false, confirmLabel: "继续恢复" },
  );
  if (!ok) return;
  await withBusy(desktopRestoreBtn, async () => {
    try {
      await api("/api/v1/desktop/restore", { method: "POST" });
      notify("Desktop 已恢复为官方纯净 runtime。");
      await refreshDesktopStatus();
    } catch (err) {
      notify(err.message, true);
    }
  });
};

// ==========================================================================
// 14. 桌面端窗口控制与启动自检
// ==========================================================================

if (window.electronAPI && window.electronAPI.isElectron) {
  const controls = document.querySelector("#desktop-window-controls");
  if (controls) controls.classList.remove("is-hidden");

  const minBtn = document.querySelector("#win-min-btn");
  if (minBtn) minBtn.onclick = () => window.electronAPI.minimizeWindow();

  const maxBtn = document.querySelector("#win-max-btn");
  if (maxBtn) maxBtn.onclick = () => window.electronAPI.maximizeWindow();

  const closeBtn = document.querySelector("#win-close-btn");
  if (closeBtn) closeBtn.onclick = () => window.electronAPI.closeWindow();

  // 后端进程状态由主进程推送，避免主进程往渲染层注入脚本来报告错误。
  if (typeof window.electronAPI.onBackendEvent === "function") {
    window.electronAPI.onBackendEvent((event) => {
      if (!event) return;
      if (event.kind === "sync-finished") {
        notify("全量配置已成功同步到 Codex");
        refreshAll();
      } else if (event.message) {
        notify(event.message, true);
      }
    });
  }
}

// 启动自检：localToken 由 preload 通过 sendSync 同步注入，页面脚本运行到这里时
// 已经可用，因此不需要轮询等待。
initM3Selects();
applyTooltips();
renderCatalogState();
populateProviderSelect();
refreshAll();
