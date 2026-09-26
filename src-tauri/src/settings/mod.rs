//! 设置：JSON 持久化，字段缺失时全部回落到默认值。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkeys: HashMap<String, String>,
    pub audio: AudioSettings,
    pub subtitle: SubtitleSettings,
    pub copy: CopySettings,
    pub opensubtitles: OpenSubtitlesSettings,
    pub llm: LlmSettings,
    pub cache: CacheSettings,
    pub bins: BinSettings,
    pub window: WindowSettings,
    pub anki: AnkiSettings,
    /// 资源管理器右键菜单“用 loopSub 播放”（仅 Windows）：None = 未做过选择，
    /// 首次启动按默认开启处理；Some 为用户显式选择，启动时与注册表对齐
    pub explorer_context_menu: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// 对白增强（固定参数短时压缩 + 峰值限制），默认开启
    pub dialogue_boost: bool,
    /// 音量上限百分比；None 表示不生效（mpv 默认 130）
    pub volume_max: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub truecase: TruecaseMode,
    pub render: RenderMode,
    /// v2 起 Windows/macOS 默认都在视频画面显示字幕；旧配置缺少此标记时迁移一次。
    #[serde(default = "render_default_v2_pending")]
    pub render_default_v2: bool,
    /// 延迟微调步长（毫秒）
    pub delay_step_ms: u32,
}

fn render_default_v2_pending() -> bool {
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruecaseMode {
    Rule,
    Llm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderMode {
    /// 仅右侧面板显示，mpv 侧 sub-visibility=no
    Panel,
    /// 交给 mpv 在视频画面渲染（默认）
    Mpv,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CopySettings {
    /// 剪贴板模板，{lines} 为台词占位符
    pub template: String,
    /// 复制时附带的上下文句数
    pub context_lines: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenSubtitlesSettings {
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmSettings {
    /// OpenAI 兼容接口地址（DeepSeek / 通义 / 本地 vLLM 等）
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub concurrency: u32,
    pub scene_threshold_s: f64,
    pub min_batch_lines: u32,
    pub max_batch_lines: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    /// 缓存根目录；None 使用系统应用数据目录
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BinSettings {
    /// mpv / ffmpeg / ffprobe 所在目录；None 按 PATH → 应用目录查找
    pub dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub dock_side: DockSide,
    /// macOS 省电模式：OpenGL 视频层使用逻辑分辨率，由系统缩放到 Retina。
    /// 视频解码分辨率不变，只减少输出 FBO 和窗口合成像素数。
    pub macos_low_power_video: bool,
    /// 焦点去无关应用时取消 always-on-top 让面板沉底
    pub sink_on_blur: bool,
    /// 切回面板时主动召回 mpv 窗口到面板下方
    pub recall_mpv_on_focus: bool,
    /// 悬浮字幕条（透明置顶窗，浮在视频画面上；hover 展开操作）
    #[serde(default)]
    pub float_bar: bool,
    /// 悬浮条位置记忆（屏幕物理坐标）；None = 默认底部居中
    #[serde(default)]
    pub float_bar_pos: Option<(i32, i32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DockSide {
    Left,
    Right,
}

pub const DEFAULT_COPY_TEMPLATE: &str =
    "请逐句讲解以下美剧台词中的生词、短语和口语用法：\n\n{lines}";

impl Default for Settings {
    fn default() -> Self {
        Self {
            hotkeys: default_hotkeys(),
            audio: AudioSettings::default(),
            subtitle: SubtitleSettings::default(),
            copy: CopySettings::default(),
            opensubtitles: OpenSubtitlesSettings::default(),
            llm: LlmSettings::default(),
            cache: CacheSettings::default(),
            bins: BinSettings::default(),
            window: WindowSettings::default(),
            anki: AnkiSettings::default(),
            explorer_context_menu: None,
        }
    }
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            dialogue_boost: true,
            volume_max: None,
        }
    }
}

impl Default for SubtitleSettings {
    fn default() -> Self {
        Self {
            truecase: TruecaseMode::Rule,
            render: RenderMode::Mpv,
            render_default_v2: true,
            delay_step_ms: 100,
        }
    }
}

impl Default for CopySettings {
    fn default() -> Self {
        Self {
            template: DEFAULT_COPY_TEMPLATE.to_string(),
            context_lines: 0,
        }
    }
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            base_url: None,
            model: None,
            api_key: None,
            concurrency: 4,
            scene_threshold_s: 60.0,
            min_batch_lines: 10,
            max_batch_lines: 30,
        }
    }
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            dock_side: DockSide::Right,
            macos_low_power_video: true,
            sink_on_blur: true,
            recall_mpv_on_focus: true,
            float_bar: false,
            float_bar_pos: None,
        }
    }
}

impl Default for TruecaseMode {
    fn default() -> Self {
        TruecaseMode::Rule
    }
}

impl Default for RenderMode {
    fn default() -> Self {
        RenderMode::Mpv
    }
}

impl Default for DockSide {
    fn default() -> Self {
        DockSide::Right
    }
}

/// Anki 导出配置（AnkiConnect 为主，连不上时降级文件导出）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AnkiSettings {
    /// 目标牌组名
    pub deck: String,
    /// 卡片标签（空格分隔）
    pub tags: String,
    /// AnkiConnect 地址
    pub connect_url: String,
}

impl Default for AnkiSettings {
    fn default() -> Self {
        Self {
            deck: "loopSub".into(),
            tags: "loopsub".into(),
            connect_url: "http://127.0.0.1:8765".into(),
        }
    }
}

/// 默认快捷键表（PotPlayer 风格），设置页可全部改绑
pub fn default_hotkeys() -> HashMap<String, String> {
    let pairs: [(&str, &str); 30] = [
        ("toggle_pause", "Space"),
        ("seek_back", "ArrowLeft"),
        ("seek_forward", "ArrowRight"),
        ("prev_sentence", "ArrowUp"),
        ("next_sentence", "ArrowDown"),
        ("speed_down", "x"),
        ("speed_up", "c"),
        ("speed_reset", "z"),
        ("ab_set_a", "["),
        ("ab_set_b", "]"),
        ("ab_nudge_a_back", "ctrl+["),
        ("ab_nudge_b_back", "ctrl+]"),
        ("ab_nudge_a_fwd", "alt+["),
        ("ab_nudge_b_fwd", "alt+]"),
        ("ab_clear_a", "shift+["),
        ("ab_clear_b", "shift+]"),
        ("sentence_loop", "Enter"),
        ("follow_mode", "r"),
        ("toggle_translation", "t"),
        ("reveal_current_translation", "ctrl+f"),
        ("select_current", "v"),
        ("sub_delay_minus", "alt+ArrowLeft"),
        ("sub_delay_plus", "alt+ArrowRight"),
        ("sub_delay_minus_coarse", "alt+shift+ArrowLeft"),
        ("sub_delay_plus_coarse", "alt+shift+ArrowRight"),
        ("sub_delay_reset", "alt+0"),
        ("toggle_panel", "s"),
        ("recall_mpv", "w"),
        ("fit_video_window", "1"),
        ("anki_export", "k"),
    ];
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Settings {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut settings: Settings = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // 迁移：ab_clear/ab_clear_alt 曾把 shift+[/] 当作“取消 AB 循环”的双键，
        // 实为误读——两个键应分别取消 A/B 点（ab_clear_a/ab_clear_b）。
        // 未改绑过的残留清掉，由下方补缺循环挂上新默认；ab_clear_alt 已废弃一律移除。
        if settings.hotkeys.get("ab_clear").map(|s| s.as_str()) == Some("shift+[") {
            settings.hotkeys.remove("ab_clear");
        }
        settings.hotkeys.remove("ab_clear_alt");
        // 旧版默认是仅面板显示。升级后迁移一次到 mpv 画面字幕，使 Windows/macOS
        // 行为一致；标记落盘后用户仍可主动切回仅面板模式。
        let migrate_render_default = !settings.subtitle.render_default_v2;
        if migrate_render_default {
            settings.subtitle.render = RenderMode::Mpv;
            settings.subtitle.render_default_v2 = true;
        }
        // 老配置补挂新版本新增的默认热键（仅补缺项；设置页无解绑功能，不会误恢复）
        for (action, combo) in default_hotkeys() {
            settings.hotkeys.entry(action).or_insert(combo);
        }
        if migrate_render_default {
            let _ = settings.save(path);
        }
        Ok(settings)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_power_setting_uses_low_power_default() {
        let settings: Settings =
            serde_json::from_str(r#"{"window":{"sink_on_blur":false}}"#).unwrap();
        assert!(settings.window.macos_low_power_video);
    }

    #[test]
    fn old_panel_default_migrates_once_to_mpv() {
        let path = std::env::temp_dir().join(format!(
            "loopsub_settings_render_migration_{}.json",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"subtitle":{"render":"panel"}}"#).unwrap();

        let mut settings = Settings::load(&path).unwrap();
        assert_eq!(settings.subtitle.render, RenderMode::Mpv);
        assert!(settings.subtitle.render_default_v2);

        settings.subtitle.render = RenderMode::Panel;
        settings.save(&path).unwrap();
        let reloaded = Settings::load(&path).unwrap();
        assert_eq!(reloaded.subtitle.render, RenderMode::Panel);
        std::fs::remove_file(path).ok();
    }
}
