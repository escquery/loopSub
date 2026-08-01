//! 缓存目录布局与 OpenSubtitles moviehash。
//!
//! <root>/
//! ├── originals/    提取或下载的字幕原文（<hash>.srt）
//! ├── truecased/    大写还原结果
//! ├── translated/   译文（<hash>.<model>.srt）与批粒度进度（<hash>.<model>.progress.json）
//! ├── lines/        行内容级译文缓存（<model>.jsonl），跨视频复用
//! └── videos/       按视频 hash 的播放配置（字幕延迟、速度等）

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
        for dir in ["originals", "truecased", "translated", "lines", "videos"] {
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
        serde_json::from_str(&data).ok()
    }

    pub fn save_sync_offset(&self, hash: u64, off: SyncOffset) -> std::io::Result<()> {
        let data = serde_json::to_string_pretty(&off).unwrap();
        std::fs::write(self.video_config_path(hash), data)
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
