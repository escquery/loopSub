//! 外部二进制查找与进程拉起工具。
//!
//! Windows 上 mpv/ffmpeg 是绿色软件（解压即用），查找顺序：
//! 1. 设置页指定目录（用户显式配置，最高优先级）
//! 2. PATH（用户自行安装）
//! 3. 应用所在目录（随包携带：sidecar / resources 释放）
//! 都找不到时返回裸名字，让 spawn 错误信息自然暴露。

use std::path::{Path, PathBuf};

/// 解析外部二进制（mpv / ffmpeg / ffprobe）的完整路径
pub fn resolve(name: &str, configured_dir: Option<&Path>) -> PathBuf {
    #[cfg(windows)]
    let file_name = format!("{name}.exe");
    #[cfg(not(windows))]
    let file_name = name.to_string();

    if let Some(dir) = configured_dir {
        let p = dir.join(&file_name);
        if p.is_file() {
            return p;
        }
    }
    if in_path(&file_name) {
        return PathBuf::from(&file_name);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(&file_name);
            if p.is_file() {
                return p;
            }
        }
    }
    PathBuf::from(&file_name)
}

fn in_path(file_name: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| dir.join(file_name).is_file())
}

/// Windows 下抑制 spawn 时的控制台黑窗；其他平台无操作
pub fn no_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn configured_dir_wins() {
        let dir = std::env::temp_dir().join("loopsub_bins_test");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(windows)]
        let name = "mpv.exe";
        #[cfg(not(windows))]
        let name = "mpv";
        let fake = dir.join(name);
        std::fs::File::create(&fake).unwrap().write_all(b"x").unwrap();

        let got = resolve("mpv", Some(&dir));
        assert_eq!(got, fake);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn falls_back_to_bare_name_when_missing() {
        let dir = std::env::temp_dir().join("loopsub_bins_empty");
        std::fs::create_dir_all(&dir).unwrap();
        // 配置目录没有该二进制、PATH 里也不存在这个名字时，回退为裸名字
        let got = resolve("definitely-not-exist-bin-9f3k2", Some(&dir));
        #[cfg(windows)]
        assert_eq!(got, PathBuf::from("definitely-not-exist-bin-9f3k2.exe"));
        #[cfg(not(windows))]
        assert_eq!(got, PathBuf::from("definitely-not-exist-bin-9f3k2"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
