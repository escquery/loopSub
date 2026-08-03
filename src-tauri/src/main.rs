// 发布 Windows 版本时不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 必须在任何窗口创建之前：DPI 虚拟化会让 mpv 按逻辑像素建交换链，
    // 渲染面只占物理客户区的 1/scale（原型实测左上角一块黑）
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows_sys::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    loopsub_lib::run();
}
