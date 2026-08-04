//! 缓存目录布局与 OpenSubtitles moviehash。
//!
//! <root>/
//! ├── originals/    提取或下载的字幕原文（<hash>.srt）
//! ├── truecased/    大写还原结果
//! ├── translated/   译文（<hash>.<model>.srt）与批粒度进度（<hash>.<model>.progress.json）
//! ├── lines/        行内容级译文缓存（<model>.jsonl），跨视频复用
//! ├── videos/       按视频 hash 的播放配置（字幕延迟、速度、播放位置）
//! └── history.json  历史打开记录（MRU 最新在前）

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MOVIEHASH_CHUNK: usize = 64 * 1024;

/// 自动对齐结果（按视频持久化，下次打开自动应用）
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct SyncOffset {
    pub delay_s: f64,
    pub speed: f64,
}

/// sub-speed 的语义默认值：1.0 = 无缩放。
/// 0 是毒化值——mpv sub-speed=0 会冻结字幕时钟（字幕事件永不激活，
/// 画面/截图/sub-text 全空），绝不可作为默认值或占位值。
fn default_speed() -> f64 {
    1.0
}

/// 每视频播放配置（videos/<hash>.json）：字幕延迟/速度/播放位置。
/// 字段全带 serde(default)，缺字段的旧文件可直接读入。
/// speed 默认必须是 1.0：历史 bug 把 default 0.0 当对齐结果下发给 mpv，
/// 字幕整体冻结；已污染的配置由读取端免疫 + 下次保存自动洗白。
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct VideoConfig {
    #[serde(default)]
    pub delay_s: f64,
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// 上次播放位置（秒）；< 5 视为从头播
    #[serde(default)]
    pub position_s: f64,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            delay_s: 0.0,
            speed: default_speed(),
            position_s: 0.0,
        }
    }
}

/// 历史记录上限
pub const MAX_HISTORY: usize = 20;

/// 一条历史打开记录
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HistoryEntry {
    pub path: String,
    /// 上次打开的 unix 秒
    pub last_opened: i64,
}

/// 翻译断点续翻的进度文件
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProgressFile {
    /// 原文行指纹：换源字幕导致行内容变化时，旧进度作废
    #[serde(default)]
    pub fingerprint: u64,
    /// 批号（build_batches 顺序号，从 0 计）-> 行号 -> 译文；仅含成功行
    #[serde(default)]
    pub batches: BTreeMap<u32, BTreeMap<u32, String>>,
}

#[derive(Clone)]
pub struct Cache {
    pub root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in ["originals", "truecased", "translated", "lines", "videos", "anki"] {
            std::fs::create_dir_all(self.root.join(dir))?;
        }
        Ok(())
    }

    pub fn original_path(&self, hash: u64) -> PathBuf {
        self.root.join("originals").join(format!("{hash:016x}.srt"))
    }

    pub fn truecased_path(&self, hash: u64) -> PathBuf {
        self.root.join("truecased").join(format!("{hash:016x}.srt"))
    }

    pub fn translated_path(&self, hash: u64, model: &str) -> PathBuf {
        self.root
            .join("translated")
            .join(format!("{hash:016x}.{model}.srt"))
    }

    pub fn video_config_path(&self, hash: u64) -> PathBuf {
        self.root.join("videos").join(format!("{hash:016x}.json"))
    }

    pub fn load_sync_offset(&self, hash: u64) -> Option<SyncOffset> {
        let data = std::fs::read_to_string(self.video_config_path(hash)).ok()?;
        let cfg: VideoConfig = serde_json::from_str(&data).ok()?;
        // speed ≤ 0 不是有效对齐结果（合法估计经 SPEED_CLAMP 限定 0.9~1.1），
        // 是历史 default 0.0 写盘的占位值；按无记录处理，防前端下发 sub-speed=0
        if cfg.speed <= 0.0 {
            return None;
        }
        Some(SyncOffset {
            delay_s: cfg.delay_s,
            speed: cfg.speed,
        })
    }

    pub fn save_sync_offset(&self, hash: u64, off: SyncOffset) -> std::io::Result<()> {
        let mut cfg = self.load_video_config(hash);
        cfg.delay_s = off.delay_s;
        // 写入端钳制：speed 必须为正，异常值归一化（读取端已免疫，此处双保险）
        cfg.speed = if off.speed > 0.0 { off.speed } else { default_speed() };
        self.save_video_config(hash, &cfg)
    }

    /// 读每视频配置；文件缺失或损坏时回落默认（delay/position 归 0，speed 归 1.0）
    pub fn load_video_config(&self, hash: u64) -> VideoConfig {
        std::fs::read_to_string(self.video_config_path(hash))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_video_config(&self, hash: u64, cfg: &VideoConfig) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(cfg).unwrap();
        std::fs::write(self.video_config_path(hash), data)
    }

    /// 记播放位置（读改写，不动延迟/速度字段）
    pub fn save_position(&self, hash: u64, pos_s: f64) -> std::io::Result<()> {
        let mut cfg = self.load_video_config(hash);
        cfg.position_s = pos_s;
        self.save_video_config(hash, &cfg)
    }

    // ---------- 历史打开记录 ----------

    pub fn history_path(&self) -> PathBuf {
        self.root.join("history.json")
    }

    pub fn load_history(&self) -> Vec<HistoryEntry> {
        std::fs::read_to_string(self.history_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_history(&self, entries: &[HistoryEntry]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let data = serde_json::to_string_pretty(entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.history_path(), data)
    }

    /// 打开记录置顶（按路径去重，MRU，截断到上限）
    pub fn touch_history(&self, path: &str) -> std::io::Result<()> {
        let mut entries = self.load_history();
        entries.retain(|e| e.path != path);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        entries.insert(
            0,
            HistoryEntry {
                path: path.to_string(),
                last_opened: ts,
            },
        );
        entries.truncate(MAX_HISTORY);
        self.save_history(&entries)
    }

    /// 剔除失效记录（文件已删除等）
    pub fn remove_history(&self, path: &str) -> std::io::Result<()> {
        let mut entries = self.load_history();
        let before = entries.len();
        entries.retain(|e| e.path != path);
        if entries.len() != before {
            self.save_history(&entries)?;
        }
        Ok(())
    }

    /// Anki 素材暂存（截图/音频切片，推送后可留作复用）
    pub fn anki_dir(&self) -> PathBuf {
        self.root.join("anki")
    }

    /// AnkiConnect 不可用时的兜底导出目录（TSV + 媒体 + 导入说明）
    pub fn anki_export_dir(&self) -> PathBuf {
        self.root.join("anki").join("export")
    }

    pub fn progress_path(&self, hash: u64, model: &str) -> PathBuf {
        self.root
            .join("translated")
            .join(format!("{hash:016x}.{model}.progress.json"))
    }

    /// 行内容级译文缓存（jsonl：{"h": fnv64(原文), "zh": 译文}）
    pub fn lines_path(&self, model: &str) -> PathBuf {
        self.root.join("lines").join(format!("{model}.jsonl"))
    }

    pub fn load_progress(&self, hash: u64, model: &str) -> ProgressFile {
        std::fs::read_to_string(self.progress_path(hash, model))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_progress(&self, hash: u64, model: &str, progress: &ProgressFile) -> std::io::Result<()> {
        let s = serde_json::to_string(progress)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(self.progress_path(hash, model), s)
    }

    pub fn delete_progress(&self, hash: u64, model: &str) {
        let _ = std::fs::remove_file(self.progress_path(hash, model));
    }
}

/// OpenSubtitles moviehash：文件大小 + 首尾各 64KB 内容的 u64 小端字之和。
/// 与 OpenSubtitles 搜索共用同一键，保证"缓存命中的就是搜索能命中的"。
pub fn moviehash(path: &Path) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hash: u64 = size;

    // 小于 128KB 的文件算法不适用，退化为仅用文件大小
    if size < 2 * MOVIEHASH_CHUNK as u64 {
        return Ok(hash);
    }

    let mut buf = [0u8; MOVIEHASH_CHUNK];
    file.read_exact(&mut buf)?;
    hash = hash.wrapping_add(sum_u64_le(&buf));

    file.seek(SeekFrom::Start(size - MOVIEHASH_CHUNK as u64))?;
    file.read_exact(&mut buf)?;
    hash = hash.wrapping_add(sum_u64_le(&buf));

    Ok(hash)
}

fn sum_u64_le(buf: &[u8]) -> u64 {
    buf.chunks_exact(8).fold(0u64, |acc, chunk| {
        acc.wrapping_add(u64::from_le_bytes(chunk.try_into().unwrap()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn moviehash_is_stable() {
        let path = std::env::temp_dir().join("loopsub_hash_test.bin");
        let mut f = File::create(&path).unwrap();
        f.write_all(&vec![0xABu8; 200 * 1024]).unwrap();
        drop(f);
        let h1 = moviehash(&path).unwrap();
        let h2 = moviehash(&path).unwrap();
        assert_eq!(h1, h2);
        assert_ne!(h1, 0);
        std::fs::remove_file(&path).ok();
    }
}
