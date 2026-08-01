//! 自建视频窗口（Phase B）：HWND 经 wid 选项交给 libmpv 渲染。
//! 窗口归本进程所有——z-order 直接按句柄控制（召回不再枚举猜窗）、
//! 关闭行为与 mpv 自建窗口一致（点 X = 投递 quit 并立即消失）。
//!
//! 窗口在专用线程创建并跑消息泵；mpv 会子类化该窗口处理渲染尺寸，
//! 与这里的 wndproc 互不冲突（自定义消息之外全走 DefWindowProc）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use windows_sys::Win32::Foundation::{GetLastError, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// 通知窗口线程销毁窗口（MpvEmbed 清理路径；WM_APP 起的应用私有消息）
const WM_VIDWIN_CLOSE: u32 = WM_APP + 1;
const CLASS_NAME: &str = "loopsub-video";

struct Shared {
    destroyed: AtomicBool,
    /// 用户点 X 时回调（向 mpv 投递 quit），由 MpvEmbed 注入
    on_close: Box<dyn Fn() + Send + Sync>,
}

pub struct VideoWindow {
    hwnd: usize, // HWND 存 usize：裸指针非 Send，会破坏 AppState 的 Send+Sync
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
            if !shared.is_null() {
                ((*shared).on_close)();
            }
            // 与 mpv 自建窗口一致：窗口立即消失（DestroyWindow 同步触发 WM_DESTROY）
            DestroyWindow(hwnd);
            0
        }
        WM_VIDWIN_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
            if !shared.is_null() {
                (*shared).destroyed.store(true, Ordering::SeqCst);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                // 归还窗口持有的那份 Arc 引用（create 里 into_raw 的）
                drop(Arc::from_raw(shared));
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn create_window() -> Result<HWND, String> {
    let hinstance = GetModuleHandleW(std::ptr::null());
    let class: Vec<u16> = CLASS_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let wc = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        // 不擦背景：客户区由 mpv 重绘，避免启动白闪
        hbrBackground: std::ptr::null_mut(),
        lpszClassName: class.as_ptr(),
        ..std::mem::zeroed()
    };
    // 重建实例时类已注册，返回 0 属正常，忽略
    RegisterClassW(&wc);

    let title: Vec<u16> = "loopSub".encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        1280,
        720,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        hinstance,
        std::ptr::null(),
    );
    if hwnd.is_null() {
        Err(format!("CreateWindowExW 失败: {}", GetLastError()))
    } else {
        Ok(hwnd)
    }
}

impl VideoWindow {
    /// 在专用线程创建窗口并跑消息泵；HWND（转 usize）经 channel 传回
    pub fn create(on_close: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Result<usize, String>>();
        let shared = Arc::new(Shared {
            destroyed: AtomicBool::new(false),
            on_close: Box::new(on_close),
        });
        let shared2 = shared.clone();
        let thread = std::thread::spawn(move || {
            match unsafe { create_window() } {
                Ok(hwnd) => {
                    let _ = tx.send(Ok(hwnd as usize));
                    // Shared 挂到窗口上（into_raw 的引用在 WM_DESTROY 归还）
                    let raw = Arc::into_raw(shared2);
                    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
                    unsafe {
                        let mut msg: MSG = std::mem::zeroed();
                        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                            TranslateMessage(&msg);
                            DispatchMessageW(&msg);
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        });
        let hwnd = rx
            .recv()
            .map_err(|_| "视频窗口线程异常退出".to_string())??;
        Ok(Self {
            hwnd,
            shared,
            thread: Some(thread),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }

    /// 销毁窗口并收线程；用户已点 X（窗口已毁）时仅收线程
    pub fn close(mut self) {
        let destroyed = self.shared.destroyed.load(Ordering::SeqCst);
        eprintln!("[vidwin] close: destroyed={destroyed}");
        if !destroyed {
            unsafe { PostMessageW(self.hwnd as HWND, WM_VIDWIN_CLOSE, 0, 0) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        eprintln!("[vidwin] closed");
    }
}
