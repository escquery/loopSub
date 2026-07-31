//! loopSub：字幕驱动的美剧学习面板（遥控 mpv）。
//! 设计依据见仓库根目录 DESIGN.md。

pub mod cache;
pub mod media;
pub mod mpv;
pub mod settings;
pub mod subtitle;
pub mod translate;

use std::path::PathBuf;
use std::sync::Mutex;

use mpv::MpvIpc;
use settings::Settings;
use tauri::Manager;

pub struct AppState {
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    mpv: tokio::sync::Mutex<Option<MpvIpc>>,
    mpv_child: Mutex<Option<std::process::Child>>,
}

impl AppState {
    fn cache(&self) -> cache::Cache {
        let root = self
            .settings
            .lock()
            .unwrap()
            .cache
            .dir
            .clone()
            .unwrap_or_else(|| {
                dirs::cache_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("loopsub")
            });
        cache::Cache::new(root)
    }

    fn rule_truecase(&self) -> bool {
        matches!(
            self.settings.lock().unwrap().subtitle.truecase,
            settings::TruecaseMode::Rule
        )
    }

    fn panel_render(&self) -> bool {
        matches!(
            self.settings.lock().unwrap().subtitle.render,
            settings::RenderMode::Panel
        )
    }

    fn audio(&self) -> settings::AudioSettings {
        self.settings.lock().unwrap().audio.clone()
    }
}

// ---------- 设置 ----------

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

// ---------- 字幕 ----------

/// 加载 SRT 字幕；规则法大写还原开启时对每句做 truecase
#[tauri::command]
fn load_srt(state: tauri::State<'_, AppState>, path: String) -> Result<Vec<subtitle::SubtitleLine>, String> {
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
    if state.rule_truecase() {
        for line in &mut lines {
            line.text = subtitle::truecase(&line.text);
        }
    }
    Ok(lines)
}

// ---------- mpv 生命周期 ----------

/// 拉起 mpv（若已连接则复用），并应用音频设置
async fn mpv_start_internal(state: &AppState) -> Result<(), String> {
    if state.mpv.lock().await.is_some() {
        return Ok(());
    }
    let endpoint = mpv::default_ipc_endpoint();
    let child = mpv::spawn_mpv(&endpoint).map_err(|e| format!("启动 mpv 失败（未安装？）: {e}"))?;
    let ipc = mpv::connect_with_retry(&endpoint, 30)
        .await
        .map_err(|e| format!("连接 mpv IPC 失败: {e}"))?;

    let audio = state.audio();
    if audio.dialogue_boost {
        let _ = ipc.set_property("af", "dynaudnorm".into()).await;
    }
    if let Some(vm) = audio.volume_max {
        let _ = ipc.set_property("volume-max", vm.into()).await;
    }

    *state.mpv.lock().await = Some(ipc);
    *state.mpv_child.lock().unwrap() = Some(child);
    Ok(())
}

#[tauri::command]
async fn mpv_start(state: tauri::State<'_, AppState>, video_path: Option<String>) -> Result<String, String> {
    mpv_start_internal(&state).await?;
    if let Some(vp) = video_path {
        let guard = state.mpv.lock().await;
        let ipc = guard.as_ref().unwrap();
        ipc.command(vec!["loadfile".into(), vp.into()])
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok("ok".into())
}

#[tauri::command]
async fn mpv_quit(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(ipc) = state.mpv.lock().await.take() {
        let _ = ipc.command(vec!["quit".into()]).await;
    }
    if let Some(mut child) = state.mpv_child.lock().unwrap().take() {
        let _ = child.kill();
    }
    Ok(())
}

/// 连接外部已运行的 mpv（手动模式）
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

// ---------- 视频加载管线 ----------

/// 加载视频：moviehash → 缓存命中直接用；否则 ffprobe 探测 + ffmpeg 提取内嵌
/// 文本字幕到缓存目录；最后拉起 mpv 播放。无内嵌文本轨时报错（搜索见 v1.5）。
#[tauri::command]
async fn load_video(state: tauri::State<'_, AppState>, path: String) -> Result<media::LoadVideoResult, String> {
    let video = PathBuf::from(&path);
    if !video.exists() {
        return Err("视频文件不存在".into());
    }
    let cache = state.cache();
    cache.ensure_dirs().map_err(|e| e.to_string())?;
    let hash = cache::moviehash(&video).map_err(|e| e.to_string())?;
    let original = cache.original_path(hash);

    let source = if original.exists() {
        "cache"
    } else {
        let v = video.clone();
        let out = original.clone();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            let tracks = media::probe_subtitles(&v).map_err(|e| e.to_string())?;
            let track = media::pick_text_track(&tracks)
                .ok_or_else(|| "无内嵌文本字幕轨（OpenSubtitles 搜索将在 v1.5 接入）".to_string())?;
            media::extract_subtitle(&v, track.index, &out).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())??;
        "embedded"
    };

    let content = std::fs::read_to_string(&original).map_err(|e| e.to_string())?;
    let mut lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
    if state.rule_truecase() {
        for line in &mut lines {
            line.text = subtitle::truecase(&line.text);
        }
    }

    // 拉起 mpv 并播放；面板渲染模式下关掉 mpv 自带字幕显示
    mpv_start_internal(&state).await?;
    {
        let guard = state.mpv.lock().await;
        let ipc = guard.as_ref().unwrap();
        ipc.command(vec!["loadfile".into(), path.clone().into()])
            .await
            .map_err(|e| e.to_string())?;
        if state.panel_render() {
            let _ = ipc.set_property("sub-visibility", false.into()).await;
        }
    }

    Ok(media::LoadVideoResult {
        lines,
        source: source.into(),
        video_hash: format!("{hash:016x}"),
        notice: None,
    })
}

pub fn run() {
    let settings_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("loopsub")
        .join("settings.json");
    let settings = Settings::load(&settings_path).unwrap_or_default();

    let app = tauri::Builder::default()
        .manage(AppState {
            settings: Mutex::new(settings),
            settings_path,
            mpv: tokio::sync::Mutex::new(None),
            mpv_child: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            load_srt,
            mpv_start,
            mpv_quit,
            mpv_connect,
            mpv_command,
            load_video,
        ])
        .build(tauri::generate_context!())
        .expect("error while building loopSub");

    // 退出时带走 mpv 子进程
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            if let Some(mut child) = handle.state::<AppState>().mpv_child.lock().unwrap().take() {
                let _ = child.kill();
            }
        }
    });
}
