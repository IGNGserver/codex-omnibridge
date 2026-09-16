const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, dialog } = require("electron");
const path = require("path");
const { spawn } = require("child_process");
const http = require("http");
const crypto = require("crypto");
const fs = require("fs");

let mainWindow = null;
let tray = null;
let rustProcess = null;
let isQuitting = false;

// 动态生成本次运行专属的本地特权 Token（免密通道）
const localToken = crypto.randomUUID();
const defaultPort = 31828;

// 保存 localToken 到用户数据目录供 CLI/扩展安全免密调用
try {
  const tokenFile = path.join(app.getPath("userData"), "local_token");
  fs.writeFileSync(tokenFile, localToken, { mode: 0o600 });
} catch (e) {
  // 忽略
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

// 启动 Rust 后台 Web & 路由服务
function startRustBackend() {
  const binPath = getBinaryPath();
  console.log(`[Electron Main] 启动 Rust 后台守护进程: ${binPath}`);

  // 启动参数：--headless 避免 Rust 再弹出基于 DBus/Win32 的原生托盘，统一由 Electron 托盘接管
  const args = [
    "web",
    "start",
    "--headless",
    `--local-token=${localToken}`,
    `--port=${defaultPort}`,
  ];

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
      console.log(`[Rust Core stdout] ${data.toString().trim()}`);
    });

    rustProcess.stderr.on("data", (data) => {
      console.error(`[Rust Core stderr] ${data.toString().trim()}`);
    });

    rustProcess.on("close", (code) => {
      console.log(`[Rust Core] 进程已退出，退出码: ${code}`);
      if (!isQuitting) {
        console.warn("[Rust Core] 异常退出，尝试重启...");
        setTimeout(startRustBackend, 3000);
      }
    });

    rustProcess.on("error", (err) => {
      console.error(`[Rust Core] 启动失败:`, err);
      if (mainWindow) {
        mainWindow.webContents.executeJavaScript(`
          console.error("启动后台 Rust 失败: ${err.message.replace(/\\/g, '\\\\')}");
        `);
      }
    });
  } catch (err) {
    console.error(`[Rust Core] 创建子进程异常:`, err);
  }
}

// 等待后端 HTTP 监听就绪
function waitForBackend(url, maxRetries = 40, intervalMs = 250) {
  return new Promise((resolve, reject) => {
    let retries = 0;
    const check = () => {
      const req = http.get(url, (res) => {
        resolve();
      });
      req.on("error", () => {
        retries++;
        if (retries >= maxRetries) {
          reject(new Error(`Backend failed to respond at ${url}`));
        } else {
          setTimeout(check, intervalMs);
        }
      });
      req.end();
    };
    check();
  });
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1220,
    height: 840,
    minWidth: 920,
    minHeight: 660,
    title: "Codex OmniBridge",
    icon: getAppIcon(),
    show: false,
    frame: false, // 去除系统原生外边框和原生标题栏
    autoHideMenuBar: true, // 彻底隐藏并去除 File, Edit, View 等菜单栏
    backgroundColor: "#0f1416",
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      nodeIntegration: false,
      contextIsolation: true,
      sandbox: false,
      webSecurity: false,
    },
  });

  // 彻底移除默认的 application menu
  Menu.setApplicationMenu(null);

  const localFile = path.resolve(__dirname, "../panel/index.html");
  const webUrl = `http://localhost:${defaultPort}`;

  // 优先直接加载本地打包的控制中心页面（永不黑屏，即开即显）
  if (fs.existsSync(localFile)) {
    mainWindow.loadFile(localFile, {
      query: { local_token: localToken }
    });
  } else {
    mainWindow.loadURL(`${webUrl}?local_token=${localToken}`);
  }

  mainWindow.once("ready-to-show", () => {
    mainWindow.show();
    mainWindow.focus();
  });

  // 兜底显示：防止极端情况下 ready-to-show 没触发导致一直黑屏
  setTimeout(() => {
    if (mainWindow && !mainWindow.isVisible()) {
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

  // 页面标题同步与外部链接拦截
  mainWindow.webContents.setWindowOpenHandler(({ url }) => {
    require("electron").shell.openExternal(url);
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
      click: async () => {
        try {
          const binPath = getBinaryPath();
          const p = spawn(binPath, ["sync"], { windowsHide: true });
          p.on("close", (code) => {
            if (code === 0) {
              if (mainWindow) {
                mainWindow.webContents.executeJavaScript(
                  `notify("全量配置已成功同步到 Codex！")`
                );
              }
            }
          });
        } catch (e) {
          console.error("Tray sync failed:", e);
        }
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

// IPC 处理器：注入 localToken 免密与窗口控制
ipcMain.on("get-local-token-sync", (event) => {
  event.returnValue = localToken;
});

ipcMain.handle("get-local-token", () => {
  return localToken;
});

ipcMain.on("window-minimize", () => {
  if (mainWindow) mainWindow.minimize();
});

ipcMain.on("window-maximize", () => {
  if (mainWindow) {
    if (mainWindow.isMaximized()) {
      mainWindow.unmaximize();
    } else {
      mainWindow.maximize();
    }
  }
});

ipcMain.on("window-close", () => {
  if (mainWindow) mainWindow.hide();
});

ipcMain.on("app-quit", () => {
  isQuitting = true;
  app.quit();
});

ipcMain.handle("sync-codex", async () => {
  const binPath = getBinaryPath();
  return new Promise((resolve) => {
    const p = spawn(binPath, ["sync"], { windowsHide: true });
    p.on("close", (code) => {
      resolve({ success: code === 0 });
    });
  });
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
