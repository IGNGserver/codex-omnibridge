const { contextBridge, ipcRenderer } = require("electron");

// 同步获取主进程生成的 localToken 与后端 origin。
// 主进程会校验发送方是否为面板自身的文档；来源不可信时返回空 token。
const bootstrap = ipcRenderer.sendSync("get-bootstrap") || { localToken: "", apiBase: "" };

// 只暴露面板实际使用的能力。窗口控制之外一律不暴露：每多一个 IPC 通道，
// 就多一条被注入脚本可达的特权路径。
contextBridge.exposeInMainWorld("electronAPI", {
  isElectron: true,
  localToken: bootstrap.localToken,
  apiBase: bootstrap.apiBase,
  appVersion: bootstrap.appVersion || "",
  minimizeWindow: () => ipcRenderer.send("window-minimize"),
  maximizeWindow: () => ipcRenderer.send("window-maximize"),
  closeWindow: () => ipcRenderer.send("window-close"),
  // 主进程推送的后端状态（启动失败、同步完成等），只读单向通道。
  onBackendEvent: (handler) => {
    if (typeof handler !== "function") return;
    ipcRenderer.on("backend-event", (_event, payload) => handler(payload));
  },
});
