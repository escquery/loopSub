//! 召回 mpv 窗口：切回面板时把 mpv 提升到普通窗口层顶部
//! （面板 always-on-top 仍在它上面）。macOS 实现（AX API）后续补充。

#[cfg(windows)]
pub fn recall_window_by_pid(pid: u32) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, SetWindowPos, HWND_TOP,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    struct Ctx {
        pid: u32,
        hwnds: Vec<HWND>,
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        let ctx = &mut *(lparam as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == ctx.pid && IsWindowVisible(hwnd) != 0 {
            ctx.hwnds.push(hwnd);
        }
        1
    }

    unsafe {
        let mut ctx = Ctx {
            pid,
            hwnds: Vec::new(),
        };
        EnumWindows(Some(enum_cb), &mut ctx as *mut Ctx as LPARAM);
        for hwnd in ctx.hwnds {
            SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn recall_window_by_pid(_pid: u32) -> Result<(), String> {
    Err("当前平台暂不支持召回（Windows 用 SetWindowPos，macOS 将用 AX API）".into())
}
