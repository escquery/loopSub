//! 资源管理器右键菜单（"用 loopSub 播放"视频文件）。
//!
//! 挂在 HKCU\Software\Classes\SystemFileAssociations\video 下：按感知类型
//! 覆盖全部视频扩展名（.mp4/.mkv/...，Win7+），无需逐个注册；HKCU 免管理员，
//! 便携版/安装版通用。注册表即真相（状态不写入 settings.json）。

// HKEY 由 Registry 模块本身定义（glob 导入带出），不要从 Foundation 导
use windows_sys::Win32::System::Registry::*;

const SHELL_KEY: &str = r"Software\Classes\SystemFileAssociations\video\shell\loopSub";
const LABEL: &str = "用 loopSub 播放";

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// rc 为 LSTATUS（i32），0 = ERROR_SUCCESS
fn create_key(path: &str) -> Result<HKEY, String> {
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            to_wide(path).as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(format!("RegCreateKeyExW {path} 失败: {rc}"));
    }
    Ok(hkey)
}

fn set_value(hkey: HKEY, name: Option<&str>, value: &str) -> Result<(), String> {
    let wide = to_wide(value);
    let name_w = name.map(to_wide);
    let rc = unsafe {
        RegSetValueExW(
            hkey,
            name_w.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
            0,
            REG_SZ,
            wide.as_ptr() as *const u8,
            (wide.len() * 2) as u32,
        )
    };
    if rc != 0 {
        return Err(format!("RegSetValueExW 失败: {rc}"));
    }
    Ok(())
}

fn close(hkey: HKEY) {
    unsafe { RegCloseKey(hkey) };
}

/// 注册右键菜单：菜单文字 + 图标指向当前 exe，command 传 "%1"
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.display().to_string();
    let shell = create_key(SHELL_KEY)?;
    let r = (|| {
        set_value(shell, None, LABEL)?;
        set_value(shell, Some("Icon"), &exe)?;
        let cmd = create_key(&format!(r"{SHELL_KEY}\command"))?;
        let r2 = set_value(cmd, None, &format!("\"{exe}\" \"%1\""));
        close(cmd);
        r2
    })();
    close(shell);
    r
}

/// 移除右键菜单（键不存在视为成功）
pub fn unregister() -> Result<(), String> {
    let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, to_wide(SHELL_KEY).as_ptr()) };
    // 2 = ERROR_FILE_NOT_FOUND
    if rc != 0 && rc != 2 {
        return Err(format!("RegDeleteTreeW 失败: {rc}"));
    }
    Ok(())
}

pub fn is_registered() -> bool {
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            to_wide(SHELL_KEY).as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if rc == 0 {
        close(hkey);
        true
    } else {
        false
    }
}
