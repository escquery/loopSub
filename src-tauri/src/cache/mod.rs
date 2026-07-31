//! 缓存目录布局与 OpenSubtitles moviehash。
//!
//! <root>/
//! ├── originals/    提取或下载的字幕原文（<hash>.srt）
//! ├── truecased/    大写还原结果
//! ├── translated/   译文（<hash>.<model>.srt）与批粒度进度
//! └── videos/       按视频 hash 的播放配置（字幕延迟、速度等）

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const MOVIEHASH_CHUNK: usize = 64 * 1024;

pub struct Cache {
    pub root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in ["originals", "truecased", "translated", "videos"] {
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
