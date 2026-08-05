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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// 对白增强（mpv dynaudnorm 滤镜），默认开启
    pub dialogue_boost: bool,
    /// 音量上限百分比；None 表示不生效（mpv 默认 130）
    pub volume_max: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleSettings {
    pub truecase: TruecaseMode,
    pub render: RenderMode,
    /// 延迟微调步长（毫秒）
    pub delay_step_ms: u32,
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
    /// 面板字幕条渲染（默认），mpv 侧 sub-visibility=no
    Panel,
    /// 交给 mpv 渲染（ASS 特效字幕场景）
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
    /// 播放时收起为迷你条
    pub mini_bar: bool,
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
            render: RenderMode::Panel,
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
            mini_bar: true,
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
        RenderMode::Panel
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
    let pairs: [(&str, &str); 28] = [
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
        ("select_current", "v"),
        ("sub_delay_minus", "alt+ArrowLeft"),
        ("sub_delay_plus", "alt+ArrowRight"),
        ("sub_delay_minus_coarse", "alt+shift+ArrowLeft"),
        ("sub_delay_plus_coarse", "alt+shift+ArrowRight"),
        ("sub_delay_reset", "alt+0"),
        ("toggle_panel", "s"),
        ("recall_mpv", "w"),
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
        let mut settings: Settings =
            serde_json::from_str(&content).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // 迁移：ab_clear/ab_clear_alt 曾把 shift+[/] 当作“取消 AB 循环”的双键，
        // 实为误读——两个键应分别取消 A/B 点（ab_clear_a/ab_clear_b）。
        // 未改绑过的残留清掉，由下方补缺循环挂上新默认；ab_clear_alt 已废弃一律移除。
        if settings.hotkeys.get("ab_clear").map(|s| s.as_str()) == Some("shift+[") {
            settings.hotkeys.remove("ab_clear");
        }
        settings.hotkeys.remove("ab_clear_alt");
        // 老配置补挂新版本新增的默认热键（仅补缺项；设置页无解绑功能，不会误恢复）
        for (action, combo) in default_hotkeys() {
            settings.hotkeys.entry(action).or_insert(combo);
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
