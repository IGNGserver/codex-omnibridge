const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, shell, nativeTheme } = require("electron");
const path = require("path");
const { fileURLToPath } = require("url");
const { spawn } = require("child_process");
const crypto = require("crypto");
const fs = require("fs");
const http = require("http");

let mainWindow = null;
let tray = null;
let rustProcess = null;
let isQuitting = false;
let restartAttempts = 0;
let quitCleanupStarted = false;
let backendStartTimer = null;
let backendWatchdogTimer = null;
let backendWatchdogInFlight = false;
let backendConsecutiveHealthFailures = 0;
let backendGeneration = 0;
let backendReady = false;
let rendererRecoveryTimer = null;
let rendererRecoveryAttempts = 0;
let rendererRecoveryBackoffAttempts = 0;
let traySyncProcess = null;
const pendingBackendEvents = [];

// 动态生成本次运行专属的本地特权 Token（免密通道）
const localToken = crypto.randomUUID();
const defaultPort = 31828;

// The panel talks to the loopback backend on this origin. 127.0.0.1 is used
// rather than "localhost" so the request is unambiguously a loopback caller,
// which is what the backend requires before honouring the desktop token.
const apiBase = `http://127.0.0.1:${defaultPort}`;

// 保存 localToken 到用户数据目录供 CLI/扩展安全免密调用
try {
  const tokenFile = path.join(app.getPath("userData"), "local_token");
  fs.writeFileSync(tokenFile, localToken, { mode: 0o600 });
} catch (e) {
  // 忽略
}

const PANEL_DIR = path.resolve(__dirname, "../panel");
const MAX_RESTART_DELAY_MS = 30000;
const BACKEND_READY_TIMEOUT_MS = 8000;
const BACKEND_HEALTH_INTERVAL_MS = 5000;
const BACKEND_HEALTH_FAILURE_LIMIT = 3;
const MAX_RENDERER_RECOVERY_ATTEMPTS = 3;
const RENDERER_RECOVERY_BASE_DELAY_MS = 500;
const MAX_RENDERER_RECOVERY_DELAY_MS = 30000;

function stopBackendWatchdog() {
  if (backendWatchdogTimer) {
    clearTimeout(backendWatchdogTimer);
    backendWatchdogTimer = null;
  }
  backendWatchdogInFlight = false;
  backendConsecutiveHealthFailures = 0;
}

function scheduleBackendRestart(generation) {
  if (isQuitting || generation !== backendGeneration || backendStartTimer) return;
  const delay = Math.min(
    1000 * 2 ** Math.min(restartAttempts, 5),
    MAX_RESTART_DELAY_MS,
  );
  restartAttempts += 1;
  console.warn(`[Rust Core] 异常退出，${delay}ms 后重试（第 ${restartAttempts} 次）...`);
  backendStartTimer = setTimeout(() => {
    backendStartTimer = null;
    startRustBackend();
  }, delay);
}

function markBackendReady(generation) {
  if (generation !== backendGeneration || isQuitting) return;
  backendReady = true;
  restartAttempts = 0;
  backendConsecutiveHealthFailures = 0;
  startBackendWatchdog(generation);
}

function startBackendWatchdog(generation) {
  stopBackendWatchdog();
  const tick = async () => {
    if (isQuitting || generation !== backendGeneration || !rustProcess) return;
    if (backendWatchdogInFlight) {
      backendWatchdogTimer = setTimeout(tick, BACKEND_HEALTH_INTERVAL_MS);
      return;
    }
    backendWatchdogInFlight = true;
    const response = await requestBackend("/healthz", { timeoutMs: 1200 });
    backendWatchdogInFlight = false;
    if (response.statusCode >= 200 && response.statusCode < 300) {
      backendConsecutiveHealthFailures = 0;
    } else {
      backendConsecutiveHealthFailures += 1;
      console.warn(
        `[Rust Core] 健康检查失败 ${backendConsecutiveHealthFailures}/${BACKEND_HEALTH_FAILURE_LIMIT}`,
      );
      if (backendConsecutiveHealthFailures >= BACKEND_HEALTH_FAILURE_LIMIT) {
        backendConsecutiveHealthFailures = 0;
        const child = rustProcess;
        if (child && !child.killed) {
          console.error("[Rust Core] 后台服务失去响应，正在重启 Rust 进程。");
          try {
            child.kill();
          } catch {
            // close/error handlers perform the retry if the process is already gone.
          }
        }
      }
    }
    if (!isQuitting && generation === backendGeneration) {
      backendWatchdogTimer = setTimeout(tick, BACKEND_HEALTH_INTERVAL_MS);
    }
  };
  backendWatchdogTimer = setTimeout(tick, BACKEND_HEALTH_INTERVAL_MS);
}

// 寻找 codex-mp 二进制路径
function getBinaryPath() {
  const binaryName = process.platform === "win32" ? "codex-mp.exe" : "codex-mp";

  // 1. 优先使用环境变量指定
  if (process.env.CODEX_MP_BIN && fs.existsSync(process.env.CODEX_MP_BIN)) {
    return process.env.CODEX_MP_BIN;
  }

  // 2. 打包安装环境：各种可能的存放路径
  const exeDir = path.dirname(app.getPath("exe"));
  const candidates = [
    path.join(process.resourcesPath || "", "bin", binaryName),
    path.join(process.resourcesPath || "", binaryName),
    path.join(exeDir, "resources", "bin", binaryName),
    path.join(exeDir, "resources", binaryName),
    path.join(exeDir, "bin", binaryName),
    path.join(exeDir, binaryName),
  ];

  for (const c of candidates) {
    if (c && fs.existsSync(c)) {
      console.log(`[Electron Main] 找到打包二进制: ${c}`);
      return c;
    }
  }

  // 3. 开发环境 target/release 或 target/debug
  const projectRoot = path.resolve(__dirname, "../..");
  const devCandidates = [
    path.join(projectRoot, "target", "release", binaryName),
    path.join(projectRoot, "target", "debug", binaryName),
  ];
  for (const d of devCandidates) {
    if (fs.existsSync(d)) {
      console.log(`[Electron Main] 找到开发环境二进制: ${d}`);
      return d;
    }
  }

  // 4. PATH 中的全局命令
  console.log(`[Electron Main] 使用 PATH 二进制: ${binaryName}`);
  return binaryName;
}

function reportToRenderer(kind, payload) {
  if (!mainWindow || mainWindow.isDestroyed() || mainWindow.webContents.isLoading()) {
    if (pendingBackendEvents.length >= 50) pendingBackendEvents.shift();
    pendingBackendEvents.push({ kind, ...payload });
    return;
  }
  // Structured payload instead of string-interpolated script: an error message
  // containing quotes or backticks must not be able to alter the executed code.
  try {
    mainWindow.webContents.send("backend-event", { kind, ...payload });
  } catch {
    if (pendingBackendEvents.length >= 50) pendingBackendEvents.shift();
    pendingBackendEvents.push({ kind, ...payload });
  }
}

function flushPendingBackendEvents() {
  if (!mainWindow || mainWindow.isDestroyed() || mainWindow.webContents.isLoading()) return;
  while (pendingBackendEvents.length) {
    try {
      mainWindow.webContents.send("backend-event", pendingBackendEvents.shift());
    } catch {
      break;
    }
  }
}

function requestBackend(pathname, { method = "GET", headers = {}, body = null, timeoutMs = 1000 } = {}) {
  return new Promise((resolve) => {
    const deadlineMs = Number.isFinite(timeoutMs) && timeoutMs > 0
      ? Math.max(1, Math.floor(timeoutMs))
      : 1000;
    let request = null;
    let deadlineTimer = null;
    let settled = false;
    let responseStarted = false;

    // Every caller only needs a small status result. Resolve exactly once so
    // timeout, response errors, and normal completion cannot race each other
    // or leave the watchdog's in-flight flag stuck forever.
    const finish = (statusCode) => {
      if (settled) return;
      settled = true;
      if (deadlineTimer) {
        clearTimeout(deadlineTimer);
        deadlineTimer = null;
      }
      resolve({ statusCode });
    };

    const abort = () => {
      try {
        request?.destroy();
      } catch {
        // The request may already have been closed.
      }
      finish(0);
    };

    try {
      request = http.request(
        {
          hostname: "127.0.0.1",
          port: defaultPort,
          path: pathname,
          method,
          headers: {
            ...(body ? { "Content-Length": Buffer.byteLength(body) } : {}),
            ...headers,
          },
        },
        (response) => {
          responseStarted = true;
          response.resume();
          response.on("end", () => finish(response.statusCode || 0));
          response.on("error", () => finish(0));
          response.on("aborted", () => finish(0));
          response.on("close", () => {
            if (!settled) finish(0);
          });
        },
      );
      // This protects against a peer that continuously sends a small amount
      // of data: request.setTimeout() alone is only an idle timeout.
      request.setTimeout(deadlineMs, abort);
      request.on("error", () => finish(0));
      request.on("close", () => {
        if (!settled && !responseStarted) finish(0);
      });
      deadlineTimer = setTimeout(abort, deadlineMs);
      if (body) request.write(body);
      request.end();
    } catch {
      abort();
    }
  });
}

async function waitForBackendReady() {
  const deadline = Date.now() + BACKEND_READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    const response = await requestBackend("/healthz", { timeoutMs: 700 });
    if (response.statusCode >= 200 && response.statusCode < 300) return true;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return false;
}

async function requestRouterStop() {
  await requestBackend("/api/v1/router/stop", {
    method: "POST",
    timeoutMs: 1200,
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${localToken}`,
      "X-Local-Token": localToken,
    },
    body: "{}",
  });
}

// 启动 Rust 后台 Web & 路由服务
function startRustBackend() {
  if (isQuitting || (rustProcess && !rustProcess.killed)) return;
  if (backendStartTimer) {
    clearTimeout(backendStartTimer);
    backendStartTimer = null;
  }
  const generation = ++backendGeneration;
  backendReady = false;
  stopBackendWatchdog();
  const binPath = getBinaryPath();
  console.log(`[Electron Main] 启动 Rust 后台守护进程: ${binPath}`);

  // 启动参数：--headless 避免 Rust 再弹出基于 DBus/Win32 的原生托盘，统一由 Electron 托盘接管。
  // localToken 只通过环境变量传递，不再放进 argv：命令行参数在 Linux 上对所有本地用户可见。
  const args = ["web", "start", "--headless", `--port=${defaultPort}`];

  try {
    const child = spawn(binPath, args, {
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      detached: false,
      env: {
        ...process.env,
        CODEX_MP_LOCAL_TOKEN: localToken,
        // Windows 凭据管理器有 2560 字符硬限制，而 OAuth JWT token 集通常远超该限制。
        // 在 Windows 平台桌面运行时默认选用 0600 安全文件后端存储凭据，防止账号保存与加载报错。
        ...(process.platform === "win32" && !process.env.CODEX_MP_SECRET_BACKEND
          ? { CODEX_MP_SECRET_BACKEND: "file" }
          : {}),
      },
    });
    rustProcess = child;
    startBackendWatchdog(generation);

    child.stdout.on("data", (data) => {
      // Keep logs bounded per event. The backend remains responsible for its
      // own log rotation; Electron must not retain a giant diagnostic chunk.
      const text = data.toString().slice(0, 16 * 1024);
      console.log(`[Rust Core stdout] ${text.trim()}`);
    });

    child.stderr.on("data", (data) => {
      console.error(`[Rust Core stderr] ${data.toString().slice(0, 16 * 1024).trim()}`);
    });

    child.on("error", (err) => {
      console.error(`[Rust Core] 启动失败:`, err);
      reportToRenderer("error", { message: `启动后台 Rust 失败: ${err.message}` });
    });

    child.on("close", (code) => {
      if (generation !== backendGeneration) return;
      rustProcess = null;
      backendReady = false;
      stopBackendWatchdog();
      console.log(`[Rust Core] 进程已退出，退出码: ${code}`);
      if (isQuitting) {
        return;
      }
      reportToRenderer("error", {
        message: "后台服务已退出，正在自动尝试恢复。",
      });
      scheduleBackendRestart(generation);
    });
  } catch (err) {
    console.error(`[Rust Core] 创建子进程异常:`, err);
    reportToRenderer("error", { message: `创建后台 Rust 进程失败: ${err.message}` });
    scheduleBackendRestart(generation);
  }
}

// 外部链接只允许交给系统浏览器打开 http/https；其余 scheme（file:/smb:/自定义协议）
// 一律拒绝，避免被注入的脚本借用系统 shell 打开任意目标。
function openExternalSafely(rawUrl) {
  let parsed;
  try {
    parsed = new URL(rawUrl);
  } catch {
    console.warn(`[Electron Main] 忽略无法解析的外部链接: ${rawUrl}`);
    return;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
    console.warn(`[Electron Main] 已阻止非 http(s) 外部链接: ${parsed.protocol}`);
    return;
  }
  shell.openExternal(parsed.toString());
}

// 主框架导航守卫：只允许停留在打包面板目录或本地后端 origin 之内。
function isNavigationAllowed(targetUrl) {
  let parsed;
  try {
    parsed = new URL(targetUrl);
  } catch {
    return false;
  }
  if (parsed.protocol === "file:") {
    let targetPath;
    try {
      targetPath = path.resolve(fileURLToPath(parsed));
    } catch {
      return false;
    }
    return targetPath === PANEL_DIR || targetPath.startsWith(PANEL_DIR + path.sep);
  }
  return parsed.origin === apiBase;
}

function guardNavigation(event, targetUrl) {
  if (isNavigationAllowed(targetUrl)) {
    return;
  }
  event.preventDefault();
  console.warn(`[Electron Main] 已阻止主框架导航: ${targetUrl}`);
  openExternalSafely(targetUrl);
}

function recoverRenderer(reason) {
  if (isQuitting || rendererRecoveryTimer) return;
  const delay = Math.min(
    RENDERER_RECOVERY_BASE_DELAY_MS * 2 ** Math.min(rendererRecoveryBackoffAttempts, 6),
    MAX_RENDERER_RECOVERY_DELAY_MS,
  );
  rendererRecoveryBackoffAttempts += 1;
  console.error(`[Electron] 渲染器异常（${reason}），${delay}ms 后准备恢复页面。`);
  reportToRenderer("error", { message: "控制中心页面异常，正在自动恢复。" });
  rendererRecoveryTimer = setTimeout(() => {
    rendererRecoveryTimer = null;
    if (!mainWindow || mainWindow.isDestroyed() || isQuitting) return;
    rendererRecoveryAttempts += 1;
    try {
      if (rendererRecoveryAttempts <= MAX_RENDERER_RECOVERY_ATTEMPTS) {
        mainWindow.webContents.reloadIgnoringCache();
        return;
      }
      // A repeatedly broken renderer may be tied to a corrupted WebContents;
      // replace the window so BrowserWindow can recreate its renderer process.
      rendererRecoveryAttempts = 0;
      const oldWindow = mainWindow;
      mainWindow = null;
      oldWindow.destroy();
      createWindow();
    } catch (error) {
      console.error("[Electron] 渲染器恢复失败:", error);
      recoverRenderer("recovery failed");
    }
  }, delay);
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1220,
    height: 840,
    // The panel switches navigation shape at 600/840/1200px. A 920px floor
    // made the compact and medium layouts unreachable in the desktop app.
    minWidth: 360,
    minHeight: 480,
    title: "Codex OmniBridge",
    icon: getAppIcon(),
    show: false,
    frame: false, // 去除系统原生外边框和原生标题栏
    autoHideMenuBar: true, // 彻底隐藏并去除 File, Edit, View 等菜单栏
    // Matches --md-sys-color-surface for the active colour scheme, so launching
    // the window does not flash the opposite theme before the page paints.
    backgroundColor: nativeTheme.shouldUseDarkColors ? "#0b1516" : "#f0fbfd",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: true,
      // webSecurity 保持默认开启：关闭同源策略会让任何被注入的脚本获得
      // 跨源读写能力，而面板本身只访问本地后端，不需要该豁免。
      webSecurity: true,
      webviewTag: false,
      allowRunningInsecureContent: false,
      spellcheck: false,
    },
  });

  // 彻底移除默认的 application menu
  Menu.setApplicationMenu(null);

  // 阻断权限请求（摄像头、麦克风、通知等），面板不需要其中任何一项。
  mainWindow.webContents.session.setPermissionRequestHandler((_wc, _permission, callback) => {
    callback(false);
  });

  const localFile = path.resolve(PANEL_DIR, "index.html");
  mainWindow.webContents.on("did-finish-load", () => {
    rendererRecoveryAttempts = 0;
    rendererRecoveryBackoffAttempts = 0;
    flushPendingBackendEvents();
  });
  mainWindow.webContents.on("render-process-gone", (_event, details) => {
    recoverRenderer(`render-process-gone:${details?.reason || "unknown"}`);
  });
  mainWindow.webContents.on("child-process-gone", (_event, details) => {
    const type = details?.type || "child";
    recoverRenderer(`child-process-gone:${type}:${details?.reason || "unknown"}`);
  });
  mainWindow.webContents.on("crashed", () => recoverRenderer("crashed"));
  mainWindow.webContents.on("unresponsive", () => {
    reportToRenderer("error", { message: "控制中心页面暂时无响应，正在等待恢复。" });
  });
  mainWindow.webContents.on("responsive", () => {
    console.log("[Electron] 控制中心页面已恢复响应。");
  });
  mainWindow.webContents.on("did-fail-load", (_event, errorCode, errorDescription, _validatedURL, isMainFrame) => {
    if (isMainFrame && errorCode !== -3) {
      recoverRenderer(`did-fail-load:${errorCode}:${errorDescription}`);
    }
  });

  // 优先直接加载本地打包的控制中心页面（永不黑屏，即开即显）。
  // localToken 通过 preload 的同步 IPC 注入，不再出现在 URL 里。
  if (fs.existsSync(localFile)) {
    mainWindow.loadFile(localFile);
  } else {
    mainWindow.loadURL(apiBase);
  }

  // 主框架导航守卫（target="_self" 链接、服务端 302 都会走到这里）
  mainWindow.webContents.on("will-navigate", guardNavigation);
  mainWindow.webContents.on("will-redirect", guardNavigation);

  // 禁止在窗口内创建新的 webview / 子窗口
  mainWindow.webContents.on("will-attach-webview", (event) => {
    event.preventDefault();
  });

  mainWindow.once("ready-to-show", () => {
    mainWindow.show();
    mainWindow.focus();
  });

  // 兜底显示：防止极端情况下 ready-to-show 没触发导致一直黑屏
  setTimeout(() => {
    if (mainWindow && !mainWindow.isDestroyed() && !mainWindow.isVisible()) {
      mainWindow.show();
    }
  }, 1200);

  // 关键机制：点击关闭（X）不退出，而是隐藏至系统托盘
  mainWindow.on("close", (event) => {
    if (!isQuitting) {
      event.preventDefault();
      mainWindow.hide();
      return false;
    }
  });

  // 外部链接一律交给系统浏览器，并经过 scheme 白名单校验
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    openExternalSafely(url);
    return { action: "deny" };
  });
}

function getAppIcon() {
  const iconPathPng = path.resolve(__dirname, "../../assets/icon.png");
  if (fs.existsSync(iconPathPng)) {
    return nativeImage.createFromPath(iconPathPng);
  }
  return null;
}

// 创建系统托盘
function createTray() {
  const icon = getAppIcon();
  if (!icon) return;

  tray = new Tray(icon);
  tray.setToolTip("Codex OmniBridge - 模型切换与桥接管理器");

  const contextMenu = Menu.buildFromTemplate([
    {
      label: "打开控制中心",
      click: () => {
        if (mainWindow) {
          mainWindow.show();
          mainWindow.focus();
        }
      },
    },
    {
      label: "将模型同步到 Codex",
      click: () => {
        if (traySyncProcess) {
          reportToRenderer("error", { message: "模型同步已经在进行中，请稍候。" });
          return;
        }
        const binPath = getBinaryPath();
        const p = spawn(binPath, ["sync"], {
          windowsHide: true,
          stdio: ["ignore", "ignore", "pipe"],
        });
        traySyncProcess = p;
        let stderr = "";
        let finished = false;
        const syncTimeout = setTimeout(() => {
          if (finished) return;
          finished = true;
          try {
            p.kill();
          } catch {
            // The close event still reports the failure if the process already exited.
          }
          traySyncProcess = null;
          reportToRenderer("error", { message: "同步超时，已终止卡住的同步进程。" });
        }, 120000);
        p.stderr.on("data", (data) => {
          if (stderr.length < 8192) stderr += data.toString().slice(0, 8192 - stderr.length);
        });
        p.on("error", (err) => {
          if (finished) return;
          finished = true;
          clearTimeout(syncTimeout);
          traySyncProcess = null;
          console.error("Tray sync failed to spawn:", err);
          reportToRenderer("error", { message: `同步失败: ${err.message}` });
        });
        p.on("close", (code) => {
          if (finished) return;
          finished = true;
          clearTimeout(syncTimeout);
          traySyncProcess = null;
          if (code === 0) {
            reportToRenderer("sync-finished", { success: true });
          } else {
            const detail = stderr.trim().slice(0, 512);
            reportToRenderer("error", {
              message: `同步失败，退出码 ${code}${detail ? `：${detail}` : ""}`,
            });
          }
        });
      },
    },
    { type: "separator" },
    {
      label: "彻底退出程序",
      click: () => {
        isQuitting = true;
        app.quit();
      },
    },
  ]);

  tray.setContextMenu(contextMenu);

  // 单击/双击托盘图标唤出主窗口
  tray.on("click", () => {
    if (mainWindow) {
      if (mainWindow.isVisible()) {
        mainWindow.focus();
      } else {
        mainWindow.show();
      }
    }
  });
  tray.on("double-click", () => {
    if (mainWindow) {
      mainWindow.show();
      mainWindow.focus();
    }
  });
}

// 只有面板自身的文档可以调用特权 IPC。缺少该检查时，任何一个能刷新主框架的
// 页面（注入脚本、被改写的链接）都能直接取走 localToken。
function isTrustedSender(event) {
  const frameUrl = event.senderFrame ? event.senderFrame.url : "";
  return isNavigationAllowed(frameUrl);
}

ipcMain.on("get-bootstrap", (event) => {
  if (!isTrustedSender(event)) {
    console.warn("[Electron Main] 拒绝来自不受信任来源的 bootstrap 请求");
    event.returnValue = { localToken: "", apiBase, appVersion: "" };
    return;
  }
  event.returnValue = { localToken, apiBase, appVersion: app.getVersion() };
});

ipcMain.on("window-minimize", (event) => {
  if (isTrustedSender(event) && mainWindow) mainWindow.minimize();
});

ipcMain.on("window-maximize", (event) => {
  if (isTrustedSender(event) && mainWindow) {
    if (mainWindow.isMaximized()) {
      mainWindow.unmaximize();
    } else {
      mainWindow.maximize();
    }
  }
});

ipcMain.on("window-close", (event) => {
  if (isTrustedSender(event) && mainWindow) mainWindow.hide();
});

// Chromium can lose its GPU process or report system memory pressure without
// taking the Electron main process down. Treat those signals as recoverable and
// ask the renderer to recreate itself; never let a stale WebContents leave the
// desktop app showing a permanent blank window.
app.on("child-process-gone", (_event, details) => {
  if (details?.type === "GPU" || details?.type === "Renderer") {
    recoverRenderer(`app-child-process-gone:${details.type}:${details.reason || "unknown"}`);
  }
});
app.on("gpu-process-crashed", () => recoverRenderer("gpu-process-crashed"));
app.on("memory-pressure", (_event, level) => {
  console.warn(`[Electron] 系统内存压力: ${level}`);
  reportToRenderer("error", { message: "系统内存紧张，控制中心正在释放页面资源。" });
  if (level === "critical" && mainWindow && !mainWindow.isDestroyed()) {
    void mainWindow.webContents.session.clearCache().catch(() => {});
  }
});

// 单实例锁：防止多开冲突
const gotTheLock = app.requestSingleInstanceLock();
if (!gotTheLock) {
  console.log("另一个实例已在运行，退出当前进程。");
  app.quit();
} else {
  app.on("second-instance", () => {
    if (mainWindow) {
      if (mainWindow.isMinimized()) mainWindow.restore();
      mainWindow.show();
      mainWindow.focus();
    }
  });

  app.whenReady().then(async () => {
    startRustBackend();
    const backendReady = await waitForBackendReady();
    if (backendReady) markBackendReady(backendGeneration);
    createWindow();
    createTray();
    if (!backendReady) {
      reportToRenderer("error", {
        message: "后台控制服务未能在规定时间内就绪，请检查 Router/后台诊断。",
      });
    }

    // Keep the pre-paint window colour aligned with the OS theme. The panel
    // itself follows the stored preference (which may be explicit light/dark),
    // so this only removes the flash for the `system` default.
    nativeTheme.on("updated", () => {
      if (mainWindow && !mainWindow.isDestroyed()) {
        mainWindow.setBackgroundColor(
          nativeTheme.shouldUseDarkColors ? "#0b1516" : "#f0fbfd",
        );
      }
    });

    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) {
        createWindow();
      } else if (mainWindow) {
        mainWindow.show();
      }
    });
  });
}

// 应用完全退出时先让 Web 侧的 Supervisor 停止它拥有的 Router，再清理
// Rust Web 进程。这样 Electron 托盘退出与 Router 的生命周期是同一条链。
app.on("before-quit", (event) => {
  if (quitCleanupStarted) return;
  event.preventDefault();
  quitCleanupStarted = true;
  isQuitting = true;
  if (backendStartTimer) {
    clearTimeout(backendStartTimer);
    backendStartTimer = null;
  }
  stopBackendWatchdog();
  void (async () => {
    console.log("[Electron Main] 正在停止 Router 与 Rust 后台守护进程...");
    await requestRouterStop();
    const child = rustProcess;
    if (child) {
      try {
        child.kill("SIGTERM");
      } catch {
        // 忽略：进程可能已经退出。
      }
      await new Promise((resolve) => {
        let settled = false;
        const finish = () => {
          if (settled) return;
          settled = true;
          resolve();
        };
        child.once("close", finish);
        setTimeout(() => {
          if (!settled) {
            try {
              child.kill("SIGKILL");
            } catch {
              // The process may have exited between the timeout and the kill.
            }
            finish();
          }
        }, 3000);
      });
    }
    app.quit();
  })();
});

app.on("window-all-closed", () => {
  // macOS 上保持在托盘运行，Linux/Windows 上也因为拦截了 close 事件常驻托盘
  // 此处不需要 quit
});
