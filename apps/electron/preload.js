const { contextBridge, ipcRenderer } = require("electron");

// 同步获取主进程生成的 localToken，彻底消除异步时序竞争
const localToken = ipcRenderer.sendSync("get-local-token-sync");

// 向渲染层（Panel 网页）注入安全本地免密通信通道与系统控制方法
contextBridge.exposeInMainWorld("electronAPI", {
  isElectron: true,
  localToken: localToken,
  minimizeWindow: () => ipcRenderer.send("window-minimize"),
  maximizeWindow: () => ipcRenderer.send("window-maximize"),
  closeWindow: () => ipcRenderer.send("window-close"),
  quitApp: () => ipcRenderer.send("app-quit"),
  syncCodex: () => ipcRenderer.invoke("sync-codex"),
  getStatus: () => ipcRenderer.invoke("get-status"),
});
