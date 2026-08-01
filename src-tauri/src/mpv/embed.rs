//! libmpv 进程内嵌入（Phase A）：dlopen libmpv-2.dll，直接调 C API。
//! 视频窗口仍由 mpv 自己创建，行为与外部 mpv.exe 一致；变化的是通信方式：
//! 无命名管道连接/重试、无外部子进程、命令同步返回。
//!
//! 手动连接外部已运行 mpv 的模式仍走 [`crate::mpv::MpvIpc`]，不受影响。
//!
//! 线程模型：mpv handle 的 command/property API 多线程安全（core 内部加锁）；
//! mpv_wait_event 只能单线程调用——专用事件线程消费事件队列，并在收到
//! SHUTDOWN（用户直接关闭 mpv 窗口）时置 dead 标志供上层感知。

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use serde_json::Value;

// ---- mpv client.h 常量（ABI 稳定，见 mpv 文档"Client API changes"） ----
const MPV_FORMAT_STRING: c_int = 1;
const MPV_FORMAT_FLAG: c_int = 3;
const MPV_FORMAT_INT64: c_int = 4;
const MPV_FORMAT_DOUBLE: c_int = 5;
const MPV_EVENT_SHUTDOWN: c_int = 1;

/// 只需读首字段；后续字段按 client.h 原样排布以保证偏移正确
#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

type Handle = *mut c_void;

/// dlopen 得到的 C API 函数表；Library 随表持有，保证函数指针始终有效
pub struct MpvApi {
    _lib: libloading::Library,
    client_api_version: unsafe extern "C" fn() -> c_ulong,
    create: unsafe extern "C" fn() -> Handle,
    initialize: unsafe extern "C" fn(Handle) -> c_int,
    set_option: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    set_option_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    command: unsafe extern "C" fn(Handle, *mut *const c_char) -> c_int,
    command_string: unsafe extern "C" fn(Handle, *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(Handle, *const c_char, c_int, *mut c_void) -> c_int,
    get_property_string: unsafe extern "C" fn(Handle, *const c_char) -> *mut c_char,
    set_property_string: unsafe extern "C" fn(Handle, *const c_char, *const c_char) -> c_int,
    wait_event: unsafe extern "C" fn(Handle, f64) -> *mut MpvEvent,
    terminate_destroy: unsafe extern "C" fn(Handle),
    free: unsafe extern "C" fn(*mut c_void),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
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
        | "volume" | "volume-max" | "time-remaining" | "audio-delay" | "cache-speed" => {
            MPV_FORMAT_DOUBLE
        }
        "pause" | "sub-visibility" | "core-idle" | "eof-reached" | "mute" | "seeking"
        | "idle-active" | "paused-for-cache" => MPV_FORMAT_FLAG,
        "chapter" | "chapter-count" | "playlist-pos" | "playlist-count" | "edition" => {
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
    /// 自建视频窗口（Phase B）；创建或 wid 设置失败时降级为 mpv 自建窗口
    #[cfg(windows)]
    vidwin: Mutex<Option<super::vidwin::VideoWindow>>,
}

impl MpvEmbed {
    /// 创建并初始化实例（"缴械"选项与原外部 mpv 的 spawn 参数一致，见 DESIGN.md §2）
    pub fn new(api: Arc<MpvApi>) -> Result<Self, String> {
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err("mpv_create 失败".into());
        }
        for (k, v) in [
            ("idle", "yes"),
            ("force-window", "yes"),
            ("osc", "no"),
            ("input-default-bindings", "no"),
            ("keep-open", "yes"),
            ("sid", "no"),
        ] {
            let ck = CString::new(k).unwrap();
            let cv = CString::new(v).unwrap();
            api.check(unsafe { (api.set_option_string)(handle, ck.as_ptr(), cv.as_ptr()) })
                .map_err(|e| format!("设置 {k}={v} 失败: {e}"))?;
        }

        // Phase B：自建视频窗口 + wid 内嵌；窗口上点 X = 向 core 投递 quit
        //（与 mpv 自建窗口的默认行为一致）。失败则降级为 mpv 自建窗口。
        #[cfg(windows)]
        let vidwin = match super::vidwin::VideoWindow::create({
            let api = api.clone();
            let h = handle as usize;
            move || {
                let quit = CString::new("quit").unwrap();
                unsafe { (api.command_string)(h as Handle, quit.as_ptr()) };
            }
        }) {
            Ok(vw) => {
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
            }
            Err(_) => None,
        };

        if let Err(e) = api.check(unsafe { (api.initialize)(handle) }) {
            unsafe { (api.terminate_destroy)(handle) };
            return Err(format!("mpv_initialize 失败: {e}"));
        }

        // 事件线程：消费事件队列，侦测 SHUTDOWN（用户直接关了 mpv 窗口）。
        // wait_event 用 200ms 超时轮询而非永久阻塞：terminate_destroy 的 unblock
        // 承诺偶发失效（实测 join 永久卡死），超时+closing 标志保证 join 有硬上界。
        let dead = Arc::new(AtomicBool::new(false));
        let closing = Arc::new(AtomicBool::new(false));
        let event_thread = std::thread::spawn({
            let api = api.clone();
            let dead = dead.clone();
            let closing = closing.clone();
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
                    if !ev.is_null() && unsafe { (*ev).event_id } == MPV_EVENT_SHUTDOWN {
                        dead.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
        Ok(Self {
            api,
            handle: handle as usize,
            dead,
            closing,
            event_thread: Mutex::new(Some(event_thread)),
            #[cfg(windows)]
            vidwin: Mutex::new(vidwin),
        })
    }

    fn handle(&self) -> Handle {
        self.handle as Handle
    }

    /// mpv core 是否已退出（事件线程侦测）
    pub fn is_dead(&self) -> bool {
        self.dead.load(Ordering::SeqCst)
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

    /// 视频窗口句柄（Phase B 自建窗口；降级或非 Windows 为 None）
    #[cfg(windows)]
    pub fn hwnd(&self) -> Option<windows_sys::Win32::Foundation::HWND> {
        self.vidwin.lock().unwrap().as_ref().map(|v| v.hwnd())
    }

    /// 视频窗口句柄（Phase B 自建窗口；降级或非 Windows 为 None）
    #[cfg(not(windows))]
    pub fn hwnd(&self) -> Option<isize> {
        None
    }

    /// 销毁实例；幂等：core 已自行退出（用户关窗口）时仅清理句柄。
    /// 先置 closing 让事件线程在超时轮询内自行退出（join ≤200ms 有保证），
    /// 再 terminate_destroy 收尾；不依赖 wait_event 的 unblock 承诺。
    pub fn shutdown(&self) {
        eprintln!("[embed] shutdown: closing + terminate_destroy");
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
pub fn resolve_dll(settings_dir: Option<PathBuf>) -> Option<PathBuf> {
    #[cfg(windows)]
    const NAMES: [&str; 2] = ["libmpv-2.dll", "mpv-2.dll"];
    #[cfg(target_os = "macos")]
    const NAMES: [&str; 2] = ["libmpv.dylib", "libmpv.2.dylib"];
    #[cfg(all(unix, not(target_os = "macos")))]
    const NAMES: [&str; 2] = ["libmpv.so.2", "libmpv.so"];

    let mut dirs: Vec<PathBuf> = settings_dir.into_iter().collect();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
            dirs.push(dir.join("mpv"));
        }
    }
    dirs.iter()
        .flat_map(|d| NAMES.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// 系统安装的 libmpv 裸名（Linux 发行版仓库、macOS Homebrew）：
/// 走系统动态库搜索路径，作为 resolve_dll 找不到文件时的兜底
pub fn system_dll_name() -> &'static str {
    #[cfg(windows)]
    return "libmpv-2.dll";
    #[cfg(target_os = "macos")]
    return "libmpv.dylib";
    #[cfg(all(unix, not(target_os = "macos")))]
    return "libmpv.so.2";
}
