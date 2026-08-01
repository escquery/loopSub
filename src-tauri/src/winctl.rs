//! 召回 mpv 窗口：切回面板时把 mpv 提升到普通窗口层顶部
//! （面板 always-on-top 仍在它上面）。
//!
//! libmpv 进程内实例的视频窗口由 mpv 自建，窗口类名 "mpv"、属本进程；
//! 按"本进程 + 可见 + 类名 mpv"定位，避免误抬面板窗口（SetWindowPos
//! 的 HWND_TOP 会摘掉已有窗口的 topmost 属性）。macOS 实现后续补充。

#[cfg(windows)]
pub fn recall_mpv_window() -> Result<(), String> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible, SetWindowPos,
        HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };

    struct Ctx {
        pid: u32,
        found: bool,
    }

    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> i32 {
        let ctx = &mut *(lparam as *mut Ctx);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != ctx.pid || IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if n > 0 && String::from_utf16_lossy(&buf[..n as usize]) == "mpv" {
            SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            ctx.found = true;
        }
        1
    }

    let mut ctx = Ctx {
        pid: std::process::id(),
        found: false,
    };
    unsafe {
        EnumWindows(Some(enum_cb), &mut ctx as *mut Ctx as LPARAM);
    }
    if ctx.found {
        Ok(())
    } else {
        Err("未找到 mpv 窗口".into())
    }
}

/// 直接按句柄抬窗（Phase B 自建视频窗口，HWND 由 MpvEmbed 持有）
#[cfg(windows)]
pub fn raise_window(hwnd: windows_sys::Win32::Foundation::HWND) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    };
    unsafe {
        if SetWindowPos(
            hwnd,
            HWND_TOP,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        ) == 0
        {
            return Err("SetWindowPos 失败".into());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn raise_window(_hwnd: isize) -> Result<(), String> {
    Err("当前平台暂不支持召回".into())
}

#[cfg(not(windows))]
pub fn recall_mpv_window() -> Result<(), String> {
    Err("当前平台暂不支持召回（Windows 用 SetWindowPos，macOS 将用 AX API）".into())
}
