//! libmpv 进程内嵌入：dlopen 动态库并直接调用 Client API。
//! Windows 通过 wid 输出到自建 HWND；macOS 通过 Render API 输出到
//! `NSOpenGLView`；均无命名管道连接/重试和外部 mpv 子进程。
//!
//! 手动连接外部已运行 mpv 的模式仍走 [`crate::mpv::MpvIpc`]，不受影响。
//!
//! 线程模型：mpv handle 的 command/property API 多线程安全（core 内部加锁）；
//! mpv_wait_event 只能单线程调用——专用事件线程消费事件队列，并在收到
//! SHUTDOWN（用户直接关闭 mpv 窗口）时置 dead 标志供上层感知。

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::Value;
use tauri::Emitter;

// ---- mpv client.h 常量（ABI 稳定，见 mpv 文档"Client API changes"） ----
const MPV_FORMAT_STRING: c_int = 1;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;
const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_CLIENT_MESSAGE: c_int = 16;

/// 只需读首字段；后续字段按 client.h 原样排布以保证偏移正确
#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

/// MPV_EVENT_CLIENT_MESSAGE 的 data（client.h: mpv_event_client_message）。
/// mpv 视频窗获得焦点时，绑定键通过 script-message 回送到 Tauri 前端。
#[repr(C)]
struct MpvEventClientMessage {
    num_args: c_int,
    args: *mut *const c_char,
}

type Handle = *mut c_void;

/// dlopen 得到的 C API 函数表；Library 随表持有，保证函数指针始终有效
pub struct MpvApi {
    _lib: libloading::Library,
    client_api_version: unsafe extern "C" fn() -> c_ulong,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    #[cfg_attr(not(windows), allow(dead_code))]
    set_option: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    #[cfg_attr(not(windows), allow(dead_code))]
    command_string: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    get_property_string: unsafe extern "C" fn(Handle, *const c_char) -> *mut c_char,
    set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut MpvEvent,
    terminate_destroy: unsafe extern "C" fn(Handle),
    free: unsafe extern "C" fn(*mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    #[cfg(target_os = "macos")]
    render: super::render_macos::RenderFns,
}

// client API 文档保证 handle 级函数多线程安全（wait_event 已单线程化）
unsafe impl Send for MpvApi {}
unsafe impl Sync for MpvApi {}

impl Drop for MpvApi {
    fn drop(&mut self) {
        eprintln!("[embed] MpvApi dropped (FreeLibrary)");
    }
}

impl MpvApi {
    pub fn load(path: &Path) -> Result<Arc<Self>, String> {
        unsafe {
            let lib = libloading::Library::new(path)
                .map_err(|e| format!("加载 {} 失败: {e}", path.display()))?;
            macro_rules! sym {
                ($name:literal, $ty:ty) => {
                    *lib.get::<$ty>($name.as_bytes())
                        .map_err(|e| format!("{} 缺少导出 {}: {e}", path.display(), $name))?
                };
            }
            let api = Self {
                client_api_version: sym!("mpv_client_api_version", unsafe extern "C" fn() -> c_ulong),
                create: sym!("mpv_create", unsafe extern "C" fn() -> Handle),
                initialize: sym!("mpv_initialize", unsafe extern "C" fn(Handle) -> c_int),
                set_option: sym!(
                    "mpv_set_option",
                    unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int
                ),
                set_option_string: sym!(
                    "mpv_set_option_string",
                    unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int
                ),
                command: sym!(
                    "mpv_command",
                    unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int
                ),
                command_string: sym!(
                    "mpv_command_string",
                    unsafe extern "C" fn(Handle, *const c_char) -> c_int
                ),
                get_property: sym!(
                    "mpv_get_property",
                    unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int
                ),
                get_property_string: sym!(
                    "mpv_get_property_string",
                    unsafe extern "C" fn(Handle, *const c_char) -> *mut c_char
                ),
                set_property_string: sym!(
                    "mpv_set_property_string",
                    unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int
                ),
                wait_event: sym!(
                    "mpv_wait_event",
                    unsafe extern "C" fn(Handle, f64) -> *mut MpvEvent
                ),
                terminate_destroy: sym!("mpv_terminate_destroy", unsafe extern "C" fn(Handle)),
                free: sym!("mpv_free", unsafe extern "C" fn(*mut c_void)),
                error_string: sym!(
                    "mpv_error_string",
                    unsafe extern "C" fn(c_int) -> *const c_char
                ),
                #[cfg(target_os = "macos")]
                render: super::render_macos::RenderFns {
                    create: sym!(
                        "mpv_render_context_create",
                        unsafe extern "C" fn(
                            *mut super::render_macos::RenderContext,
                            Handle,
                            *mut std::ffi::c_void,
                        ) -> c_int
                    ),
                    set_update_callback: sym!(
                        "mpv_render_context_set_update_callback",
                        unsafe extern "C" fn(
                            super::render_macos::RenderContext,
                            super::render_macos::UpdateCallback,
                            *mut c_void,
                        )
                    ),
                    update: sym!(
                        "mpv_render_context_update",
                        unsafe extern "C" fn(super::render_macos::RenderContext) -> u64
                    ),
                    render: sym!(
                        "mpv_render_context_render",
                        unsafe extern "C" fn(
                            super::render_macos::RenderContext,
                            *mut c_void,
                        ) -> c_int
                    ),
                    report_swap: sym!(
                        "mpv_render_context_report_swap",
                        unsafe extern "C" fn(super::render_macos::RenderContext)
                    ),
                    free: sym!(
                        "mpv_render_context_free",
                        unsafe extern "C" fn(super::render_macos::RenderContext)
                    ),
                },
                _lib: lib,
            };
            let ver = (api.client_api_version)();
            if (ver >> 16) < 1 {
                return Err(format!(
                    "libmpv 客户端 API 版本过旧: {}.{}",
                    ver >> 16,
                    ver & 0xFFFF
                ));
            }
            Ok(Arc::new(api))
        }
    }

    fn check(&self, code: c_int) -> Result<(), String> {
        if code < 0 {
            let msg = unsafe { CStr::from_ptr((self.error_string)(code)) }
                .to_string_lossy()
                .into_owned();
            Err(format!("mpv error {code}: {msg}"))
        } else {
            Ok(())
        }
    }
}

/// 常用属性的取值格式表（JSON IPC 的 get_property 返回带类型 data，
/// 前端依赖其类型；STRING 格式下 bool 会变成 "yes"/"no"，必须按表取）
fn format_of(name: &str) -> c_int {
    match name {
        "time-pos" | "duration" | "percent-pos" | "speed" | "sub-delay" | "sub-speed"
        | "volume" | "volume-max" | "time-remaining" | "audio-delay" | "cache-speed"
        // AB 循环端点：未设置时 mpv 返回错误，设置后是秒数；若走默认 STRING 分支，
        // 前端 typeof number 守卫会静默丢弃（nudge 失效、AB badge 永不显示）
        | "ab-loop-a" | "ab-loop-b" => MPV_FORMAT_DOUBLE,
        "pause" | "sub-visibility" | "core-idle" | "eof-reached" | "mute" | "seeking"
        | "idle-active" | "paused-for-cache" => MPV_FORMAT_FLAG,
        "chapter" | "chapter-count" | "playlist-pos" | "playlist-count" | "edition"
        // dwidth/dheight：视频显示像素；STRING 分支会返回 "1920" 字符串，
        // 前端 typeof number 守卫静默丢弃（窗口重置为视频大小失效）
        | "dwidth" | "dheight" | "window-id" => {
            MPV_FORMAT_INT64
        }
        _ => MPV_FORMAT_STRING,
    }
}

/// 命令参数转 C 字符串（mpv argv 风格；bool → yes/no，对象兜底 JSON——
/// 如 loadfile 的 options 请优先传 "key=value" 字符串，mpv 会同样解析）
fn value_to_cstring(v: &Value) -> Result<CString, String> {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_string(),
        Value::Null => return Err("命令参数不支持 null".into()),
        other => serde_json::to_string(other).map_err(|e| e.to_string())?,
    };
    CString::new(s).map_err(|_| "命令参数含 NUL 字符".to_string())
}

pub struct MpvEmbed {
    api: Arc<MpvApi>,
    handle: usize, // 原始指针转 usize 以获得 Send；调用处 cast 回来
    dead: Arc<AtomicBool>,
    /// 主动关闭中：通知事件线程退出（wait_event 的 unblock 依赖 libmpv，实测偶发失效）
    closing: Arc<AtomicBool>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
    /// 自建视频窗口：顶层（Phase B）或嵌入主窗口的子窗口（Phase C 单窗口）；
    /// 创建或 wid 设置失败时降级为 mpv 自建窗口
    #[cfg(windows)]
    vidwin: Mutex<Option<super::vidwin::VideoWindow>>,
    #[cfg(target_os = "macos")]
    renderer: Mutex<Option<super::render_macos::MacRenderer>>,
}

/// 视频子窗口布局（Phase C 单窗口，Windows）：父窗口 HWND + 客户区矩形（物理像素）。
/// None = 自建顶层视频窗口（Phase B 形态）。非 Windows 平台忽略（mpv 自建窗口）。
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct VidLayout {
    pub parent: usize,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl MpvEmbed {
    /// 缴械选项（与 DESIGN.md §2 原外部 mpv 的 spawn 参数一致）
    fn set_base_options(api: &MpvApi, handle: Handle) -> Result<(), String> {
        for (k, v) in [
            ("idle", "yes"),
            // RenderContext 必须在第一次 VO 创建前就绪；macOS 不让 force-window
            // 在 mpv_initialize 后抢先回退到 Cocoa 独立窗口。
            ("force-window", if cfg!(target_os = "macos") { "no" } else { "yes" }),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("keep-open", "yes"),
            ("sid", "no"),
        ] {
            let ck = CString::new(k).unwrap();
            let cv = CString::new(v).unwrap();
            let code = unsafe { (api.set_option_string)(handle, ck.as_ptr(), cv.as_ptr()) };
            // 裁剪掉 Lua/JavaScript/OSC 的轻量 libmpv 构建没有 osc 选项；这类
            // 构建本来就不会显示 OSC，因此 option-not-found（-5）等同目标状态。
            if k == "osc" && code == -5 {
                continue;
            }
            api.check(code)
                .map_err(|e| format!("设置 {k}={v} 失败: {e}"))?;
        }
        #[cfg(target_os = "macos")]
        {
            let key = CString::new("hwdec").unwrap();
            let value = CString::new("auto-safe").unwrap();
            api.check(unsafe { (api.set_option_string)(handle, key.as_ptr(), value.as_ptr()) })
                .map_err(|e| format!("启用 VideoToolbox 硬件解码失败: {e}"))?;
        }
        Ok(())
    }

    /// 事件线程：消费事件队列，侦测 SHUTDOWN（用户直接关了 mpv 窗口）。
    /// wait_event 用 200ms 超时轮询而非永久阻塞：terminate_destroy 的 unblock
    /// 承诺偶发失效（实测 join 永久卡死），超时+closing 标志保证 join 有硬上界。
    fn spawn_event_thread(
        api: &Arc<MpvApi>,
        handle: Handle,
        dead: &Arc<AtomicBool>,
        closing: &Arc<AtomicBool>,
        app: &tauri::AppHandle,
    ) -> JoinHandle<()> {
        std::thread::spawn({
            let api = api.clone();
            let dead = dead.clone();
            let closing = closing.clone();
            let app = app.clone();
            let handle = handle as usize;
            move || {
                let handle = handle as Handle;
                loop {
                    if closing.load(Ordering::SeqCst) {
                        break;
                    }
                    let ev = unsafe { (api.wait_event)(handle, 0.2) };
                    if closing.load(Ordering::SeqCst) {
                        break;
                    }
                    if ev.is_null() {
                        continue;
                    }
                    match unsafe { (*ev).event_id } {
                        MPV_EVENT_SHUTDOWN => {
                            dead.store(true, Ordering::SeqCst);
                            break;
                        }
                        MPV_EVENT_CLIENT_MESSAGE => unsafe {
                            let data = (*ev).data as *const MpvEventClientMessage;
                            if data.is_null() || (*data).args.is_null() || (*data).num_args < 2 {
                                continue;
                            }
                            let args = std::slice::from_raw_parts(
                                (*data).args,
                                (*data).num_args as usize,
                            );
                            let arg = |i: usize| {
                                args.get(i)
                                    .filter(|p| !p.is_null())
                                    .map(|p| CStr::from_ptr(*p).to_string_lossy().into_owned())
                            };
                            if arg(0).as_deref() == Some("loopsub-hotkey") {
                                if let Some(action) = arg(1) {
                                    let _ = app.emit("mpv-hotkey", action);
                                }
                            }
                        },
                        _ => {}
                    }
                }
            }
        })
    }

    /// 创建并初始化实例（"缴械"选项与原外部 mpv 的 spawn 参数一致，见 DESIGN.md §2）。
    /// layout：Windows 下 Some = 子窗口嵌入主窗口（Phase C 单窗口），
    /// None = 自建顶层视频窗口（Phase B）；创建/wid 失败均降级为 mpv 自建窗口。
    /// app 用于把子窗口创建投递到主线程（非 Windows 忽略）。
    pub fn new(api: Arc<MpvApi>, layout: Option<VidLayout>, app: &tauri::AppHandle) -> Result<Self, String> {
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("mpv_create 失败".into());
        }
        Self::set_base_options(&api, handle)?;

        // 自建视频窗口 + wid 内嵌
        #[cfg(windows)]
        let vidwin = {
            let try_create = |layout: Option<VidLayout>| -> Option<super::vidwin::VideoWindow> {
                let created = match layout {
                    // Phase C：子窗口嵌入主窗口（命中穿透，输入归 WebView2；
                    // 无关闭按钮，生命周期随主窗口）。子窗口必须与父窗口同属主线程
                    // （本函数跑在 tokio 工作线程），故投递创建、经 channel 取回——
                    // VideoWindow 句柄存 usize，可跨线程移动。调用方不得持 mpv 锁之外的
                    // 主线程可能等待的锁，否则与主线程形成等待环。
                    Some(l) => {
                        let (tx, rx) = std::sync::mpsc::channel();
                        match app.run_on_main_thread(move || {
                            let _ = tx.send(super::vidwin::VideoWindow::create_child(
                                l.parent as _, l.x, l.y, l.w, l.h,
                            ));
                        }) {
                            Ok(()) => rx.recv().unwrap_or_else(|_| Err("主线程已退出".into())),
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    // Phase B：顶层窗口，点 X = 向 core 投递 quit（同 mpv 自建窗口行为）
                    None => super::vidwin::VideoWindow::create({
                        let api = api.clone();
                        let h = handle as usize;
                        move || {
                            let quit = CString::new("quit").unwrap();
                            unsafe { (api.command_string)(h as Handle, quit.as_ptr()) };
                        }
                    }),
                };
                created.ok().and_then(|vw| {
                    let wid = vw.hwnd() as i64;
                    let opt = CString::new("wid").unwrap();
                    let ok = api.check(unsafe {
                        (api.set_option)(
                            handle,
                            opt.as_ptr(),
                            MPV_FORMAT_INT64,
                            &wid as *const i64 as *mut c_void,
                        )
                    });
                    match ok {
                        Ok(()) => Some(vw),
                        Err(_) => {
                            vw.close();
                            None
                        }
                    }
                })
            };
            try_create(layout)
        };
        #[cfg(not(windows))]
        let _ = layout;

        if let Err(e) = api.check(unsafe { (api.initialize)(handle) }) {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(format!("mpv_initialize 失败: {e}"));
        }

        #[cfg(target_os = "macos")]
        let renderer = match super::render_macos::MacRenderer::create(api.render, handle, app) {
            Ok(renderer) => renderer,
            Err(e) => {
                unsafe { (api.terminate_destroy)(handle) };
                return Err(e);
            }
        };

        let dead = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let event_thread = Self::spawn_event_thread(&api, handle, &dead, &closing, app);
        Ok(Self {
            api,
            handle: handle as usize,
            dead,
            closing,
            event_thread: Mutex::new(Some(event_thread)),
            #[cfg(windows)]
            vidwin: Mutex::new(vidwin),
            #[cfg(target_os = "macos")]
            renderer: Mutex::new(Some(renderer)),
        })
    }

    fn handle(&self) -> Handle {
        self.handle as Handle
    }

    /// mpv core 是否已退出（事件线程侦测）
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
    }

    /// 把前端快捷键同步成 mpv 输入段。内嵌视频层不收输入，但绑定无害；
    /// Linux 独立窗口和手动 IPC 模式用 client-message 转发为前端动作。
    pub fn bind_hotkeys(&self, hotkeys: &HashMap<String, String>) -> Result<(), String> {
        let config = hotkey_section(hotkeys);
        self.command(vec![
            "define-section".into(),
            "loopsub".into(),
            config.into(),
            "force".into(),
        ])?;
        self.command(vec!["enable-section".into(), "loopsub".into()])?;
        Ok(())
    }

    /// 与 [`crate::mpv::MpvIpc::command`] 同形的命令入口：
    /// get/set_property 特判走类型化 API，其余按 argv 透传
    pub fn command(&self, args: Vec<Value>) -> Result<Value, String> {
        let Some(op) = args.first().and_then(Value::as_str) else {
            return Err("empty command".into());
        };
        match op {
            "get_property" if args.len() == 2 => match args[1].as_str() {
                Some(name) => return self.get_property_value(name),
                None => return Err("get_property 属性名须为字符串".into()),
            },
            "set_property" if args.len() == 3 => match args[1].as_str() {
                Some(name) => {
                    return self.set_property_value(name, args[2].clone()).map(|_| Value::Null)
                }
                None => return Err("set_property 属性名须为字符串".into()),
            },
            _ => {}
        }
        if self.is_dead() {
            return Err("mpv 已退出".into());
        }
        let cstrs: Vec<CString> = args
            .iter()
            .map(value_to_cstring)
            .collect::<Result<_, _>>()?;
        let mut ptrs: Vec<*const c_char> = cstrs.iter().map(|s| s.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        self.api
            .check(unsafe { (self.api.command)(self.handle(), ptrs.as_mut_ptr()) })?;
        Ok(Value::Null)
    }

    pub fn get_property_value(&self, name: &str) -> Result<Value, String> {
        if self.is_dead() {
            return Err("mpv 已退出".into());
        }
        let cname = CString::new(name).map_err(|_| "属性名非法".to_string())?;
        let h = self.handle();
        unsafe {
            match format_of(name) {
                MPV_FORMAT_DOUBLE => {
                    let mut v = 0f64;
                    self.api.check((self.api.get_property)(
                        h,
                        cname.as_ptr(),
                        MPV_FORMAT_DOUBLE,
                        &mut v as *mut f64 as *mut c_void,
                    ))?;
                    Ok(Value::from(v))
                }
                MPV_FORMAT_FLAG => {
                    let mut v = 0i32;
                    self.api.check((self.api.get_property)(
                        h,
                        cname.as_ptr(),
                        MPV_FORMAT_FLAG,
                        &mut v as *mut i32 as *mut c_void,
                    ))?;
                    Ok(Value::from(v != 0))
                }
                MPV_FORMAT_INT64 => {
                    let mut v = 0i64;
                    self.api.check((self.api.get_property)(
                        h,
                        cname.as_ptr(),
                        MPV_FORMAT_INT64,
                        &mut v as *mut i64 as *mut c_void,
                    ))?;
                    Ok(Value::from(v))
                }
                _ => {
                    let p = (self.api.get_property_string)(h, cname.as_ptr());
                    if p.is_null() {
                        Err(format!("读取属性 {name} 失败"))
                    } else {
                        let s = CStr::from_ptr(p).to_string_lossy().into_owned();
                        (self.api.free)(p as *mut c_void);
                        Ok(Value::String(s))
                    }
                }
            }
        }
    }

    pub fn set_property_value(&self, name: &str, value: Value) -> Result<(), String> {
        if self.is_dead() {
            return Err("mpv 已退出".into());
        }
        let cname = CString::new(name).map_err(|_| "属性名非法".to_string())?;
        let cv = value_to_cstring(&value)?;
        self.api.check(unsafe {
            (self.api.set_property_string)(self.handle(), cname.as_ptr(), cv.as_ptr())
        })
    }

    /// 视频子窗口（Phase C 单窗口）重排/抬顶；非嵌入形态返回 None
    #[cfg(windows)]
    pub fn with_vidwin<R>(&self, f: impl FnOnce(&super::vidwin::VideoWindow) -> R) -> Option<R> {
        self.vidwin.lock().unwrap().as_ref().map(|v| f(v))
    }

    #[cfg(target_os = "macos")]
    pub fn set_render_layout(&self, drawer_w: f64, top_h: f64, bottom_h: f64) {
        if let Some(renderer) = self.renderer.lock().unwrap().as_ref() {
            renderer.set_layout(drawer_w, top_h, bottom_h);
        }
    }

    /// 销毁实例；幂等：core 已自行退出（用户关窗口）时仅清理句柄。
    /// 先置 closing 让事件线程在超时轮询内自行退出（join ≤200ms 有保证），
    /// 再 terminate_destroy 收尾；不依赖 wait_event 的 unblock 承诺。
    pub fn shutdown(&self) {
        eprintln!("[embed] shutdown: closing + terminate_destroy");
        #[cfg(target_os = "macos")]
        if let Some(mut renderer) = self.renderer.lock().unwrap().take() {
            renderer.shutdown();
        }
        self.closing.store(true, Ordering::SeqCst);
        if let Some(t) = self.event_thread.lock().unwrap().take() {
            let _ = t.join();
        }
        eprintln!("[embed] event thread joined, terminate_destroy");
        unsafe { (self.api.terminate_destroy)(self.handle()) };
        #[cfg(windows)]
        if let Some(vw) = self.vidwin.lock().unwrap().take() {
            eprintln!("[embed] closing vidwin");
            vw.close();
        }
        eprintln!("[embed] shutdown done");
    }
}

/// 解析 libmpv 动态库：设置的外部程序目录 → exe 同目录 → exe/mpv 子目录
/// → exe/resources 子目录（tauri bundle.resources 的默认释放位置，安装版布局）
pub fn resolve_dll(settings_dir: Option<PathBuf>) -> Option<PathBuf> {
    #[cfg(windows)]
    const NAMES: [&str; 2] = ["libmpv-2.dll", "mpv-2.dll"];
    #[cfg(target_os = "macos")]
    const NAMES: [&str; 2] = ["libmpv.dylib", "libmpv.2.dylib"];
    #[cfg(all(unix, not(target_os = "macos")))]
    const NAMES: [&str; 2] = ["libmpv.so.2", "libmpv.so"];

    let mut dirs = Vec::<PathBuf>::new();
    if let Some(dir) = settings_dir {
        dirs.push(dir.clone());
        dirs.push(dir.join("lib"));
        // 设置页常填 Homebrew 的 bin 目录；libmpv 实际在同级 lib。
        if dir.file_name().and_then(|n| n.to_str()) == Some("bin") {
            if let Some(prefix) = dir.parent() {
                dirs.push(prefix.join("lib"));
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("mpv"));
            dirs.push(dir.join("resources"));
            // macOS bundle：可执行文件在 Contents/MacOS，资源在 Contents/Resources。
            #[cfg(target_os = "macos")]
            if let Some(contents) = dir.parent() {
                let resources = contents.join("Resources");
                dirs.push(resources.clone());
                dirs.push(resources.join("mpv"));
                dirs.push(resources.join("lib"));
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Finder 启动的 .app 没有 Homebrew PATH；dlopen 裸名在 Apple Silicon 上
        // 也不会自动搜索 /opt/homebrew/lib，因此显式覆盖两种 Homebrew 前缀。
        if let Some(prefix) = std::env::var_os("HOMEBREW_PREFIX") {
            let prefix = PathBuf::from(prefix);
            dirs.push(prefix.join("lib"));
            dirs.push(prefix.join("opt/mpv/lib"));
        }
        for prefix in [Path::new("/opt/homebrew"), Path::new("/usr/local")] {
            dirs.push(prefix.join("lib"));
            dirs.push(prefix.join("opt/mpv/lib"));
        }
    }
    dirs.iter()
        .flat_map(|d| NAMES.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// 浏览器组合键名 → mpv input.conf 键名。
/// Shift+符号按 settings.js 的美式物理键归一规则还原为实际字符，避免 mpv
/// 忽略文本键的 Shift 修饰后与未按 Shift 的绑定冲突。
fn combo_to_mpv(combo: &str) -> Option<String> {
    let mut parts: Vec<&str> = combo.split('+').collect();
    let key = parts.pop()?;
    if key.is_empty() || key.contains(['\n', '\r', ' ', '"']) {
        return None;
    }
    let mut shift = false;
    let mut modifiers = Vec::new();
    for modifier in parts {
        match modifier.to_ascii_lowercase().as_str() {
            "ctrl" => modifiers.push("Ctrl"),
            "alt" => modifiers.push("Alt"),
            "meta" => modifiers.push("Meta"),
            "shift" => shift = true,
            _ => return None,
        }
    }
    let mut key = match key {
        "Space" => "SPACE".to_string(),
        "Enter" => "ENTER".to_string(),
        "Escape" => "ESC".to_string(),
        "ArrowLeft" => "LEFT".to_string(),
        "ArrowRight" => "RIGHT".to_string(),
        "ArrowUp" => "UP".to_string(),
        "ArrowDown" => "DOWN".to_string(),
        "Backspace" => "BS".to_string(),
        "Delete" => "DEL".to_string(),
        "#" => "SHARP".to_string(),
        other => other.to_string(),
    };
    let is_special = matches!(
        key.as_str(),
        "SPACE" | "ENTER" | "ESC" | "LEFT" | "RIGHT" | "UP" | "DOWN" | "BS" | "DEL"
    );
    if shift && !is_special {
        let shifted = match key.as_str() {
            "[" => Some("{"), "]" => Some("}"), "," => Some("<"), "." => Some(">"),
            "/" => Some("?"), "\\" => Some("|"), ";" => Some(":"), "'" => Some("\""),
            "-" => Some("_"), "=" => Some("+"), "`" => Some("~"), "1" => Some("!"),
            "2" => Some("@"), "3" => Some("#"), "4" => Some("$"), "5" => Some("%"),
            "6" => Some("^"), "7" => Some("&"), "8" => Some("*"), "9" => Some("("),
            "0" => Some(")"), _ => None,
        };
        if let Some(produced) = shifted {
            key = if produced == "#" { "SHARP".into() } else { produced.into() };
        } else if key.chars().count() == 1 && key.chars().all(|c| c.is_ascii_alphabetic()) {
            key.make_ascii_uppercase();
        } else {
            modifiers.push("Shift");
        }
    } else if shift {
        modifiers.push("Shift");
    }
    modifiers.push(&key);
    Some(modifiers.join("+"))
}

pub fn hotkey_section(hotkeys: &HashMap<String, String>) -> String {
    let mut rows: Vec<_> = hotkeys.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    rows.into_iter()
        .filter(|(action, _)| action.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .filter_map(|(action, combo)| {
            combo_to_mpv(combo).map(|key| {
                let mut keys = vec![key.clone()];
                // Cocoa VO 按当前键盘布局上报 charactersIgnoringModifiers。macOS
                // 拼音布局会把裸方括号上报为全角【/】，而 Shift 后的 {/} 正常，
                // 所以 AB 设置失效但取消正常。为无 Shift 方括号同时绑定全角别名。
                let has_shift = combo
                    .split('+')
                    .any(|part| part.eq_ignore_ascii_case("shift"));
                if !has_shift {
                    match combo.split('+').next_back() {
                        Some("[") => keys.push(format!("{}【", &key[..key.len() - 1])),
                        Some("]") => keys.push(format!("{}】", &key[..key.len() - 1])),
                        _ => {}
                    }
                }
                keys.into_iter()
                    .map(|key| format!("{key} script-message loopsub-hotkey {action}"))
                    .collect::<Vec<_>>()
            })
        })
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
}

/// 系统动态库裸名兜底。
pub fn system_dll_name() -> &'static str {
    #[cfg(windows)]
    return "libmpv-2.dll";
    #[cfg(target_os = "macos")]
    return "libmpv.dylib";
    #[cfg(all(unix, not(target_os = "macos")))]
    return "libmpv.so.2";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_browser_combos_to_mpv_keys() {
        assert_eq!(combo_to_mpv("Space").as_deref(), Some("SPACE"));
        assert_eq!(combo_to_mpv("alt+shift+ArrowLeft").as_deref(), Some("Alt+Shift+LEFT"));
        assert_eq!(combo_to_mpv("ctrl+[").as_deref(), Some("Ctrl+["));
        // mpv 会忽略文本键显式 Shift；必须绑定实际产生的花括号。
        assert_eq!(combo_to_mpv("shift+[").as_deref(), Some("{"));
        assert_eq!(combo_to_mpv("meta+k").as_deref(), Some("Meta+k"));
    }

    #[test]
    fn configured_prefix_lib_is_found() {
        let root = std::env::temp_dir().join(format!(
            "loopsub-libmpv-prefix-{}",
            std::process::id()
        ));
        let lib = root.join("lib").join(system_dll_name());
        std::fs::create_dir_all(lib.parent().unwrap()).unwrap();
        std::fs::write(&lib, b"fake").unwrap();
        assert_eq!(resolve_dll(Some(root.clone())), Some(lib));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn hotkey_section_is_stable_and_rejects_injected_actions() {
        let mut hotkeys = HashMap::new();
        hotkeys.insert("toggle_pause".into(), "Space".into());
        hotkeys.insert("bad\naction".into(), "x".into());
        assert_eq!(
            hotkey_section(&hotkeys),
            "SPACE script-message loopsub-hotkey toggle_pause"
        );
    }

    #[test]
    fn hotkey_section_adds_macos_pinyin_bracket_aliases() {
        let hotkeys = HashMap::from([
            ("ab_set_a".into(), "[".into()),
            ("ab_nudge_b_back".into(), "ctrl+]".into()),
            ("ab_clear_a".into(), "shift+[".into()),
        ]);
        let section = hotkey_section(&hotkeys);
        assert!(section.contains("[ script-message loopsub-hotkey ab_set_a"));
        assert!(section.contains("【 script-message loopsub-hotkey ab_set_a"));
        assert!(section.contains("Ctrl+】 script-message loopsub-hotkey ab_nudge_b_back"));
        assert!(section.contains("{ script-message loopsub-hotkey ab_clear_a"));
        assert!(!section.contains("Shift+【"));
    }
}
