//! Platform-agnostic system tray runner.
//! On Linux, uses ksni (StatusNotifierItem via DBus) with zero GTK dependencies.
//! On Windows, uses Shell_NotifyIconW with native win32 message loop.
//! On other systems (or when headless), provides a clean fallback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
pub struct TrayConfig {
    pub web_url: String,
    pub title: String,
}

#[allow(dead_code)]
pub struct TrayHandle {
    stop_signal: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl TrayHandle {
    pub fn stop(&self) {
        self.stop_signal.store(true, Ordering::SeqCst);
    }
}

pub fn spawn_tray(config: TrayConfig) -> Option<TrayHandle> {
    let stop_signal = Arc::new(AtomicBool::new(false));
    let stop_clone = stop_signal.clone();

    #[cfg(target_os = "linux")]
    {
        thread::spawn(move || {
            linux_tray::run_linux_tray(config, stop_clone);
        });
        return Some(TrayHandle { stop_signal });
    }

    #[cfg(windows)]
    {
        thread::spawn(move || {
            windows_tray::run_windows_tray(config, stop_clone);
        });
        return Some(TrayHandle { stop_signal });
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (config, stop_clone);
        None
    }
}

// ============================================================================
// Linux Tray (via ksni - zero GTK/WebKit)
// ============================================================================
#[cfg(target_os = "linux")]
mod linux_tray {
    use super::*;
    use ksni::{MenuItem, Tray, TrayMethods, menu::StandardItem};

    struct CodexTrayItem {
        config: TrayConfig,
    }

    impl Tray for CodexTrayItem {
        fn id(&self) -> String {
            "codex-multiprovider".into()
        }

        fn title(&self) -> String {
            self.config.title.clone()
        }

        fn category(&self) -> ksni::Category {
            ksni::Category::ApplicationStatus
        }

        fn status(&self) -> ksni::Status {
            ksni::Status::Active
        }

        fn icon_name(&self) -> String {
            "preferences-system-network".into()
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            let _ = open::that(&self.config.web_url);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let url = self.config.web_url.clone();
            vec![
                StandardItem {
                    label: "打开控制面板".into(),
                    activate: Box::new(move |_| {
                        let _ = open::that(&url);
                    }),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: format!("访问地址: {}", self.config.web_url),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "退出 OmniBridge".into(),
                    activate: Box::new(|_| {
                        std::process::exit(0);
                    }),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    pub fn run_linux_tray(config: TrayConfig, stop: Arc<AtomicBool>) {
        let tray = CodexTrayItem { config };
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(_) => return,
        };

        rt.block_on(async {
            let handle = match tray.spawn().await {
                Ok(h) => h,
                Err(_) => return,
            };

            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let _ = handle.shutdown().await;
        });
    }
}

// ============================================================================
// Windows Native Tray (Shell_NotifyIconW + Win32 API)
// ============================================================================
#[cfg(windows)]
mod windows_tray {
    use super::*;
    use std::ptr::null_mut;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
        GetCursorPos, IDI_APPLICATION, LoadIconW, MF_DISABLED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
        MSG, PostQuitMessage, RegisterClassW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN,
        TrackPopupMenu, TranslateMessage, WM_APP, WM_COMMAND, WM_DESTROY, WM_LBUTTONDBLCLK,
        WM_RBUTTONUP, WNDCLASSW,
    };

    const WM_TRAYICON: u32 = WM_APP + 1;
    const IDM_OPEN: usize = 1001;
    const IDM_URL: usize = 1002;
    const IDM_EXIT: usize = 1003;

    static GLOBAL_CONFIG: OnceLock<TrayConfig> = OnceLock::new();

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TRAYICON => {
                match lparam as u32 {
                    WM_LBUTTONDBLCLK => {
                        if let Some(config) = GLOBAL_CONFIG.get() {
                            let _ = open::that(&config.web_url);
                        }
                    }
                    WM_RBUTTONUP => {
                        let mut pt = POINT { x: 0, y: 0 };
                        GetCursorPos(&mut pt);
                        let menu = CreatePopupMenu();

                        let open_str: Vec<u16> = "打开控制面板\0".encode_utf16().collect();
                        let exit_str: Vec<u16> = "退出 OmniBridge\0".encode_utf16().collect();
                        let url_str: Vec<u16> = format!(
                            "访问: {}\0",
                            GLOBAL_CONFIG
                                .get()
                                .map(|c| c.web_url.as_str())
                                .unwrap_or("")
                        )
                        .encode_utf16()
                        .collect();

                        AppendMenuW(menu, MF_STRING, IDM_OPEN, open_str.as_ptr());
                        AppendMenuW(
                            menu,
                            MF_STRING | MF_DISABLED | MF_GRAYED,
                            IDM_URL,
                            url_str.as_ptr(),
                        );
                        AppendMenuW(menu, MF_SEPARATOR, 0, null_mut());
                        AppendMenuW(menu, MF_STRING, IDM_EXIT, exit_str.as_ptr());

                        SetForegroundWindow(hwnd);
                        TrackPopupMenu(
                            menu,
                            TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                            pt.x,
                            pt.y,
                            0,
                            hwnd,
                            null_mut(),
                        );
                        DestroyMenu(menu);
                    }
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                match wparam {
                    IDM_OPEN => {
                        if let Some(config) = GLOBAL_CONFIG.get() {
                            let _ = open::that(&config.web_url);
                        }
                    }
                    IDM_EXIT => {
                        std::process::exit(0);
                    }
                    _ => {}
                }
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    pub fn run_windows_tray(config: TrayConfig, stop: Arc<AtomicBool>) {
        unsafe {
            let _ = GLOBAL_CONFIG.set(config.clone());
            let class_name: Vec<u16> = "CodexTrayWindowClass\0".encode_utf16().collect();

            let wnd_class = WNDCLASSW {
                style: 0,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: null_mut(),
                hIcon: null_mut(),
                hCursor: null_mut(),
                hbrBackground: null_mut(),
                lpszMenuName: null_mut(),
                lpszClassName: class_name.as_ptr(),
            };

            RegisterClassW(&wnd_class);

            let hwnd = windows_sys::Win32::UI::WindowsAndMessaging::CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );

            if hwnd.is_null() {
                return;
            }

            let icon = LoadIconW(null_mut(), IDI_APPLICATION);
            let mut tip: [u16; 128] = [0; 128];
            let title_units: Vec<u16> = config.title.encode_utf16().collect();
            let len = title_units.len().min(127);
            tip[..len].copy_from_slice(&title_units[..len]);

            let mut nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAYICON,
                hIcon: icon,
                szTip: tip,
                dwState: 0,
                dwStateMask: 0,
                szInfo: [0; 256],
                Anonymous: std::mem::zeroed(),
                szInfoTitle: [0; 64],
                dwInfoFlags: 0,
                guidItem: std::mem::zeroed(),
                hBalloonIcon: null_mut(),
            };

            Shell_NotifyIconW(NIM_ADD, &mut nid);

            // 消息循环
            let mut msg = MSG {
                hwnd: null_mut(),
                message: 0,
                wParam: 0,
                lParam: 0,
                time: 0,
                pt: POINT { x: 0, y: 0 },
            };

            while !stop.load(Ordering::SeqCst) {
                while windows_sys::Win32::UI::WindowsAndMessaging::PeekMessageW(
                    &mut msg,
                    null_mut(),
                    0,
                    0,
                    windows_sys::Win32::UI::WindowsAndMessaging::PM_REMOVE,
                ) != 0
                {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                thread::sleep(Duration::from_millis(50));
            }

            Shell_NotifyIconW(NIM_DELETE, &mut nid);
            DestroyWindow(hwnd);
        }
    }
}
