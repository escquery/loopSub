//! macOS 单窗口视频层：libmpv Render API 输出到 AppKit `NSOpenGLView`。
//!
//! mpv 更新回调只唤醒专用渲染线程；该线程独占 OpenGL context 和全部
//! `mpv_render_*` 调用，避免阻塞 AppKit/WKWebView 主线程。窗口布局仍只在
//! AppKit 主线程执行，并通过同一把 GL 锁与绘制串行。

#![allow(deprecated)] // NSOpenGLView 在 macOS 12 可用，是 mpv Render API 的稳定后端。

use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSOpenGLContext, NSOpenGLContextParameter, NSOpenGLView, NSWindow, NSWindowOrderingMode,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use tauri::Manager;

pub type RenderContext = *mut c_void;

const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;
const MPV_RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct OpenGlInitParams {
    get_proc_address: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void>,
    get_proc_address_ctx: *mut c_void,
    extra_exts: *const c_char,
}

#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

pub type UpdateCallback = Option<unsafe extern "C" fn(*mut c_void)>;

#[derive(Clone, Copy)]
pub struct RenderFns {
    pub create: unsafe extern "C" fn(*mut RenderContext, *mut c_void, *mut c_void) -> c_int,
    pub set_update_callback: unsafe extern "C" fn(RenderContext, UpdateCallback, *mut c_void),
    pub update: unsafe extern "C" fn(RenderContext) -> u64,
    pub render: unsafe extern "C" fn(RenderContext, *mut c_void) -> c_int,
    pub report_swap: unsafe extern "C" fn(RenderContext),
    pub free: unsafe extern "C" fn(RenderContext),
}

#[derive(Default)]
struct RenderWork {
    pending: bool,
    force: bool,
    closing: bool,
}

struct RenderState {
    fns: RenderFns,
    gl_flush: unsafe extern "C" fn(),
    render_context: usize,
    view: usize,
    gl_context: usize,
    width: AtomicI32,
    height: AtomicI32,
    gl_lock: Mutex<()>,
    work: Mutex<RenderWork>,
    wake: Condvar,
}

// AppKit objects are only dereferenced on the AppKit thread, except NSOpenGLContext's
// documented thread-safe rendering methods on the dedicated GL thread. Raw addresses are
// required because MainThreadOnly Retained<T> cannot live in Tauri's Send + Sync AppState.
unsafe impl Send for RenderState {}
unsafe impl Sync for RenderState {}

struct RenderWake {
    state: Arc<RenderState>,
}

pub struct MacRenderer {
    app: tauri::AppHandle,
    state: Arc<RenderState>,
    wake: Box<RenderWake>,
    render_thread: Mutex<Option<JoinHandle<()>>>,
    // mpv may resolve GL entry points for the lifetime of the RenderContext.
    _gl_library: Box<libloading::Library>,
}

unsafe impl Send for MacRenderer {}
unsafe impl Sync for MacRenderer {}

unsafe extern "C" fn get_proc_address(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    let library = unsafe { &*(ctx as *const libloading::Library) };
    let name = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    match unsafe { library.get::<unsafe extern "C" fn()>(name) } {
        Ok(symbol) => *symbol as *const () as *mut c_void,
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn update_callback(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    let wake = unsafe { &*(ctx as *const RenderWake) };
    if let Ok(mut work) = wake.state.work.lock() {
        if !work.closing {
            work.pending = true;
            wake.state.wake.notify_one();
        }
    }
}

fn render_loop(state: Arc<RenderState>) {
    loop {
        let force = {
            let mut work = state.work.lock().unwrap();
            while !work.pending && !work.closing {
                work = state.wake.wait(work).unwrap();
            }
            if work.closing {
                break;
            }
            work.pending = false;
            std::mem::take(&mut work.force)
        };
        render_frame(&state, force);
    }

    // RenderContext 必须在 mpv core 之前释放，且释放时同一 GL context 必须 current。
    let _gl_guard = state.gl_lock.lock().unwrap();
    unsafe {
        let gl = &*(state.gl_context as *const NSOpenGLContext);
        gl.makeCurrentContext();
        (state.fns.free)(state.render_context as RenderContext);
        NSOpenGLContext::clearCurrentContext();
    }
}

fn render_frame(state: &RenderState, force: bool) {
    let width = state.width.load(Ordering::Acquire);
    let height = state.height.load(Ordering::Acquire);
    if width <= 0 || height <= 0 {
        return;
    }

    let _gl_guard = state.gl_lock.lock().unwrap();
    unsafe {
        let gl = &*(state.gl_context as *const NSOpenGLContext);
        gl.makeCurrentContext();
        let render_ctx = state.render_context as RenderContext;
        let flags = (state.fns.update)(render_ctx);
        if !force && flags & MPV_RENDER_UPDATE_FRAME == 0 {
            NSOpenGLContext::clearCurrentContext();
            return;
        }

        let mut fbo = OpenGlFbo {
            fbo: 0,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip_y: c_int = 1;
        let mut params = [
            RenderParam {
                kind: MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            RenderParam {
                kind: MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip_y as *mut _ as *mut c_void,
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let code = (state.fns.render)(render_ctx, params.as_mut_ptr() as *mut c_void);
        if code < 0 {
            eprintln!("[render-macos] mpv_render_context_render failed: {code}");
            NSOpenGLContext::clearCurrentContext();
            return;
        }
        // NSOpenGLView::defaultPixelFormat 在 Monterey 上可能是单缓冲；仅调用
        // flushBuffer 不会提交画面，必须显式 glFlush。
        (state.gl_flush)();
        gl.flushBuffer();
        (state.fns.report_swap)(render_ctx);
        NSOpenGLContext::clearCurrentContext();
    }
}

impl MacRenderer {
    pub fn create(
        fns: RenderFns,
        mpv: *mut c_void,
        app: &tauri::AppHandle,
    ) -> Result<Self, String> {
        let gl_library = Box::new(unsafe {
            libloading::Library::new("/System/Library/Frameworks/OpenGL.framework/OpenGL")
                .map_err(|e| format!("加载 macOS OpenGL.framework 失败: {e}"))?
        });
        let gl_flush = unsafe {
            *gl_library
                .get::<unsafe extern "C" fn()>(b"glFlush\0")
                .map_err(|e| e.to_string())?
        };
        let gl_library_id = &*gl_library as *const libloading::Library as usize;
        let webview_window = app.get_webview_window("main").ok_or("主窗口不存在")?;
        let window_id = webview_window.ns_window().map_err(|e| e.to_string())? as usize;
        let mpv_id = mpv as usize;
        let (tx, rx) = std::sync::mpsc::channel();

        app.run_on_main_thread(move || unsafe {
            let mtm = MainThreadMarker::new().expect("已在 AppKit 主线程");
            let window = &*(window_id as *const NSWindow);
            let Some(content) = window.contentView() else {
                let _ = tx.send(Err("主窗口没有 contentView".to_string()));
                return;
            };
            let pixel_format = NSOpenGLView::defaultPixelFormat(mtm);
            let Some(view) = NSOpenGLView::initWithFrame_pixelFormat(
                NSOpenGLView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
                Some(&pixel_format),
            ) else {
                let _ = tx.send(Err("创建 NSOpenGLView 失败".to_string()));
                return;
            };
            view.setWantsBestResolutionOpenGLSurface(true);
            content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Above, None);
            view.prepareOpenGL();
            let Some(gl) = view.openGLContext() else {
                view.removeFromSuperview();
                let _ = tx.send(Err("创建 NSOpenGLContext 失败".to_string()));
                return;
            };
            gl.makeCurrentContext();
            // Wry 的 WKWebView 使用 CoreAnimation 合成层；OpenGL surface 必须显式
            // 排在它上面，再由透明 CSS 画面区透出。
            let mut surface_order: i32 = 1;
            gl.setValues_forParameter(
                std::ptr::NonNull::from(&mut surface_order),
                NSOpenGLContextParameter::SurfaceOrder,
            );
            let mut surface_opacity: i32 = 1;
            gl.setValues_forParameter(
                std::ptr::NonNull::from(&mut surface_opacity),
                NSOpenGLContextParameter::SurfaceOpacity,
            );
            gl.update(mtm);

            let mut init = OpenGlInitParams {
                get_proc_address: Some(get_proc_address),
                get_proc_address_ctx: gl_library_id as *mut c_void,
                extra_exts: std::ptr::null(),
            };
            let api_type = b"opengl\0";
            let mut params = [
                RenderParam {
                    kind: MPV_RENDER_PARAM_API_TYPE,
                    data: api_type.as_ptr() as *mut c_void,
                },
                RenderParam {
                    kind: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: &mut init as *mut _ as *mut c_void,
                },
                RenderParam {
                    kind: 0,
                    data: std::ptr::null_mut(),
                },
            ];
            let mut render_ctx: RenderContext = std::ptr::null_mut();
            let code = (fns.create)(
                &mut render_ctx,
                mpv_id as *mut c_void,
                params.as_mut_ptr() as *mut c_void,
            );
            if code < 0 || render_ctx.is_null() {
                view.clearGLContext();
                view.removeFromSuperview();
                let _ = tx.send(Err(format!(
                    "创建 libmpv OpenGL RenderContext 失败: {code}"
                )));
                return;
            }
            NSOpenGLContext::clearCurrentContext();
            let view_id = &*view as *const NSOpenGLView as usize;
            let gl_id = &*gl as *const NSOpenGLContext as usize;
            let _ = tx.send(Ok((render_ctx as usize, view_id, gl_id)));
        })
        .map_err(|e| e.to_string())?;

        let (render_context, view, gl_context) = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "创建 macOS OpenGL 视频层超时".to_string())??;
        let state = Arc::new(RenderState {
            fns,
            gl_flush,
            render_context,
            view,
            gl_context,
            width: AtomicI32::new(1),
            height: AtomicI32::new(1),
            gl_lock: Mutex::new(()),
            work: Mutex::new(RenderWork::default()),
            wake: Condvar::new(),
        });
        let wake = Box::new(RenderWake {
            state: state.clone(),
        });
        let thread_state = state.clone();
        let render_thread = std::thread::spawn(move || render_loop(thread_state));
        unsafe {
            (fns.set_update_callback)(
                render_context as RenderContext,
                Some(update_callback),
                &*wake as *const RenderWake as *mut c_void,
            );
        }
        Ok(Self {
            app: app.clone(),
            state,
            wake,
            render_thread: Mutex::new(Some(render_thread)),
            _gl_library: gl_library,
        })
    }

    /// 按 Wry 容器的真实 bounds 布局，避开标题栏尺寸被 Tauri inner_size 计入后
    /// 造成的纵向偏差。
    pub fn set_layout(&self, drawer_width: f64, top_height: f64, bottom_height: f64) {
        let state = self.state.clone();
        let _ = self.app.run_on_main_thread(move || unsafe {
            let closing = state.work.lock().map(|work| work.closing).unwrap_or(true);
            if closing {
                return;
            }
            let mtm = MainThreadMarker::new().expect("已在 AppKit 主线程");
            let view = &*(state.view as *const NSOpenGLView);
            let gl = &*(state.gl_context as *const NSOpenGLContext);
            let Some(parent) = view.superview() else {
                return;
            };
            let bounds = parent.bounds();
            // Wry 根 view 覆盖整个 NSWindow（含标题栏），原生视频层需额外避开
            // contentLayoutRect 之外的标题区域。
            let title_height = view
                .window()
                .map(|window| {
                    (bounds.size.height - window.contentLayoutRect().size.height).max(0.0)
                })
                .unwrap_or(0.0);
            let top_reserved = top_height + title_height;
            let y = if parent.isFlipped() {
                top_reserved
            } else {
                bottom_height
            };

            let _gl_guard = state.gl_lock.lock().unwrap();
            view.setFrame(NSRect::new(
                NSPoint::new(0.0, y),
                NSSize::new(
                    (bounds.size.width - drawer_width).max(1.0),
                    (bounds.size.height - top_reserved - bottom_height).max(1.0),
                ),
            ));
            gl.update(mtm);
            let backing = view.convertRectToBacking(view.bounds());
            state
                .width
                .store(backing.size.width.round() as i32, Ordering::Release);
            state
                .height
                .store(backing.size.height.round() as i32, Ordering::Release);
            drop(_gl_guard);

            // resize/1:1 在暂停时未必触发 mpv callback，强制渲染上一帧。
            if let Ok(mut work) = state.work.lock() {
                work.force = true;
                work.pending = true;
                state.wake.notify_one();
            }
        });
    }

    pub fn shutdown(&mut self) {
        unsafe {
            (self.state.fns.set_update_callback)(
                self.state.render_context as RenderContext,
                None,
                std::ptr::null_mut(),
            );
        }
        if let Ok(mut work) = self.state.work.lock() {
            work.closing = true;
            self.state.wake.notify_one();
        }
        if let Some(thread) = self.render_thread.lock().unwrap().take() {
            let _ = thread.join();
        }

        let state = self.state.clone();
        let detach = move || detach_view_on_main(state);
        if MainThreadMarker::new().is_some() {
            detach();
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            if self
                .app
                .run_on_main_thread(move || {
                    detach();
                    let _ = tx.send(());
                })
                .is_ok()
            {
                let _ = rx.recv_timeout(Duration::from_secs(2));
            }
        }
        // Keep callback storage alive through callback removal and render thread join.
        let _ = &self.wake;
    }
}

fn detach_view_on_main(state: Arc<RenderState>) {
    let _mtm = MainThreadMarker::new().expect("OpenGL 销毁必须在 AppKit 主线程");
    unsafe {
        let view = &*(state.view as *const NSOpenGLView);
        view.clearGLContext();
        view.removeFromSuperview();
    }
}
