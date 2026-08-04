//! loopSub：字幕驱动的美剧学习面板（进程内 libmpv 播放）。
//! 设计依据见仓库根目录 DESIGN.md。

pub mod anki;
pub mod cache;
pub mod media;
pub mod mpv;
pub mod opensub;
pub mod settings;
pub mod subtitle;
pub mod sync;
pub mod translate;
mod bins;

use std::path::PathBuf;
use std::sync::Mutex;

use mpv::{Mpv, MpvIpc};
use settings::Settings;
use tauri::{Emitter, Manager};

pub struct AppState {
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    mpv: tokio::sync::Mutex<Option<Mpv>>,
    /// 当前加载视频的 hash（播放位置按它存；后端持有，避开 JS f64 存不下 u64 的精度问题）
    current_video: Mutex<Option<u64>>,
    /// 学习面板抽屉开合（Phase C 单窗口；视频区随它收窄/恢复）
    drawer_open: Mutex<bool>,
}

/// 单窗口布局常量（Phase C，Windows）：顶栏高 / 抽屉宽（逻辑像素）
#[cfg(windows)]
const BAR_H: f64 = 44.0;
/// 画面底部常驻进度条高度（DOM 绘制，视频窗口减高让位——进度条若叠在画面
/// 上会被 loopsub-video 子窗口挡住）
#[cfg(windows)]
const PROGRESS_H: f64 = 14.0;
#[cfg(windows)]
const DRAWER_W: f64 = 420.0;

/// 重排视频子窗口：顶栏以下、抽屉以左的区域（顺带抬顶——WebView2 异步初始化
/// 会把自己的子窗口压在 mpv 子窗口上面）。非嵌入形态（降级/顶层窗口）无操作。
#[cfg(windows)]
fn relayout_video(app: &tauri::AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let state = app.state::<AppState>();
    let Ok(scale) = win.scale_factor() else { return };
    let Ok(size) = win.inner_size() else { return };
    let bar_h = (BAR_H * scale).round() as i32;
    let drawer_w = if *state.drawer_open.lock().unwrap() {
        (DRAWER_W * scale).round() as i32
    } else {
        0
    };
    let (w, h) = (size.width as i32, size.height as i32);
    // 本函数在主线程执行（Resized/命令投递）：绝不阻塞等 mpv 锁——持锁方
    // （mpv_start_internal 创建子窗口）可能正在等主线程，blocking 会成死锁环；
    // 拿不到就跳过，下次 Resized/ready/drawer 再排。
    let Ok(guard) = state.mpv.try_lock() else { return };
    if let Some(Mpv::Embed(e)) = guard.as_ref() {
        let progress_h = (PROGRESS_H * scale).round() as i32;
        e.with_vidwin(|vw| {
            vw.set_rect(0, bar_h, (w - drawer_w).max(1), (h - bar_h - progress_h).max(1));
            vw.raise();
        });
    }
}

/// 前端形态：Windows = 单窗口（顶栏+抽屉+内嵌视频）；其余平台 = 面板+独立视频窗
#[tauri::command]
fn window_mode() -> &'static str {
    #[cfg(windows)]
    return "single";
    #[cfg(not(windows))]
    return "panel";
}

/// 抽屉开合：记录状态并重排视频区
#[tauri::command]
fn set_drawer(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    open: bool,
) -> Result<(), String> {
    *state.drawer_open.lock().unwrap() = open;
    #[cfg(windows)]
    {
        // 重排要做 SetWindowPos，必须在子窗口所属的主线程执行（command 跑在工作线程）
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || relayout_video(&app2));
    }
    Ok(())
}

/// 前端就绪（DOMContentLoaded）：WebView2 初始化完成后抬顶+重排视频子窗口
#[tauri::command]
fn webview_ready(app: tauri::AppHandle) {
    #[cfg(windows)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || relayout_video(&app2));
    }
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

    /// 解析外部二进制（设置目录 → PATH → 应用目录）
    fn bin(&self, name: &str) -> PathBuf {
        let dir = self.settings.lock().unwrap().bins.dir.clone();
        bins::resolve(name, dir.as_deref())
    }

    /// (场景阈值 ms, 最小批行数, 最大批行数, 并发批数)
    fn llm_tuning(&self) -> (i64, usize, usize, usize) {
        let llm = &self.settings.lock().unwrap().llm;
        (
            (llm.scene_threshold_s * 1000.0) as i64,
            llm.min_batch_lines as usize,
            llm.max_batch_lines as usize,
            (llm.concurrency as usize).max(1),
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

/// 拉起 mpv（libmpv 进程内实例；已存活则复用，已退出则重建），并应用音频设置。
/// Windows 单窗口形态：视频子窗口嵌入主窗口顶栏以下区域。
async fn mpv_start_internal(state: &AppState, app: &tauri::AppHandle) -> Result<(), String> {
    let mut guard = state.mpv.lock().await;
    if let Some(m) = guard.as_ref() {
        if !m.is_dead() {
            return Ok(());
        }
        // 用户直接关了 mpv 窗口：清理句柄后重建
        if let Some(Mpv::Embed(e)) = guard.take() {
            e.shutdown();
        }
    }
        // 设置目录/exe 目录找不到时，裸名走系统动态库搜索路径兜底
    // （Linux 发行版仓库、macOS Homebrew 安装的 libmpv）
    let api = match mpv::embed::resolve_dll(state.settings.lock().unwrap().bins.dir.clone()) {
        Some(dll) => mpv::embed::MpvApi::load(&dll)?,
        None => mpv::embed::MpvApi::load(std::path::Path::new(mpv::embed::system_dll_name()))
            .map_err(|e| format!("未找到 libmpv 动态库：可放至程序目录、系统安装，或在设置页指定外部程序目录（{e}）"))?,
    };

    // Windows：子窗口嵌入主窗口（Phase C 单窗口）；其余平台：mpv 自建顶层窗口
    #[cfg(windows)]
    let layout = {
        let win = app.get_webview_window("main").ok_or("主窗口不存在")?;
        let scale = win.scale_factor().map_err(|e| e.to_string())?;
        let size = win.inner_size().map_err(|e| e.to_string())?;
        let bar_h = (BAR_H * scale).round() as i32;
        let drawer_w = if *state.drawer_open.lock().unwrap() {
            (DRAWER_W * scale).round() as i32
        } else {
            0
        };
        let progress_h = (PROGRESS_H * scale).round() as i32;
        Some(mpv::embed::VidLayout {
            parent: win.hwnd().map_err(|e| e.to_string())?.0 as usize,
            x: 0,
            y: bar_h,
            w: (size.width as i32 - drawer_w).max(1),
            h: (size.height as i32 - bar_h - progress_h).max(1),
        })
    };
    #[cfg(not(windows))]
    let layout = None;

    let embed = mpv::embed::MpvEmbed::new(api, layout, app)?;
    let m = Mpv::Embed(embed);

    let audio = state.audio();
    if audio.dialogue_boost {
        let _ = m.set_property("af", "dynaudnorm".into()).await;
    }
    if let Some(vm) = audio.volume_max {
        let _ = m.set_property("volume-max", vm.into()).await;
    }

    *guard = Some(m);
    // 新建视频子窗口后抬顶+落位：WebView2 的渲染层会压在子窗口上面
    //（实测创建后黑屏，raise 一次才显示）。异步投递，闭包执行时本函数已还锁。
    #[cfg(windows)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || relayout_video(&app2));
    }
    Ok(())
}

#[tauri::command]
async fn mpv_start(app: tauri::AppHandle, state: tauri::State<'_, AppState>, video_path: Option<String>) -> Result<String, String> {
    mpv_start_internal(&state, &app).await?;
    if let Some(vp) = video_path {
        let guard = state.mpv.lock().await;
        let ipc = guard.as_ref().unwrap();
        ipc.command(vec!["loadfile".into(), vp.into()])
            .await
            .map_err(|e| e.to_string())?;
    }
    // 同 load_video：锁释放后抬顶+落位视频子窗口
    #[cfg(windows)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || relayout_video(&app2));
    }
    Ok("ok".into())
}

#[tauri::command]
async fn mpv_quit(state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(m) = state.mpv.lock().await.take() {
        match m {
            Mpv::Embed(e) => e.shutdown(),
            Mpv::Ipc(i) => {
                let _ = i.command(vec!["quit".into()]).await;
            }
        }
    }
    Ok(())
}

/// 连接外部已运行的 mpv（手动模式）
#[tauri::command]
async fn mpv_connect(state: tauri::State<'_, AppState>, socket_path: String) -> Result<(), String> {
    let ipc = MpvIpc::connect(&socket_path).await.map_err(|e| e.to_string())?;
    *state.mpv.lock().await = Some(Mpv::Ipc(ipc));
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
async fn load_video(app: tauri::AppHandle, state: tauri::State<'_, AppState>, path: String) -> Result<media::LoadVideoResult, String> {
    let video = PathBuf::from(&path);
    if !video.exists() {
        // 历史里的失效记录顺手剔除
        let _ = state.cache().remove_history(&path);
        return Err("视频文件不存在".into());
    }
    let cache = state.cache();
    cache.ensure_dirs().map_err(|e| e.to_string())?;
    let hash = cache::moviehash(&video).map_err(|e| e.to_string())?;
    *state.current_video.lock().unwrap() = Some(hash);
    let original = cache.original_path(hash);
    // 续播位置（≥5s 才生效，loadfile 时作为 start 选项下发）
    let resume = cache.load_video_config(hash).position_s;

    let mut notice = None;
    let source = if original.exists() {
        "cache"
    } else {
        let _ = app.emit("video-load-progress", "正在探测字幕轨…");
        let v = video.clone();
        let ffprobe = state.bin("ffprobe");
        let tracks = tokio::task::spawn_blocking(move || media::probe_subtitles(&v, &ffprobe))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let extracted = if let Some(track) = media::pick_text_track(&tracks) {
            let _ = app.emit("video-load-progress", "正在提取内嵌字幕（首次打开较慢）…");
            let v = video.clone();
            let out = original.clone();
            let ffmpeg = state.bin("ffmpeg");
            let idx = track.index;
            tokio::task::spawn_blocking(move || media::extract_subtitle(&v, idx, &out, &ffmpeg))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
            true
        } else {
            false
        };
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
            // 画面字幕同步用还原版（与句子列表一致）：落盘 truecased/ 供 sub-add
            // 挂载；写失败不致命，挂载处会回落 originals 原文
            let _ = std::fs::write(cache.truecased_path(hash), subtitle::to_srt(&lines));
        }
    }

    // 拉起 mpv 并播放（无字幕也先播，等用户搜索）；面板渲染模式下关掉 mpv 自带字幕
    let _ = app.emit("video-load-progress", "正在启动播放器…");
    mpv_start_internal(&state, &app).await?;
    {
        let guard = state.mpv.lock().await;
        let ipc = guard.as_ref().unwrap();
        let args = if resume >= 5.0 {
            vec![
                "loadfile".into(),
                path.clone().into(),
                "replace".into(),
                (-1).into(),
                // options 以 "key=value" 字符串下发，libmpv argv 与 JSON IPC 均如此解析
                format!("start={resume:.3}").into(),
            ]
        } else {
            vec!["loadfile".into(), path.clone().into()]
        };
        ipc.command(args).await.map_err(|e| e.to_string())?;
        // 面板形态（非 Windows）：字幕由面板 mini-bar 渲染，mpv 侧关字幕防双显
        #[cfg(not(windows))]
        if state.panel_render() {
            let _ = ipc.set_property("sub-visibility", false.into()).await;
        }
    }

    // 锁已释放：抬顶+落位视频子窗口（mpv_start_internal 的投递可能撞锁被跳过）
    #[cfg(windows)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || relayout_video(&app2));
    }

    // 单窗口形态（Windows）画面字幕交给 mpv 渲染（字幕条已退役、句子列表收在抽屉里）。
    // demuxer 异步打开，未就绪时 sub-add 会被拒（mpv error -12）：轮询 duration 至就绪
    // （上界 5s）再挂轨；每次循环锁内仅一次查询，sleep 在锁外不挡前端播放轮询
    #[cfg(windows)]
    if source != "none" {
        // 挂载路径：truecase 开启且有还原文件时用还原版，否则 originals 原文
        let sub_path = if state.rule_truecase() {
            let tc = cache.truecased_path(hash);
            if tc.exists() { tc } else { original.clone() }
        } else {
            original.clone()
        };
        let mut attached = false;
        for _ in 0..50 {
            {
                let guard = state.mpv.lock().await;
                if let Some(ipc) = guard.as_ref() {
                    if let Ok(v) = ipc.get_property("duration").await {
                        if v.as_f64().unwrap_or(0.0) > 0.0 {
                            let _ = ipc.set_property("sub-visibility", true.into()).await;
                            attached = ipc
                                .command(vec![
                                    "sub-add".into(),
                                    sub_path.to_string_lossy().into_owned().into(),
                                    "select".into(),
                                ])
                                .await
                                .is_ok();
                        }
                    }
                }
            }
            if attached {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // 成功加载后写入历史记录（MRU 置顶）
    let _ = cache.touch_history(&path);

    Ok(media::LoadVideoResult {
        lines,
        source: source.into(),
        video_hash: format!("{hash:016x}"),
        notice,
        resume_s: resume,
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
        // 画面字幕同步用还原版（同 load_video）；写失败则挂载处回落 originals
        let _ = std::fs::write(cache.truecased_path(hash), subtitle::to_srt(&lines));
    }
    // 单窗口形态（Windows）：正在播放时挂给 mpv 渲染画面字幕（此时 demuxer 已就绪）
    #[cfg(windows)]
    {
        let sub_path = if state.rule_truecase() {
            let tc = cache.truecased_path(hash);
            if tc.exists() { tc } else { cache.original_path(hash) }
        } else {
            cache.original_path(hash)
        };
        let guard = state.mpv.lock().await;
        if let Some(ipc) = guard.as_ref() {
            let _ = ipc.set_property("sub-visibility", true.into()).await;
            let _ = ipc
                .command(vec![
                    "sub-add".into(),
                    sub_path.to_string_lossy().into_owned().into(),
                    "select".into(),
                ])
                .await;
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

    let (scene_ms, min_b, max_b, conc) = state.llm_tuning();
    let fingerprint = translate::lines_fingerprint(&lines);
    let mut prog = cache.load_progress(hash, &model);
    if prog.fingerprint != fingerprint || force {
        // 换源字幕（行内容变化）或强制重翻：旧断点作废；行缓存仍生效
        prog = cache::ProgressFile {
            fingerprint,
            ..Default::default()
        };
    }
    let mut line_cache = translate::LineCache::load(cache.lines_path(&model));

    let prog = std::sync::Arc::new(std::sync::Mutex::new(prog));
    // 先取出续翻数据：锁守卫若留在 translate_all 实参表达式里，生命周期会延伸到
    // .await 语句尾，导致 MutexGuard 跨 await（std MutexGuard 非 Send）
    let resume = std::mem::take(&mut prog.lock().unwrap().batches);
    let prog2 = prog.clone();
    let app2 = app.clone();
    let cache2 = cache.clone();
    let model2 = model.clone();
    let outcome = translate::translate_all(
        translate::TranslateOpts {
            client: &client,
            lines: &lines,
            scene_threshold_ms: scene_ms,
            min_batch: min_b,
            max_batch: max_b,
            concurrency: conc,
            resume,
            line_cache: Some(&mut line_cache),
        },
        move |ev| match ev {
            translate::TranslateEvent::Progress(p) => {
                let _ = app2.emit("translate-progress", &p);
            }
            translate::TranslateEvent::BatchDone(idx, rows) => {
                // 批粒度落盘：中断后重启可续翻
                let mut g = prog2.lock().unwrap();
                g.batches.insert(idx, rows);
                let _ = cache2.save_progress(hash, &model2, &g);
            }
        },
    )
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
    cache.delete_progress(hash, &model); // 整集完成，断点文件退役
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

/// 召回主窗口（单窗口形态 = 视频窗口本体）：还原最小化并聚焦。
/// 手动 IPC 连接外部 mpv 的模式下无意义。
#[tauri::command]
async fn recall_mpv(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    if matches!(state.mpv.lock().await.as_ref(), Some(Mpv::Ipc(_)) | None) {
        return Err("mpv 未在播放（手动连接模式不支持召回）".into());
    }
    let win = app.get_webview_window("main").ok_or("主窗口不存在")?;
    let _ = win.unminimize();
    let _ = win.show();
    win.set_focus().map_err(|e| e.to_string())
}

/// renderer 焦点恢复链（Windows）：切回窗口后 Chromium renderer 可能未聚焦
///（document.hasFocus=false，keydown 不进页面），而激活时序下立即 set_focus
/// 会被忽略，故在 50/150/400ms 延迟点重试（set_focus → WebView2 MoveFocus，幂等）
#[cfg(windows)]
fn spawn_focus_recovery(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        for delay in [50u64, 150, 400] {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            let Some(w) = app.get_webview_window("main") else { return };
            let _ = w.set_focus();
        }
    });
}

// ---------- 字幕自动对齐 ----------

#[derive(serde::Serialize)]
struct SyncResult {
    delay_s: f64,
    speed: f64,
    drift: bool,
    segments_ok: usize,
    segments_total: usize,
}

/// ffmpeg 提全片 8kHz 单声道 PCM（1h ≈ 58MB 原始数据，提取速度数倍于实时）
async fn extract_pcm(ffmpeg: &std::path::Path, video: &str) -> Result<Vec<i16>, String> {
    let out = tokio::process::Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            video,
            "-vn",
            "-ac",
            "1",
            "-ar",
            &sync::SAMPLE_RATE.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .output()
        .await
        .map_err(|e| format!("ffmpeg 启动失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("ffmpeg 提取音频失败: {}", String::from_utf8_lossy(&out.stderr)));
    }
    Ok(out
        .stdout
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

/// 自动对齐字幕：能量包络互相关估偏移 → 映射 mpv sub-delay / sub-speed。
/// 行数据由前端传入（后端不持有字幕状态）；结果由前端下发 mpv 并持久化。
#[tauri::command]
async fn auto_sync_subtitles(
    state: tauri::State<'_, AppState>,
    video_path: String,
    lines: Vec<subtitle::SubtitleLine>,
    search_s: Option<f64>,
) -> Result<SyncResult, String> {
    let ffmpeg = state.bin("ffmpeg");
    let pcm = extract_pcm(&ffmpeg, &video_path).await?;
    // 全片互相关约 1 亿次乘加：阻塞任务丢到线程池，别卡 async runtime
    let est = tokio::task::spawn_blocking(move || {
        let audio = sync::energy_envelope(&pcm);
        let n = audio.len();
        let subs = sync::subtitle_envelope(&lines, n);
        sync::estimate(&audio, &subs, search_s.unwrap_or(30.0))
    })
    .await
    .map_err(|e| e.to_string())?
    .ok_or("音频语音太少或字幕与音轨不匹配，请手动微调（Alt+←→）")?;
    Ok(SyncResult {
        delay_s: est.delay_s,
        speed: est.speed,
        drift: est.drift,
        segments_ok: est.segments_ok,
        segments_total: est.segments_total,
    })
}

/// 持久化对齐结果（下次打开同一视频自动应用）
#[tauri::command]
fn save_sync_offset(
    state: tauri::State<'_, AppState>,
    video_hash: String,
    offset: cache::SyncOffset,
) -> Result<(), String> {
    let hash = u64::from_str_radix(&video_hash, 16).map_err(|e| e.to_string())?;
    state
        .cache()
        .save_sync_offset(hash, offset)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_sync_offset(state: tauri::State<'_, AppState>, video_hash: String) -> Option<cache::SyncOffset> {
    let hash = u64::from_str_radix(&video_hash, 16).ok()?;
    state.cache().load_sync_offset(hash)
}

/// 历史打开记录（MRU 最新在前）
#[tauri::command]
fn get_history(state: tauri::State<'_, AppState>) -> Vec<cache::HistoryEntry> {
    state.cache().load_history()
}

/// 播放位置记忆：前端周期性上报，按后端持有的当前视频 hash 存
#[tauri::command]
fn save_playback_position(state: tauri::State<'_, AppState>, position_s: f64) {
    if let Some(hash) = *state.current_video.lock().unwrap() {
        let _ = state.cache().save_position(hash, position_s.max(0.0));
    }
}

// ---------- Anki 导出 ----------

/// 导出当前句到 Anki：mpv 截图（干净帧）+ ffmpeg 音频切片（字幕时间 × sub-speed
/// + sub-delay 换算回音频时间轴，前后 0.25s 余量）→ AnkiConnect 推送；
/// Anki 未启动/未装插件时兜底写入导出目录。返回用户提示语。
#[tauri::command]
async fn export_anki_note(
    state: tauri::State<'_, AppState>,
    video_hash: String,
    video_path: String,
    line: subtitle::SubtitleLine,
    zh: Option<String>,
) -> Result<String, String> {
    use serde_json::Value;
    let video_hash = u64::from_str_radix(&video_hash, 16).map_err(|e| e.to_string())?;
    // 1) mpv 侧：取 sub-delay/sub-speed 并下发截图
    let material = {
        let guard = state.mpv.lock().await;
        let ipc = guard.as_ref().ok_or("mpv 未连接")?;
        let delay = ipc
            .get_property("sub-delay")
            .await
            .ok()
            .and_then(|v: Value| v.as_f64())
            .unwrap_or(0.0);
        let speed = ipc
            .get_property("sub-speed")
            .await
            .ok()
            .and_then(|v: Value| v.as_f64())
            .unwrap_or(1.0);
        let cache = state.cache();
        let anki_dir = cache.anki_dir();
        std::fs::create_dir_all(&anki_dir).map_err(|e| e.to_string())?;
        let stem = format!("loopsub_{video_hash:016x}_{}", line.number);
        let img = anki_dir.join(format!("{stem}.png"));
        let _ = std::fs::remove_file(&img); // 清旧文件，轮询只等新文件
        ipc.command(vec![
            Value::from("screenshot-to-file"),
            Value::from(img.to_string_lossy().as_ref()),
            Value::from("video"),
        ])
        .await
        .map_err(|e| e.to_string())?;
        // 音频切片参数在锁内算好，出锁再切（不挡播放控制）
        let t0 = ((line.start_ms as f64 / 1000.0) * speed + delay - 0.25).max(0.0);
        let t1 = (line.end_ms as f64 / 1000.0) * speed + delay + 0.25;
        drop(guard);

        // 2) 等截图 + 并行切片（spawn_blocking 不卡 runtime）
        let ffmpeg = state.bin("ffmpeg");
        let aud = anki_dir.join(format!("{stem}.mp3"));
        let video_c = video_path.clone();
        let aud_c = aud.clone();
        let cut = tokio::task::spawn_blocking(move || {
            media::cut_audio(std::path::Path::new(&video_c), t0, t1, &aud_c, &ffmpeg)
        });
        let mut shot_ok = false;
        for _ in 0..40 {
            if std::fs::metadata(&img).map(|m| m.len() > 0).unwrap_or(false) {
                shot_ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if !shot_ok {
            return Err("截图超时（2s）：mpv 未写出文件".into());
        }
        cut.await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;

        // 3) 组装素材
        let stem2 = stem;
        let title = std::path::Path::new(&video_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mmss = {
            let s = line.start_ms / 1000;
            format!("{:02}:{:02}", s / 60, s % 60)
        };
        anki::NoteMaterial {
            image_path: img,
            audio_path: aud,
            image_name: format!("{stem2}.png"),
            audio_name: format!("{stem2}.mp3"),
            sentence: line.text.clone(),
            translation: zh.unwrap_or_default(),
            source: format!("{title} #{} {mmss}", line.number),
        }
    };

    // 4) 推送或兜底
    let (deck, tags, host) = {
        let s = state.settings.lock().unwrap();
        (
            s.anki.deck.clone(),
            s.anki.tags.split_whitespace().map(|t| t.to_string()).collect::<Vec<_>>(),
            s.anki.connect_url.clone(),
        )
    };
    let ac = anki::AnkiConnect::new(&host);
    if ac.available().await {
        anki::push_note(&ac, &deck, &tags, &material)
            .await
            .map_err(|e| format!("推送 Anki 失败: {e}"))?;
        Ok(format!("已加入牌组「{deck}」"))
    } else {
        let dir = anki::export_fallback(&state.cache().anki_export_dir(), &material)?;
        Ok(format!(
            "Anki 未连接（需装 AnkiConnect 并启动 Anki）— 素材已导出到 {}",
            dir.display()
        ))
    }
}

#[tauri::command]
fn set_always_on_top(window: tauri::Window, flag: bool) -> Result<(), String> {
    window.set_always_on_top(flag).map_err(|e| e.to_string())
}

/// 原生文件对话框选视频（Rust 侧调起，前端无需 dialog 插件权限）
/// 置顶的面板会盖住非置顶对话框：打开期间临时取消置顶，选完恢复原状
#[tauri::command]
async fn pick_video(window: tauri::Window) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let was_top = window.is_always_on_top().unwrap_or(false);
    if was_top {
        let _ = window.set_always_on_top(false);
    }
    let picked = window
        .dialog()
        .file()
        .add_filter(
            "视频文件",
            &[
                "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "ts", "m2ts", "mpg",
                "mpeg", "rmvb",
            ],
        )
        .blocking_pick_file();
    if was_top {
        let _ = window.set_always_on_top(true);
    }
    picked.map(|f| f.to_string())
}

pub fn run() {
    let settings_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("loopsub")
        .join("settings.json");
    let settings = Settings::load(&settings_path).unwrap_or_default();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 主面板销毁即带走 libmpv：视频窗口是本进程窗口，Tauri 只收自己的
            // webview 窗口；面板没了，视频窗口就成了没有控制端的孤儿（进程也不退）
            if let Some(main_win) = app.get_webview_window("main") {
                let app_handle = app.handle().clone();
                main_win.on_window_event(move |event| {
                    match event {
                        tauri::WindowEvent::Destroyed => {
                            eprintln!("[win-event] main destroyed, shutdown mpv");
                            let state = app_handle.state::<AppState>();
                            // 退出路径不阻塞等锁：持锁方（mpv_start_internal）可能正在
                            // 等主线程创建子窗口，此处 blocking 会成死锁环退不掉；
                            // 拿不到就随进程退出（进程内 libmpv 由 OS 兜底清理）
                            let g = state.mpv.try_lock();
                            if let Ok(mut g) = g {
                                if let Some(Mpv::Embed(e)) = g.take() {
                                    e.shutdown();
                                }
                            }
                        }
                        // 拉伸重排视频子窗口（顶栏以下、抽屉以左）
                        #[cfg(windows)]
                        tauri::WindowEvent::Resized(_) => relayout_video(&app_handle),
                        // 切回窗口后把键盘焦点交还 WebView2：失焦再激活后 Chromium
                        // 内部焦点不自动恢复（页面快捷键全失效）；激活时序下立即
                        // MoveFocus 会被忽略，走延迟重试链恢复
                        tauri::WindowEvent::Focused(true) => {
                            if let Some(w) = app_handle.get_webview_window("main") {
                                let _ = w.set_focus();
                            }
                            #[cfg(windows)]
                            spawn_focus_recovery(app_handle.clone());
                        }
                        _ => {}
                    }
                });
            }
            // 焦点看门狗（Windows）：鼠标点视频画面激活主窗口时，系统会把键盘
            // 焦点恢复给“上次持焦的子窗口”——纯显示的视频窗口，MoveFocus 与
            // WM_MOUSEACTIVATE 拦截都压不住这个时序。轮询发现焦点落在视频窗口
            // 上（loopsub-video 无输入绑定，持焦必是异常）就投递主线程抢回
            // WebView2；与 mpv 子类化/消息时序完全无关。主窗口销毁后线程退出。
            #[cfg(windows)]
            {
                use windows_sys::Win32::Foundation::HWND;
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    loop {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let Some(win) = app_handle.get_webview_window("main") else { break };
                    let Ok(hwnd) = win.hwnd() else { break };
                    let main = hwnd.0 as HWND;
                    // mpv 会在 loopsub-video 内自建 mpv 类子窗口做渲染输出：它一旦
                    // 持焦键盘就全废（属 libmpv VO 线程，主线程 GUIThreadInfo 看不
                    // 到，消息也不经我们的 wndproc）。找到就补 WS_DISABLED——不收任
                    // 何鼠标键盘输入，命中测试穿透回我们的穿透链；渲染不走输入路径
                    // 无副作用。换视频重建窗口后会自动再补（幂等）。
                    let vc: Vec<u16> = "loopsub-video".encode_utf16().chain(std::iter::once(0)).collect();
                    let mc: Vec<u16> = "mpv".encode_utf16().chain(std::iter::once(0)).collect();
                    let video = unsafe { FindWindowExW(main, std::ptr::null_mut(), vc.as_ptr(), std::ptr::null()) };
                    if !video.is_null() {
                        let mpvw = unsafe { FindWindowExW(video, std::ptr::null_mut(), mc.as_ptr(), std::ptr::null()) };
                        if !mpvw.is_null() {
                            let style = unsafe { GetWindowLongPtrW(mpvw, GWL_STYLE) };
                            if style & (WS_DISABLED as isize) == 0 {
                                unsafe { SetWindowLongPtrW(mpvw, GWL_STYLE, style | (WS_DISABLED as isize)) };
                            }
                            // WS_DISABLED 只断输入不断命中：disabled 窗口的 NCHITTEST
                            // 仍返回 HTCLIENT，鼠标 down 派发给它后被系统直接丢弃——
                            // 穿透链断、主窗口不激活（实测病根：点画面窗口不置前）。
                            // 补 WS_EX_TRANSPARENT 让命中测试整体跳过它（WebView2 自带
                            // 的 D3D 输出窗口同为 disabled，就靠此样式让位）；只影响
                            // 命中测试，不影响 mpv 渲染输出
                            let exstyle = unsafe { GetWindowLongPtrW(mpvw, GWL_EXSTYLE) };
                            if exstyle & (WS_EX_TRANSPARENT as isize) == 0 {
                                unsafe { SetWindowLongPtrW(mpvw, GWL_EXSTYLE, exstyle | (WS_EX_TRANSPARENT as isize)) };
                            }
                        }
                    }
                    // GetFocus 只对调用线程队列有效，必须经 GUIThreadInfo 看主线程
                    let tid = unsafe { GetWindowThreadProcessId(main, std::ptr::null_mut()) };
                    let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
                    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
                    if unsafe { GetGUIThreadInfo(tid, &mut info) } == 0 {
                        continue;
                    }
                    let mut buf = [0u16; 64];
                    let n = if info.hwndFocus.is_null() {
                        0
                    } else {
                        unsafe { GetClassNameW(info.hwndFocus, buf.as_mut_ptr(), buf.len() as i32) }
                    };
                    let name = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
                    if info.hwndActive != main || info.hwndFocus.is_null() {
                        continue; // 主窗口非激活/无人持焦：不管
                    }
                    if name.is_empty() {
                        continue;
                    }
                    // 白名单：焦点在 wry 容器及其下的 WebView2/Chromium 窗口即正常；
                    // 其余（主窗口框架、loopsub-video 纯显示层等）DOM 都收不到键盘，
                    // 一律抢回。放宽原因：激活时系统恢复的焦点目标并不固定。
                    let normal = name.starts_with("WRY_WEBVIEW")
                        || name.starts_with("Chrome_WidgetWin")
                        || name.starts_with("Windows.UI.Core.CoreWindow");
                    if normal {
                        continue;
                    }
                    let wh = app_handle.clone();
                    let _ = app_handle.run_on_main_thread(move || {
                        if let Some(w) = wh.get_webview_window("main") {
                            let _ = w.set_focus();
                        }
                    });
                    }
                });
            }
            Ok(())
        })
        .manage(AppState {
            settings: Mutex::new(settings),
            settings_path,
            mpv: tokio::sync::Mutex::new(None),
            current_video: Mutex::new(None),
            drawer_open: Mutex::new(false),
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
            pick_video,
            auto_sync_subtitles,
            save_sync_offset,
            get_sync_offset,
            get_history,
            save_playback_position,
            export_anki_note,
            window_mode,
            set_drawer,
            webview_ready,
        ])
        .build(tauri::generate_context!())
        .expect("error while building loopSub");

    // 退出时销毁 libmpv 实例（terminate_destroy 触发并等待 core 退出）
    app.run(|handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let state = handle.state::<AppState>();
            // 同 Destroyed：try_lock，拿不到就由 OS 回收（下一行 process::exit 反正强退）
            let g = state.mpv.try_lock();
            if let Ok(mut g) = g {
                if let Some(Mpv::Embed(e)) = g.take() {
                    e.shutdown();
                }
            }
            eprintln!("[run-event] exit cleanup done, process::exit");
            // libmpv/WebView2/CRT 多层运行时的退出收尾在 Windows 上会互相等锁
            // （实测：Exit 后 25 线程全 Wait、进程不散）。我们的资源（mpv core、
            // 视频窗口、线程）已在 Destroyed/上方主动清理，剩下的交给 OS 回收，
            // 直接退出进程，跳过不可控的收尾。
            std::process::exit(0);
        }
    });
}
