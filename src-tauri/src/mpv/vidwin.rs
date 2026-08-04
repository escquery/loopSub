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
    /// 鼠标命中穿透（子窗口嵌入形态）：视频层纯显示，输入全归下层 WebView2
    input_passthrough: bool,
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
        WM_MOUSEACTIVATE => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
            if !shared.is_null() && (*shared).input_passthrough {
                // 穿透形态：激活主窗口（标题栏正常点亮、Focused(true) →
                // set_focus 把键盘焦点还给 WebView2），但吞掉这一击的派发——
                // 否则 WM_LBUTTONDOWN 到本窗口后 DefWindowProc 会 SetFocus 给
                // 自己，把刚还回去的焦点抢走。必须是 ACTIVATEANDEAT 而非
                // NOACTIVATEANDEAT：后者连顶层窗口激活一起挡掉（标题栏不亮、
                // Focused 不触发）。画面区无交互（osc 已关），吞掉无副作用。
                return MA_ACTIVATEANDEAT as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_NCHITTEST => {
            let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
            if !shared.is_null() && (*shared).input_passthrough {
                // 穿透到下层兄弟（WebView2）：按键/点击/滚轮全归 UI 层
                return HTTRANSPARENT as LRESULT;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
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

struct WinCfg {
    parent: usize, // HWND 转 usize 以跨线程传递（裸指针非 Send）
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    title: &'static str,
}

unsafe fn create_window(cfg: &WinCfg) -> Result<HWND, String> {
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

    let title: Vec<u16> = cfg.title.encode_utf16().chain(std::iter::once(0)).collect();
    let hwnd = CreateWindowExW(
        0,
        class.as_ptr(),
        title.as_ptr(),
        cfg.style,
        cfg.x,
        cfg.y,
        cfg.w,
        cfg.h,
        cfg.parent as HWND,
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
    fn spawn(
        cfg: WinCfg,
        input_passthrough: bool,
        on_close: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Result<usize, String>>();
        let shared = Arc::new(Shared {
            destroyed: AtomicBool::new(false),
            on_close: Box::new(on_close),
            input_passthrough,
        });
        let shared2 = shared.clone();
        let thread = std::thread::spawn(move || {
            match unsafe { create_window(&cfg) } {
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

    /// 在专用线程创建顶层窗口并跑消息泵；HWND（转 usize）经 channel 传回
    pub fn create(on_close: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        Self::spawn(
            WinCfg {
                parent: 0,
                style: WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                x: CW_USEDEFAULT as i32,
                y: 0,
                w: 1280,
                h: 720,
                title: "loopSub",
            },
            false,
            on_close,
        )
    }

    /// 子窗口形态（Phase C 叠层单窗口）：在**调用线程**创建——调用方必须保证
    /// 这是主线程（embed 经 run_on_main_thread 投递；Tauri 主线程 event loop
    /// 顺带 Dispatch 子窗口消息）。
    /// 为何不开专用线程：命中穿透（HTTRANSPARENT）与 SetWindowPos 重排都只在
    /// 同线程窗口间可靠工作；跨线程实测死锁（主窗口 Responding=False）。
    /// 输入穿透——视频层纯显示，鼠标/键盘全归下层的 WebView2 UI。
    pub fn create_child(parent: HWND, x: i32, y: i32, w: i32, h: i32) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            destroyed: AtomicBool::new(false),
            on_close: Box::new(|| {}), // 子窗口无关闭按钮，不会触发
            input_passthrough: true,
        });
        let hwnd = unsafe {
            create_window(&WinCfg {
                parent: parent as usize,
                style: WS_CHILD | WS_VISIBLE,
                x,
                y,
                w,
                h,
                title: "",
            })
        }?;
        let raw = Arc::into_raw(shared.clone());
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };
        Ok(Self {
            hwnd: hwnd as usize,
            shared,
            thread: None, // 消息由宿主线程的泵分发
        })
    }

    /// 跟随父窗口拉伸重排（客户区坐标）
    pub fn set_rect(&self, x: i32, y: i32, w: i32, h: i32) {
        unsafe {
            SetWindowPos(
                self.hwnd(),
                std::ptr::null_mut(),
                x,
                y,
                w,
                h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// 抬到兄弟窗口 z 序顶端（不抢焦点）
    pub fn raise(&self) {
        unsafe {
            SetWindowPos(
                self.hwnd(),
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
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
