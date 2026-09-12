const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
const providersElement = document.querySelector("#providers");
const messageElement = document.querySelector("#message");
const routerStateElement = document.querySelector("#router-state");
let discoveredByProvider = new Map();

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

function message(text, error = false) {
  messageElement.textContent = text;
  messageElement.style.color = error ? "#ff9f9f" : "#8ce2bf";
}

async function refreshRouter() {
  try {
    const status = await invoke("router_status");
    routerStateElement.textContent = status.healthy ? "Router 正常" : status.running ? "Router 启动中" : "Router 未运行";
    routerStateElement.style.color = status.healthy ? "#8ce2bf" : "#ffc777";
  } catch (error) {
    routerStateElement.textContent = "Router 状态未知";
    message(String(error), true);
  }
}

function modelRow(provider, model) {
  const row = document.createElement("div");
  row.className = "model";
  const context = model.context_window ? `上下文 ${model.context_window}` : "默认上下文";
  row.innerHTML = `<div><strong>${escapeHtml(model.display_name || model.logical_model_id)}</strong><span>${escapeHtml(model.logical_model_id)} · ${escapeHtml(context)}</span></div>`;
  const actions = document.createElement("div");
  actions.className = "model-actions";
  const toggle = document.createElement("button");
  toggle.className = "secondary";
  toggle.textContent = model.enabled ? "停用" : "启用";
  toggle.onclick = async () => {
    try {
      await invoke("set_model_enabled", { logicalModelId: model.logical_model_id, enabled: !model.enabled });
      await refresh();
    } catch (error) { message(String(error), true); }
  };
  const edit = document.createElement("button");
  edit.className = "secondary";
  edit.textContent = "编辑";
  edit.onclick = async () => {
    const displayName = window.prompt("新的显示名称", model.display_name || "");
    if (displayName === null) return;
    const contextValue = window.prompt("上下文窗口（留空不修改，输入 0 清除）", model.context_window || "");
    const args = { logicalModelId: model.logical_model_id, displayName, clearContextWindow: false, contextWindow: null };
    if (contextValue !== null && contextValue.trim() === "0") {
      args.clearContextWindow = true;
    } else if (contextValue !== null && contextValue.trim() !== "") {
      const parsedContext = Number(contextValue);
      if (!Number.isSafeInteger(parsedContext) || parsedContext < 1) {
        return message("上下文窗口必须是正整数、0（清除）或留空（不修改）。", true);
      }
      args.contextWindow = parsedContext;
    }
    try {
      await invoke("edit_model", args);
      await refresh();
    } catch (error) { message(String(error), true); }
  };
  const remove = document.createElement("button");
  remove.className = "secondary";
  remove.textContent = "删除";
  remove.onclick = async () => {
    if (!window.confirm(`删除模型 ${model.logical_model_id}？`)) return;
    try {
      await invoke("remove_model", { logicalModelId: model.logical_model_id });
      await refresh();
      message("模型已删除，请同步到 Codex。 ");
    } catch (error) { message(String(error), true); }
  };
  actions.append(toggle, edit, remove);
  row.append(actions);
  return row;
}

function discoveredRow(provider) {
  const wrap = document.createElement("div");
  wrap.className = "provider";
  wrap.innerHTML = `<div class="provider-head"><div><h3>发现模型</h3><div class="meta">勾选后导入 ${escapeHtml(provider.id)}</div></div><button class="secondary">刷新列表</button></div>`;
  const list = document.createElement("div");
  list.className = "model-list";
  const importButton = document.createElement("button");
  importButton.textContent = "导入选中模型";
  importButton.style.marginTop = "12px";
  const refreshButton = wrap.querySelector("button");
  const load = async () => {
    try {
      const discovered = await invoke("discover_models", { providerId: provider.id });
      discoveredByProvider.set(provider.id, discovered);
      list.replaceChildren();
      for (const model of discovered) {
        const label = document.createElement("label");
        label.className = "model";
        label.innerHTML = `<span><strong>${escapeHtml(model.display_name || model.upstream_model_id)}</strong><span>${escapeHtml(model.upstream_model_id)}</span></span><input type="checkbox" value="${escapeHtml(model.upstream_model_id)}" />`;
        list.append(label);
      }
      message(`已发现 ${discovered.length} 个模型，请选择需要导入的模型。`);
    } catch (error) { message(String(error), true); }
  };
  refreshButton.onclick = load;
  importButton.onclick = async () => {
    const discovered = discoveredByProvider.get(provider.id) || [];
    const selectedIds = [...list.querySelectorAll("input:checked")].map((input) => input.value);
    if (!selectedIds.length) return message("请先选择模型。", true);
    try {
      await invoke("import_models", { providerId: provider.id, discovered, selectedIds });
      await refresh();
      message(`已导入 ${selectedIds.length} 个模型。`);
    } catch (error) { message(String(error), true); }
  };
  wrap.append(list, importButton);
  return wrap;
}

function providerCard(provider) {
  const card = document.createElement("article");
  card.className = "provider";
  card.innerHTML = `<div class="provider-head"><div><h3>${escapeHtml(provider.name)} <span class="meta">(${escapeHtml(provider.id)})</span></h3><div class="meta">${escapeHtml(provider.base_url)} · ${escapeHtml(provider.protocol)} · ${provider.enabled ? "启用" : "停用"}</div></div><div class="provider-actions"><button class="secondary" data-action="discover">发现模型</button><button class="secondary" data-action="edit">编辑</button><button class="secondary" data-action="remove">删除</button></div></div>`;
  const models = document.createElement("div");
  models.className = "model-list";
  for (const model of provider.models) models.append(modelRow(provider, model));
  const discovered = discoveredRow(provider);
  discovered.style.display = "none";
  card.querySelector('[data-action="discover"]').onclick = () => { discovered.style.display = discovered.style.display === "none" ? "block" : "none"; };
  card.querySelector('[data-action="edit"]').onclick = async () => {
    const name = window.prompt("Provider 名称", provider.name);
    if (name === null) return;
    const baseUrl = window.prompt("Base URL", provider.base_url);
    if (baseUrl === null) return;
    try {
      await invoke("edit_provider", { id: provider.id, name: name.trim(), baseUrl: baseUrl.trim(), protocol: null, enabled: null, apiKey: null });
      await refresh();
      message("Provider 已更新。 ");
    } catch (error) { message(String(error), true); }
  };
  card.querySelector('[data-action="remove"]').onclick = async () => {
    if (!window.confirm(`删除 Provider ${provider.id}？`)) return;
    const purgeCredential = window.confirm("同时删除该 Provider 的系统 Keyring API Key？");
    try {
      await invoke("remove_provider", { id: provider.id, purgeCredential });
      await refresh();
      message("Provider 已删除，请同步到 Codex。 ");
    } catch (error) { message(String(error), true); }
  };
  card.append(models, discovered);
  return card;
}

async function refresh() {
  try {
    const providers = await invoke("list_providers");
    providersElement.replaceChildren(...providers.map(providerCard));
    await refreshRouter();
  } catch (error) { message(String(error), true); }
}

document.querySelector("#provider-form").onsubmit = async (event) => {
  event.preventDefault();
  const form = new FormData(event.currentTarget);
  try {
    await invoke("add_provider", {
      name: form.get("name"),
      baseUrl: form.get("base_url"),
      protocol: form.get("protocol"),
      apiKey: form.get("api_key") || null,
    });
    event.currentTarget.reset();
    await refresh();
    message("Provider 已保存到系统 Keyring/Registry，请同步到 Codex。 ");
  } catch (error) { message(String(error), true); }
};

document.querySelector("#model-form").onsubmit = async (event) => {
  event.preventDefault();
  const form = new FormData(event.currentTarget);
  const context = form.get("context_window");
  try {
    await invoke("add_model", {
      providerId: form.get("provider_id"),
      upstreamModelId: form.get("upstream_model_id"),
      displayName: form.get("display_name"),
      contextWindow: context ? Number(context) : null,
      images: form.get("images") === "on",
      tools: form.get("tools") === "on",
    });
    event.currentTarget.reset();
    await refresh();
    message("模型已添加，请同步到 Codex。 ");
  } catch (error) { message(String(error), true); }
};

document.querySelector("#refresh").onclick = refresh;
document.querySelector("#sync-catalog").onclick = async () => {
  try {
    const catalogPath = await invoke("sync_catalog");
    message(`Codex Catalog 已同步：${catalogPath}`);
  } catch (error) { message(String(error), true); }
};
refresh();
