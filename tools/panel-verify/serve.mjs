// Mock backend + static server for the Codex OmniBridge panel.
//
// Serves apps/panel over HTTP with the same CSP the Rust backend sends, and
// answers every /api/v1 route the panel calls with deterministic fixture data.
// Purpose: render every populated UI state (accounts, providers, models, quota)
// in a real browser so the panel can be verified without the Rust binary, a
// real Codex login, or network access.
//
// Run: node tools/panel-verify/serve.mjs [panelDir] [port]
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT =
  process.argv[2] ||
  path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../apps/panel");
const PORT = Number(process.argv[3] || 4599);

// Kept textually identical to the header in crates/web/src/lib.rs and the meta
// tag in apps/panel/index.html.
const CSP =
  "default-src 'none'; script-src 'self'; " +
  "style-src 'self' 'unsafe-inline'; " +
  "font-src 'self'; img-src 'self' data:; " +
  "connect-src 'self' http://127.0.0.1:* http://localhost:*; " +
  "form-action 'none'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'";

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".woff2": "font/woff2",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

// ---------------------------------------------------------------- fixtures --
// Deliberately includes the awkward cases: every plan type (plus/pro/team/free
// and an unrecognised one), a quota at 100%, a limit-reached reserve, an
// account with no usage data, and a provider with no models.
const accounts = [
  {
    id: "acc_plus_main",
    name: "Plus 主号",
    email: "main.plus@example.com",
    plan_type: "plus",
    is_active: true,
    usage: {
      primary_5h: { used_percent: 42, reset_after_seconds: 3600 * 3 + 1200 },
      secondary_weekly: { used_percent: 71, reset_after_seconds: 86400 * 4 + 3600 },
      reserve: { used_percent: 12, reset_after_seconds: 86400 * 11 },
    },
  },
  {
    id: "acc_pro_work",
    name: "Pro 工作号",
    email: "work.pro@example.com",
    plan_type: "pro",
    is_active: false,
    usage: {
      primary_5h: { used_percent: 93, reset_after_seconds: 900 },
      secondary_weekly: { used_percent: 58, reset_after_seconds: 86400 * 2 },
      reserve: { limit_reached: true, reset_after_seconds: 86400 * 6 },
    },
  },
  {
    id: "acc_team_shared",
    name: "Team 共享号",
    email: "shared.team@example.com",
    plan_type: "team",
    is_active: false,
    usage: {
      primary_5h: { used_percent: 8, reset_after_seconds: 3600 * 4 },
      secondary_weekly: { used_percent: 25, reset_after_seconds: 86400 * 5 },
      reserve: null,
    },
  },
  {
    id: "acc_free_trial",
    name: "Free 试用",
    email: "trial.free@example.com",
    plan_type: "free",
    is_active: false,
    usage: null,
  },
  {
    id: "acc_unknown_plan",
    name: "未识别方案号",
    email: "unknown@example.com",
    // Exercises the plan-class allow-list fallback.
    plan_type: "enterprise-x",
    is_active: false,
    usage: {
      primary_5h: { used_percent: 100, reset_after_seconds: 60 },
      secondary_weekly: null,
      reserve: null,
    },
  },
];

const providers = [
  {
    id: "newapi-primary",
    name: "NewAPI 主线路",
    base_url: "https://api.newapi.example.com/v1",
    protocol: "responses",
    models: [
      {
        logical_model_id: "gpt-5.2-codex",
        display_name: "NewAPI / GPT-5.2 Codex",
        upstream_model_id: "gpt-5.2-codex",
        context_window: 400000,
        enabled: true,
        capabilities: { images: true, tools: true },
      },
      {
        logical_model_id: "gemini-3.8-flash",
        display_name: "NewAPI / Gemini 3.8 Flash",
        upstream_model_id: "gemini-3.8-flash",
        context_window: 1048576,
        enabled: true,
        capabilities: { images: true, tools: false },
      },
      {
        logical_model_id: "deepseek-v4.1",
        display_name: "NewAPI / DeepSeek V4.1",
        upstream_model_id: "deepseek-v4.1",
        context_window: null,
        enabled: false,
        capabilities: { images: false, tools: true },
      },
    ],
  },
  {
    id: "openrouter-backup",
    name: "OpenRouter 备用线路",
    base_url: "https://openrouter.ai/api/v1",
    protocol: "chat_completions",
    models: [
      {
        logical_model_id: "claude-opus-4.6",
        display_name: "OpenRouter / Claude Opus 4.6",
        upstream_model_id: "anthropic/claude-opus-4.6",
        context_window: 200000,
        enabled: true,
        capabilities: { images: true, tools: true },
      },
    ],
  },
  {
    id: "oneapi-empty",
    name: "OneAPI 未配置",
    base_url: "https://oneapi.internal.example.com/v1",
    protocol: "chat_completions",
    models: [],
  },
];

const discovered = [
  { upstream_model_id: "gpt-5.2", display_name: "GPT-5.2", context_window: 400000 },
  { upstream_model_id: "gpt-5.2-mini", display_name: "GPT-5.2 mini", context_window: 200000 },
  { upstream_model_id: "o5-preview", display_name: "o5 Preview", context_window: 200000 },
  { upstream_model_id: "qwen4-max", display_name: "Qwen4 Max", context_window: 131072 },
  { upstream_model_id: "kimi-k3", display_name: "Kimi K3", context_window: 262144 },
];

// ------------------------------------------------------------- api handler --
function readRoute(route) {
  if (route === "/api/v1/router/status") {
    return { healthy: true, running: true, port: 31828 };
  }
  if (route === "/api/v1/desktop/status") {
    return {
      state: "managed",
      version: "1.4.2",
      entrypoint: "/home/user/.local/bin/codex-standalone",
      active_pids: [4821, 4822],
    };
  }
  if (route === "/api/v1/security/status") {
    return {
      web_enabled: true,
      password_set: true,
      allow_remote: false,
      bind_addr: "127.0.0.1",
      port: 31828,
    };
  }
  if (route === "/api/v1/accounts") return accounts;
  if (route === "/api/v1/accounts/active") {
    return { is_logged_in: true, email: "main.plus@example.com", plan_type: "plus" };
  }
  if (route === "/api/v1/providers") return providers;
  if (route === "/api/v1/catalog/sync") return { catalog_path: "/home/user/.codex/models.json" };
  return null;
}

// Mutating routes the panel calls. Each returns a plausible success payload.
const MUTATIONS = new Set([
  "/api/v1/router/restart",
  "/api/v1/security/login",
  "/api/v1/security/logout",
  "/api/v1/security/update",
  "/api/v1/accounts/restart-codex",
  "/api/v1/accounts/capture",
  "/api/v1/accounts/import",
  "/api/v1/accounts/switch",
  "/api/v1/accounts/rename",
  "/api/v1/accounts/delete",
  "/api/v1/providers/add",
  "/api/v1/providers/edit",
  "/api/v1/providers/remove",
  "/api/v1/models/add",
  "/api/v1/models/edit",
  "/api/v1/models/enabled",
  "/api/v1/models/import",
  "/api/v1/models/remove",
  "/api/v1/desktop/install",
  "/api/v1/desktop/restore",
]);

function mockResponse(route, payload = {}) {
  if (route === "/api/v1/router/restart") return { healthy: true, running: true, port: 31828 };
  if (route === "/api/v1/security/login") return { token: "mock-session-token" };
  if (route === "/api/v1/security/update") return { token: "mock-session-token" };
  if (route === "/api/v1/accounts/restart-codex") return { terminated_pids: [11, 22, 33] };
  if (route === "/api/v1/accounts/capture") return { name: "新收纳账号" };
  if (route === "/api/v1/accounts/import") return { name: "导入的账号" };
  if (route === "/api/v1/accounts/switch") return { account: { name: "已切换账号" } };
  if (route === "/api/v1/providers/add") {
    const id = String(payload.name || "provider")
      .trim()
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "provider";
    const provider = {
      id,
      name: String(payload.name || id),
      base_url: String(payload.base_url || ""),
      protocol: payload.protocol || "responses",
      auth_strategy: payload.auth_strategy || "bearer",
      models: [],
    };
    providers.push(provider);
    return provider;
  }
  if (route === "/api/v1/providers/edit") {
    const provider = providers.find((item) => item.id === payload.id);
    if (provider) {
      if (payload.name) provider.name = payload.name;
      if (payload.base_url) provider.base_url = payload.base_url;
      if (payload.protocol) provider.protocol = payload.protocol;
      if (payload.auth_strategy) provider.auth_strategy = payload.auth_strategy;
      return provider;
    }
  }
  if (route === "/api/v1/providers/remove") {
    const index = providers.findIndex((item) => item.id === payload.id);
    if (index >= 0) providers.splice(index, 1);
    return { ok: true };
  }
  if (route === "/api/v1/models/add") {
    const provider = providers.find((item) => item.id === payload.provider_id);
    const model = {
      logical_model_id: payload.provider_id + "/" + payload.upstream_model_id,
      upstream_model_id: payload.upstream_model_id,
      display_name: payload.display_name || (provider?.name || payload.provider_id) + " / " + payload.upstream_model_id,
      context_window: payload.context_window || null,
      enabled: true,
      capabilities: { images: !!payload.images, tools: !!payload.tools },
    };
    provider?.models.push(model);
    return model;
  }
  if (route === "/api/v1/models/import") {
    const provider = providers.find((item) => item.id === payload.provider_id);
    const selected = new Set(payload.selected_ids || []);
    for (const item of discovered) {
      if (!selected.has(item.upstream_model_id)) continue;
      provider?.models.push({
        logical_model_id: payload.provider_id + "/" + item.upstream_model_id,
        upstream_model_id: item.upstream_model_id,
        display_name: item.display_name,
        context_window: item.context_window || null,
        enabled: true,
        capabilities: { images: false, tools: false },
      });
    }
    return provider?.models || [];
  }
  if (route === "/api/v1/models/enabled") {
    for (const provider of providers) {
      const model = provider.models.find((item) => item.logical_model_id === payload.logical_model_id);
      if (model) model.enabled = !!payload.enabled;
    }
  }
  if (route === "/api/v1/models/edit") {
    for (const provider of providers) {
      const model = provider.models.find((item) => item.logical_model_id === payload.logical_model_id);
      if (model) {
        if (payload.display_name) model.display_name = payload.display_name;
        if (payload.clear_context_window) model.context_window = null;
        else if (payload.context_window) model.context_window = payload.context_window;
        return model;
      }
    }
  }
  if (route === "/api/v1/models/remove") {
    for (const provider of providers) {
      provider.models = provider.models.filter((item) => item.logical_model_id !== payload.logical_model_id);
    }
  }
  return { ok: true };
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const route = url.pathname;

  if (route.startsWith("/api/")) {
    // Drain the body so the connection can be reused.
    const chunks = [];
    req.on("data", (chunk) => chunks.push(chunk));
    req.on("end", () => {
      let payload = {};
      try {
        payload = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
      } catch {
        payload = {};
      }
      const headers = {
        "Content-Type": "application/json",
        "X-Content-Type-Options": "nosniff",
      };
      const send = (code, obj) => {
        res.writeHead(code, headers);
        res.end(JSON.stringify(obj));
      };

      // Per-account usage and per-provider discovery are parameterised.
      if (route.endsWith("/usage")) return send(200, { ok: true });
      if (route.endsWith("/discover")) return send(200, discovered);

      // Exact read routes must resolve before any generic prefix match,
      // otherwise /accounts/active is swallowed by a mutation pattern.
      const read = readRoute(route);
      if (read !== null) return send(200, read);

      if (MUTATIONS.has(route)) return send(200, mockResponse(route, payload));

      return send(500, { error: `mock: unhandled ${route}` });
    });
    return;
  }

  // Static files.
  const rel = path.normalize(route === "/" ? "/index.html" : route).replace(/^(\.\.[/\\])+/, "");
  const file = path.join(ROOT, rel);
  fs.readFile(file, (err, data) => {
    if (err) {
      // Mirror the Rust backend: a client-side route falls back to index.html,
      // but a mistyped asset is a real 404.
      if (path.extname(rel)) {
        res.writeHead(404, { "Content-Type": "text/plain" });
        return res.end("404");
      }
      fs.readFile(path.join(ROOT, "index.html"), (e2, html) => {
        if (e2) {
          res.writeHead(404, { "Content-Type": "text/plain" });
          return res.end("404");
        }
        res.writeHead(200, { "Content-Type": MIME[".html"], "Content-Security-Policy": CSP });
        res.end(html);
      });
      return;
    }
    res.writeHead(200, {
      "Content-Type": MIME[path.extname(file)] || "application/octet-stream",
      "Content-Security-Policy": CSP,
    });
    res.end(data);
  });
});

server.listen(PORT, "127.0.0.1", () =>
  console.log(`panel mock server on http://127.0.0.1:${PORT} serving ${ROOT}`),
);
