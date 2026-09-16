const { contextBridge, ipcRenderer } = require("electron");

// 向渲染层（Panel 网页）注入安全本地免密通信通道与系统控制方法
contextBridge.exposeInMainWorld("electronAPI", {
  isElectron: true,
  localToken: undefined,
  minimizeWindow: () => ipcRenderer.send("window-minimize"),
  maximizeWindow: () => ipcRenderer.send("window-maximize"),
  closeWindow: () => ipcRenderer.send("window-close"),
  quitApp: () => ipcRenderer.send("app-quit"),
  syncCodex: () => ipcRenderer.invoke("sync-codex"),
  getStatus: () => ipcRenderer.invoke("get-status"),
});

// 接收主进程分发的动态 localToken
window.addEventListener("DOMContentLoaded", async () => {
  try {
    const token = await ipcRenderer.invoke("get-local-token");
    if (window.electronAPI) {
      window.electronAPI.localToken = token;
    }
  } catch (err) {
    console.error("Failed to load electron local token:", err);
  }
});
