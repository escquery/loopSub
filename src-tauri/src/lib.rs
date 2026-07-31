//! loopSub：字幕驱动的美剧学习面板（遥控 mpv）。
//! 设计依据见仓库根目录 DESIGN.md。

pub mod cache;
pub mod mpv;
pub mod settings;
pub mod subtitle;
pub mod translate;

use std::path::PathBuf;
use std::sync::Mutex;

use mpv::MpvIpc;
use settings::Settings;

pub struct AppState {
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    mpv: tokio::sync::Mutex<Option<MpvIpc>>,
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn save_settings(state: tauri::State<'_, AppState>, settings: Settings) -> Result<(), String> {
    settings.save(&state.settings_path).map_err(|e| e.to_string())?;
    *state.settings.lock().unwrap() = settings;
    Ok(())
}

/// 加载 SRT 字幕；规则法大写还原开启时对每句做 truecase
#[tauri::command]
fn load_srt(state: tauri::State<'_, AppState>, path: String) -> Result<Vec<subtitle::SubtitleLine>, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
    let rule_mode = matches!(
        state.settings.lock().unwrap().subtitle.truecase,
        settings::TruecaseMode::Rule
    );
    if rule_mode {
        for line in &mut lines {
            line.text = subtitle::truecase(&line.text);
        }
    }
    Ok(lines)
}

/// 连接 mpv IPC（Unix socket 路径或 Windows 命名管道）
#[tauri::command]
async fn mpv_connect(state: tauri::State<'_, AppState>, socket_path: String) -> Result<(), String> {
    let ipc = MpvIpc::connect(&socket_path).await.map_err(|e| e.to_string())?;
    *state.mpv.lock().await = Some(ipc);
    Ok(())
}

/// mpv 命令透传：args 为 JSON 数组，如 ["seek", 12.5, "absolute"]
#[tauri::command]
async fn mpv_command(
    state: tauri::State<'_, AppState>,
    args: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let guard = state.mpv.lock().await;
    let ipc = guard.as_ref().ok_or_else(|| "mpv not connected".to_string())?;
    ipc.command(args).await.map_err(|e| e.to_string())
}

pub fn run() {
    let settings_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("loopsub")
        .join("settings.json");
    let settings = Settings::load(&settings_path).unwrap_or_default();

    tauri::Builder::default()
        .manage(AppState {
            settings: Mutex::new(settings),
            settings_path,
            mpv: tokio::sync::Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            load_srt,
            mpv_connect,
            mpv_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running loopSub");
}
