//! 视频加载管线：ffprobe 探测字幕轨、ffmpeg 提取内嵌字幕。
//! 图形字幕（PGS/VobSub）无法直接使用，留给 OpenSubtitles 搜索兜底（v1.5）。

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::subtitle::SubtitleLine;

#[derive(Error, Debug)]
pub enum MediaError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ffprobe failed: {0}")]
    Ffprobe(String),
    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),
}

/// 可提取为文本的字幕编码
const TEXT_CODECS: &[&str] = &[
    "subrip", "srt", "ass", "ssa", "mov_text", "webvtt", "text", "eia_608",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubTrack {
    /// 全局流索引（ffmpeg -map 0:N 用）
    pub index: u32,
    pub codec: String,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_text: bool,
}

#[derive(Debug, Serialize)]
pub struct LoadVideoResult {
    pub lines: Vec<SubtitleLine>,
    /// "cache"（缓存命中）| "embedded"（本次提取）
    pub source: String,
    pub video_hash: String,
    pub notice: Option<String>,
}

pub fn probe_subtitles(video: &Path, ffprobe: &Path) -> Result<Vec<SubTrack>, MediaError> {
    let mut cmd = std::process::Command::new(ffprobe);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_streams",
        "-select_streams",
        "s",
    ])
    .arg(video);
    crate::bins::no_window(&mut cmd);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(MediaError::Ffprobe(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    let v: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let mut tracks = Vec::new();
    if let Some(streams) = v.get("streams").and_then(|s| s.as_array()) {
        for s in streams {
            let codec = s
                .get("codec_name")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            let index = s.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let tags = s.get("tags");
            let get_tag = |key: &str| {
                tags.and_then(|t| t.get(key))
                    .and_then(|l| l.as_str())
                    .map(String::from)
            };
            tracks.push(SubTrack {
                index,
                is_text: TEXT_CODECS.contains(&codec.as_str()),
                codec,
                language: get_tag("language"),
                title: get_tag("title"),
            });
        }
    }
    Ok(tracks)
}

/// 挑选文本字幕轨：优先英文轨，否则第一条文本轨
pub fn pick_text_track(tracks: &[SubTrack]) -> Option<&SubTrack> {
    tracks
        .iter()
        .find(|t| {
            t.is_text
                && t.language
                    .as_deref()
                    .map(|l| l.starts_with("en"))
                    .unwrap_or(false)
        })
        .or_else(|| tracks.iter().find(|t| t.is_text))
}

pub fn extract_subtitle(video: &Path, track_index: u32, out: &Path, ffmpeg: &Path) -> Result<(), MediaError> {
    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-y", "-v", "error", "-i"])
        .arg(video)
        .args(["-map", &format!("0:{track_index}"), "-c:s", "srt"])
        .arg(out);
    crate::bins::no_window(&mut cmd);
    let output = cmd.output()?;
    if !output.status.success() {
        return Err(MediaError::Ffmpeg(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_english_text_track_first() {
        let tracks = vec![
            SubTrack {
                index: 2,
                codec: "subrip".into(),
                language: Some("chi".into()),
                title: None,
                is_text: true,
            },
            SubTrack {
                index: 3,
                codec: "subrip".into(),
                language: Some("eng".into()),
                title: None,
                is_text: true,
            },
            SubTrack {
                index: 4,
                codec: "hdmv_pgs_subtitle".into(),
                language: Some("eng".into()),
                title: None,
                is_text: false,
            },
        ];
        assert_eq!(pick_text_track(&tracks).unwrap().index, 3);
    }
}
