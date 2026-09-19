const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, shell, nativeTheme } = require("electron");
const path = require("path");
const { fileURLToPath } = require("url");
const { spawn } = require("child_process");
const crypto = require("crypto");
const fs = require("fs");

let mainWindow = null;
let tray = null;
let rustProcess = null;
let isQuitting = false;
let restartAttempts = 0;

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
  if (!mainWindow || mainWindow.isDestroyed()) {
    return;
  }
  // Structured payload instead of string-interpolated script: an error message
  // containing quotes or backticks must not be able to alter the executed code.
  mainWindow.webContents.send("backend-event", { kind, ...payload });
}

// 启动 Rust 后台 Web & 路由服务
function startRustBackend() {
  const binPath = getBinaryPath();
  console.log(`[Electron Main] 启动 Rust 后台守护进程: ${binPath}`);

  // 启动参数：--headless 避免 Rust 再弹出基于 DBus/Win32 的原生托盘，统一由 Electron 托盘接管。
  // localToken 只通过环境变量传递，不再放进 argv：命令行参数在 Linux 上对所有本地用户可见。
  const args = ["web", "start", "--headless", `--port=${defaultPort}`];

  try {
    rustProcess = spawn(binPath, args, {
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      detached: false,
      env: {
        ...process.env,
        CODEX_MP_LOCAL_TOKEN: localToken,
      },
    });

    rustProcess.stdout.on("data", (data) => {
      const text = data.toString();
      console.log(`[Rust Core stdout] ${text.trim()}`);
      // The backend prints this banner only after it has bound its port and
      // started serving. Resetting the counter here makes the 5-attempt limit
      // mean "5 consecutive failures": previously nothing ever reset it, so five
      // crashes spread across hours of healthy operation permanently disabled
      // auto-restart and the panel was unrecoverable without a manual relaunch.
      if (text.includes("Web 控制面板已启动")) {
        restartAttempts = 0;
        console.log("[Rust Core] 后端已就绪，重启计数已重置。");
      }
    });

    rustProcess.stderr.on("data", (data) => {
      console.error(`[Rust Core stderr] ${data.toString().trim()}`);
    });

    rustProcess.on("error", (err) => {
      console.error(`[Rust Core] 启动失败:`, err);
      reportToRenderer("error", { message: `启动后台 Rust 失败: ${err.message}` });
    });

    rustProcess.on("close", (code) => {
      console.log(`[Rust Core] 进程已退出，退出码: ${code}`);
      if (isQuitting) {
        return;
      }
      // 指数退避并设置上限，避免后端持续崩溃时无限重启刷屏。
      if (restartAttempts >= 5) {
        console.error("[Rust Core] 已连续重启 5 次仍失败，停止自动重启。");
        reportToRenderer("error", {
          message: "后台服务连续启动失败，已停止自动重启。请检查端口占用或日志。",
        });
        return;
      }
      const delay = Math.min(1000 * 2 ** restartAttempts, MAX_RESTART_DELAY_MS);
      restartAttempts += 1;
      console.warn(`[Rust Core] 异常退出，${delay}ms 后重试（第 ${restartAttempts} 次）...`);
      setTimeout(startRustBackend, delay);
    });
  } catch (err) {
    console.error(`[Rust Core] 创建子进程异常:`, err);
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
        const binPath = getBinaryPath();
        const p = spawn(binPath, ["sync"], { windowsHide: true });
        p.on("error", (err) => {
          console.error("Tray sync failed to spawn:", err);
          reportToRenderer("error", { message: `同步失败: ${err.message}` });
        });
        p.on("close", (code) => {
          if (code === 0) {
            reportToRenderer("sync-finished", { success: true });
          } else {
            reportToRenderer("error", { message: `同步失败，退出码 ${code}` });
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
    event.returnValue = { localToken: "", apiBase };
    return;
  }
  event.returnValue = { localToken, apiBase };
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

  app.whenReady().then(() => {
    startRustBackend();
    createWindow();
    createTray();

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

// 应用完全退出时清理 Rust 后台进程
app.on("before-quit", () => {
  isQuitting = true;
  if (rustProcess) {
    try {
      console.log("[Electron Main] 正在停止 Rust 后台守护进程...");
      rustProcess.kill("SIGTERM");
    } catch (e) {
      // 忽略
    }
  }
});

app.on("window-all-closed", () => {
  // macOS 上保持在托盘运行，Linux/Windows 上也因为拦截了 close 事件常驻托盘
  // 此处不需要 quit
});
