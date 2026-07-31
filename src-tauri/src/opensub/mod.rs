//! OpenSubtitles REST API（opensubtitles.com API v1）：moviehash 精确搜索 +
//! 文件名解析回退搜索 + 下载。API Key 惰性校验（首次搜索时才检查）。

use serde::{Deserialize, Serialize};
use thiserror::Error;

const API_BASE: &str = "https://api.opensubtitles.com/api/v1";
const USER_AGENT: &str = "loopSub v0.1";

#[derive(Error, Debug)]
pub enum OsError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error: {0}")]
    Api(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubCandidate {
    pub file_id: u64,
    pub release: String,
    pub language: String,
    pub downloads: u64,
    pub hearing_impaired: bool,
}

pub struct OsClient {
    http: reqwest::Client,
    api_key: String,
}

impl OsClient {
    pub fn new(api_key: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            api_key: api_key.to_string(),
        }
    }

    pub async fn search_by_hash(&self, hash_hex: &str, lang: &str) -> Result<Vec<SubCandidate>, OsError> {
        self.search(&[
            ("moviehash", hash_hex.to_string()),
            ("languages", lang.to_string()),
        ])
        .await
    }

    pub async fn search_by_query(&self, info: &ShowInfo, lang: &str) -> Result<Vec<SubCandidate>, OsError> {
        let mut params = vec![
            ("query", info.title.clone()),
            ("languages", lang.to_string()),
        ];
        if let Some(s) = info.season {
            params.push(("season_number", s.to_string()));
        }
        if let Some(e) = info.episode {
            params.push(("episode_number", e.to_string()));
        }
        self.search(&params).await
    }

    async fn search(&self, params: &[(&str, String)]) -> Result<Vec<SubCandidate>, OsError> {
        let resp = self
            .http
            .get(format!("{API_BASE}/subtitles"))
            .header("Api-Key", &self.api_key)
            .header("User-Agent", USER_AGENT)
            .query(params)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(OsError::Api(format!("search 失败: HTTP {}", resp.status())));
        }
        let v: serde_json::Value = resp.json().await?;
        let mut out = Vec::new();
        if let Some(data) = v.get("data").and_then(|d| d.as_array()) {
            for item in data {
                let attr = &item["attributes"];
                let file_id = attr["files"]
                    .as_array()
                    .and_then(|f| f.first())
                    .and_then(|f| f["file_id"].as_u64());
                let Some(file_id) = file_id else { continue };
                out.push(SubCandidate {
                    file_id,
                    release: attr["release"].as_str().unwrap_or("").to_string(),
                    language: attr["language"].as_str().unwrap_or("").to_string(),
                    downloads: attr["download_count"].as_u64().unwrap_or(0),
                    hearing_impaired: attr["hearing_impaired"].as_bool().unwrap_or(false),
                });
            }
        }
        // 下载量高的排前面
        out.sort_by(|a, b| b.downloads.cmp(&a.downloads));
        out.truncate(20);
        Ok(out)
    }

    /// 下载字幕内容（SRT 文本）
    pub async fn download(&self, file_id: u64) -> Result<String, OsError> {
        let resp = self
            .http
            .post(format!("{API_BASE}/download"))
            .header("Api-Key", &self.api_key)
            .header("User-Agent", USER_AGENT)
            .json(&serde_json::json!({ "file_id": file_id }))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(OsError::Api(format!("download 失败: HTTP {}", resp.status())));
        }
        let v: serde_json::Value = resp.json().await?;
        let link = v["link"]
            .as_str()
            .ok_or_else(|| OsError::Api("响应缺少下载链接".into()))?;
        Ok(self.http.get(link).send().await?.text().await?)
    }
}

/// 从文件名解析剧名与集数
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowInfo {
    pub title: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

pub fn parse_filename(name: &str) -> ShowInfo {
    // 去掉扩展名
    let stem = match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    };
    let clean = |s: &str| s.replace(['.', '_'], " ").trim().to_string();

    // 美剧主流命名：Title.S01E03.xxx / Title.s01e03.xxx
    let re_se = regex::Regex::new(r"(?i)^(.*?)[.\s_\-]+s(\d{1,2})e(\d{1,2})").unwrap();
    if let Some(c) = re_se.captures(stem) {
        return ShowInfo {
            title: clean(&c[1]),
            season: c[2].parse().ok(),
            episode: c[3].parse().ok(),
        };
    }
    // 备选：Title.1x03.xxx
    let re_x = regex::Regex::new(r"(?i)^(.*?)[.\s_\-]+(\d{1,2})x(\d{1,2})").unwrap();
    if let Some(c) = re_x.captures(stem) {
        return ShowInfo {
            title: clean(&c[1]),
            season: c[2].parse().ok(),
            episode: c[3].parse().ok(),
        };
    }
    ShowInfo {
        title: clean(stem),
        season: None,
        episode: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_release_names() {
        let info = parse_filename("The.Last.of.Us.S01E03.1080p.WEB.h264-GROUP.mkv");
        assert_eq!(info.title, "The Last of Us");
        assert_eq!(info.season, Some(1));
        assert_eq!(info.episode, Some(3));

        let info = parse_filename("severance.s02e01.2160p.web-dl.mkv");
        assert_eq!(info.title, "severance");
        assert_eq!(info.episode, Some(1));

        let info = parse_filename("Some Show 2x05 HDTV.mp4");
        assert_eq!(info.title, "Some Show");
        assert_eq!(info.season, Some(2));
        assert_eq!(info.episode, Some(5));

        let info = parse_filename("Movie.2024.mkv");
        assert_eq!(info.title, "Movie 2024");
        assert_eq!(info.season, None);
    }
}
