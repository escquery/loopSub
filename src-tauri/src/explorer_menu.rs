//! 资源管理器右键菜单（"用 loopSub 播放"视频文件）。
//!
//! 按扩展名注册到 HKCU\Software\Classes\SystemFileAssociations\.<ext>\shell\loopSub：
//! 该路径固定被资源管理器查询，不依赖 PerceivedType。只按感知类型注册
//!（video\shell）会失效——PotPlayer 等播放器在 HKCU 接管扩展名关联时不写
//! PerceivedType，覆盖 HKLM 合并视图后"视频"感知类型丢失，该路径永不被查询。
//! 另保留 video 感知类型键作为干净系统上未枚举格式的兜底。HKCU 免管理员，
//! 便携版/安装版通用。注册表即真相（状态不写入 settings.json）。

// HKEY 由 Registry 模块本身定义（glob 导入带出），不要从 Foundation 导
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Shell::SHChangeNotify;

/// 通知 Shell 文件关联已变：资源管理器缓存右键菜单，不写注册表后不发通知
/// 的话要重启 explorer/注销才生效
fn notify_shell() {
    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, 0, std::ptr::null(), std::ptr::null()) };
}

const LABEL: &str = "用 loopSub 播放";

/// 常见视频扩展名：逐一注册扩展名特定 verb。播放器接管关联后感知类型路径
/// 会失效（见模块注释），扩展名路径固定被查询，是唯一可靠的覆盖方式
const VIDEO_EXTS: &[&str] = &[
    ".mp4", ".mkv", ".avi", ".mov", ".wmv", ".flv", ".webm", ".m4v",
    ".ts", ".m2ts", ".mpg", ".mpeg", ".vob", ".3gp", ".rmvb", ".rm",
    ".asf", ".f4v", ".divx", ".ogv",
];

fn verb_key(base: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{base}\shell\loopSub")
}

/// 全部注册目标：video 感知类型键（兜底）+ 各视频扩展名键
fn all_keys() -> impl Iterator<Item = String> {
    std::iter::once(verb_key("video")).chain(VIDEO_EXTS.iter().map(|e| verb_key(e)))
}

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

/// 写入单个 verb：菜单文字 + 图标指向当前 exe，command 传 "%1"
fn write_verb(key: &str, exe: &str) -> Result<(), String> {
    let shell = create_key(key)?;
    let r = (|| {
        set_value(shell, None, LABEL)?;
        set_value(shell, Some("Icon"), exe)?;
        let cmd = create_key(&format!(r"{key}\command"))?;
        let r2 = set_value(cmd, None, &format!("\"{exe}\" \"%1\""));
        close(cmd);
        r2
    })();
    close(shell);
    r
}

/// 注册右键菜单到全部目标键（单个失败不中断其余，返回第一个错误）
pub fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.display().to_string();
    let mut first_err = None;
    for key in all_keys() {
        if let Err(e) = write_verb(&key, &exe) {
            if first_err.is_none() {
                first_err = Some(e);
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => {
            notify_shell();
            Ok(())
        }
    }
}

/// 移除右键菜单（键不存在视为成功，返回第一个真实错误）
pub fn unregister() -> Result<(), String> {
    let mut first_err = None;
    for key in all_keys() {
        let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, to_wide(&key).as_ptr()) };
        // 2 = ERROR_FILE_NOT_FOUND
        if rc != 0 && rc != 2 && first_err.is_none() {
            first_err = Some(format!("RegDeleteTreeW {key} 失败: {rc}"));
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => {
            notify_shell();
            Ok(())
        }
    }
}

pub fn is_registered() -> bool {
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            to_wide(&verb_key(".mp4")).as_ptr(),
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
