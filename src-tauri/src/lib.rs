//! loopSub：字幕驱动的美剧学习面板（遥控 mpv）。
//! 设计依据见仓库根目录 DESIGN.md。

pub mod cache;
pub mod media;
pub mod mpv;
pub mod opensub;
pub mod settings;
pub mod subtitle;
pub mod translate;
mod winctl;

use std::path::PathBuf;
use std::sync::Mutex;

use mpv::MpvIpc;
use settings::Settings;
use tauri::{Emitter, Manager};

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

    /// LLM 客户端（惰性校验：首次翻译时才检查配置）
    fn llm_client(&self) -> Result<translate::llm::LlmClient, String> {
        let llm = self.settings.lock().unwrap().llm.clone();
        let base = llm.base_url.filter(|s| !s.is_empty())
            .ok_or("未配置大模型 Base URL，请到设置页填写")?;
        let model = llm.model.filter(|s| !s.is_empty())
            .ok_or("未配置模型名，请到设置页填写")?;
        Ok(translate::llm::LlmClient::new(
            &base,
            &model,
            llm.api_key.as_deref().unwrap_or(""),
        ))
    }

    /// OpenSubtitles 客户端（惰性校验：首次搜索时才检查 key）
    fn os_client(&self) -> Result<opensub::OsClient, String> {
        let key = self.settings.lock().unwrap().opensubtitles.api_key.clone();
        let key = key.filter(|s| !s.is_empty())
            .ok_or("未配置 OpenSubtitles API Key，请到设置页填写")?;
        Ok(opensub::OsClient::new(&key))
    }

    /// 当前模型名（用于译文缓存文件名；路径不安全字符替换掉）
    fn model_slug(&self) -> String {
        self.settings
            .lock()
            .unwrap()
            .llm
            .model
            .clone()
            .unwrap_or_else(|| "default".into())
            .replace(['/', '\\', ':'], "_")
    }

    fn llm_tuning(&self) -> (i64, usize, usize) {
        let llm = &self.settings.lock().unwrap().llm;
        (
            (llm.scene_threshold_s * 1000.0) as i64,
            llm.min_batch_lines as usize,
            llm.max_batch_lines as usize,
        )
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
/// 文本字幕到缓存目录；最后拉起 mpv 播放。无内嵌文本轨时降级为 notice，
/// 由前端走 OpenSubtitles 搜索流程。
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

    let mut notice = None;
    let source = if original.exists() {
        "cache"
    } else {
        let v = video.clone();
        let out = original.clone();
        let extracted = tokio::task::spawn_blocking(move || -> Result<bool, String> {
            let tracks = media::probe_subtitles(&v).map_err(|e| e.to_string())?;
            match media::pick_text_track(&tracks) {
                Some(track) => {
                    media::extract_subtitle(&v, track.index, &out).map_err(|e| e.to_string())?;
                    Ok(true)
                }
                None => Ok(false),
            }
        })
        .await
        .map_err(|e| e.to_string())??;
        if extracted {
            "embedded"
        } else {
            notice = Some("无内嵌文本字幕轨，可尝试 OpenSubtitles 搜索".to_string());
            "none"
        }
    };

    let mut lines = Vec::new();
    if source != "none" {
        let content = std::fs::read_to_string(&original).map_err(|e| e.to_string())?;
        lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
        if state.rule_truecase() {
            for line in &mut lines {
                line.text = subtitle::truecase(&line.text);
            }
        }
    }

    // 拉起 mpv 并播放（无字幕也先播，等用户搜索）；面板渲染模式下关掉 mpv 自带字幕
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
        notice,
    })
}

// ---------- OpenSubtitles 搜索 ----------

/// 搜索字幕：moviehash 精确匹配 → 文件名解析回退（剧名+SxxExx）
#[tauri::command]
async fn search_subtitles(
    state: tauri::State<'_, AppState>,
    video_path: String,
) -> Result<Vec<opensub::SubCandidate>, String> {
    let client = state.os_client()?;
    let video = PathBuf::from(&video_path);

    let hash = cache::moviehash(&video).map_err(|e| e.to_string())?;
    let by_hash = client
        .search_by_hash(&format!("{hash:016x}"), "en")
        .await
        .map_err(|e| e.to_string())?;
    if !by_hash.is_empty() {
        return Ok(by_hash);
    }

    let name = video
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let info = opensub::parse_filename(&name);
    client
        .search_by_query(&info, "en")
        .await
        .map_err(|e| e.to_string())
}

/// 下载选中的字幕 → 存入缓存 originals/ → 解析返回（并应用 truecase）
#[tauri::command]
async fn download_subtitle(
    state: tauri::State<'_, AppState>,
    video_hash: String,
    file_id: u64,
) -> Result<Vec<subtitle::SubtitleLine>, String> {
    let client = state.os_client()?;
    let hash = u64::from_str_radix(&video_hash, 16).map_err(|e| e.to_string())?;
    let content = client.download(file_id).await.map_err(|e| e.to_string())?;

    let cache = state.cache();
    cache.ensure_dirs().map_err(|e| e.to_string())?;
    std::fs::write(cache.original_path(hash), &content).map_err(|e| e.to_string())?;

    let mut lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
    if state.rule_truecase() {
        for line in &mut lines {
            line.text = subtitle::truecase(&line.text);
        }
    }
    Ok(lines)
}

// ---------- LLM 翻译 ----------

/// 整集翻译：缓存命中直接返回；否则跑完整管线，进度经 translate-progress 事件推送
#[tauri::command]
async fn translate_subtitles(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    video_hash: String,
    force: bool,
) -> Result<usize, String> {
    let client = state.llm_client()?;
    let hash = u64::from_str_radix(&video_hash, 16).map_err(|e| e.to_string())?;
    let cache = state.cache();
    let model = state.model_slug();
    let out_path = cache.translated_path(hash, &model);

    if out_path.exists() && !force {
        let content = std::fs::read_to_string(&out_path).map_err(|e| e.to_string())?;
        return Ok(subtitle::parse_srt(&content).map_err(|e| e.to_string())?.len());
    }

    // 原文（与展示层一致，应用 truecase）
    let original = cache.original_path(hash);
    let content = std::fs::read_to_string(&original)
        .map_err(|_| "缓存中没有原文，请先加载视频或搜索字幕".to_string())?;
    let mut lines = subtitle::parse_srt(&content).map_err(|e| e.to_string())?;
    if state.rule_truecase() {
        for line in &mut lines {
            line.text = subtitle::truecase(&line.text);
        }
    }

    let (scene_ms, min_b, max_b) = state.llm_tuning();
    let app2 = app.clone();
    let outcome = translate::translate_all(&client, &lines, scene_ms, min_b, max_b, move |p| {
        let _ = app2.emit("translate-progress", &p);
    })
    .await;

    // 用原文时间轴 + 译文写出 SRT 缓存
    let mut srt = String::new();
    for line in &lines {
        if let Some(zh) = outcome.translations.get(&line.number) {
            srt.push_str(&format!(
                "{}\n{} --> {}\n{}\n\n",
                line.number,
                fmt_ts(line.start_ms),
                fmt_ts(line.end_ms),
                zh
            ));
        }
    }
    std::fs::write(&out_path, &srt).map_err(|e| e.to_string())?;
    Ok(outcome.translations.len())
}

fn fmt_ts(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// 读取已缓存的译文（number -> 中文），无缓存返回空表
#[tauri::command]
fn get_translation(
    state: tauri::State<'_, AppState>,
    video_hash: String,
) -> Result<std::collections::BTreeMap<u32, String>, String> {
    let hash = u64::from_str_radix(&video_hash, 16).map_err(|e| e.to_string())?;
    let path = state.cache().translated_path(hash, &state.model_slug());
    let mut map = std::collections::BTreeMap::new();
    if let Ok(content) = std::fs::read_to_string(&path) {
        if let Ok(lines) = subtitle::parse_srt(&content) {
            for l in lines {
                map.insert(l.number, l.text);
            }
        }
    }
    Ok(map)
}

// ---------- 窗口行为 ----------

/// 召回 mpv 窗口：提升到普通窗口层顶部（面板 always-on-top 仍在它上面）
#[tauri::command]
fn recall_mpv(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let pid = state.mpv_child.lock().unwrap().as_ref().map(|c| c.id());
    match pid {
        Some(p) => winctl::recall_window_by_pid(p),
        None => Err("mpv 未由本程序拉起，无法召回".into()),
    }
}

#[tauri::command]
fn set_always_on_top(window: tauri::Window, flag: bool) -> Result<(), String> {
    window.set_always_on_top(flag).map_err(|e| e.to_string())
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
            search_subtitles,
            download_subtitle,
            translate_subtitles,
            get_translation,
            recall_mpv,
            set_always_on_top,
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
