//! 字幕解析与处理：SRT 解析、全大写还原（规则法）、当前句反查。

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SubtitleError {
    #[error("invalid SRT: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleLine {
    pub number: u32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// 解析 "HH:MM:SS,mmm"（也兼容 '.' 分隔毫秒）
pub fn parse_timestamp(ts: &str) -> Option<i64> {
    let ts = ts.trim().replace('.', ",");
    let mut parts = ts.splitn(2, ',');
    let hms = parts.next()?;
    let ms: i64 = parts.next().unwrap_or("0").trim().parse().ok()?;
    let mut it = hms.split(':');
    let h: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.trim().parse().ok()?;
    let s: i64 = it.next()?.trim().parse().ok()?;
    Some(((h * 60 + m) * 60 + s) * 1000 + ms)
}

pub fn format_timestamp(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

pub fn parse_srt(content: &str) -> Result<Vec<SubtitleLine>, SubtitleError> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = Vec::new();
    for block in normalized.split("\n\n") {
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        let mut block_lines = block.lines();
        let number: u32 = block_lines
            .next()
            .unwrap_or("0")
            .trim()
            .parse()
            .map_err(|_| SubtitleError::Invalid(format!("bad index in block: {block}")))?;
        let timing = block_lines
            .next()
            .ok_or_else(|| SubtitleError::Invalid("missing timing line".into()))?;
        let mut parts = timing.splitn(2, "-->");
        let start = parts
            .next()
            .and_then(parse_timestamp)
            .ok_or_else(|| SubtitleError::Invalid(format!("bad start time: {timing}")))?;
        let end_raw = parts
            .next()
            .ok_or_else(|| SubtitleError::Invalid(format!("missing --> : {timing}")))?;
        // 时间戳后可能跟随位置信息（如 X1:.. X2:..），只取第一个 token
        let end = parse_timestamp(end_raw.split_whitespace().next().unwrap_or(""))
            .ok_or_else(|| SubtitleError::Invalid(format!("bad end time: {timing}")))?;
        let text = block_lines.collect::<Vec<_>>().join("\n");
        lines.push(SubtitleLine {
            number,
            start_ms: start,
            end_ms: end,
            text,
        });
    }
    Ok(lines)
}

/// 仅用于生成的译文缓存：跳过旧模型输出混入的说明段和损坏块，保留可续翻的有效行。
/// 原文字幕仍使用严格的 parse_srt，避免静默丢失源台词。
pub fn parse_translated_srt(content: &str) -> Vec<SubtitleLine> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    normalized
        .split("\n\n")
        .filter_map(|block| parse_srt(block).ok())
        .flatten()
        .filter(|line| !line.text.trim().is_empty())
        .collect()
}

pub fn to_srt(lines: &[SubtitleLine]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                i + 1,
                format_timestamp(l.start_ms),
                format_timestamp(l.end_ms),
                l.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 规则法全大写还原：已含小写字母的文本原样返回；否则整体小写后，
/// 句首大写，并修复独立的 i 与 i'm/i'll/i've/i'd 等缩略形式。
/// 已知局限：人名/专名（RACHEL → Rachel 无法还原为 Rachel，会变成 rachel）、
/// "Mr." 等缩写点号会误触发句首大写——如需完美还原可切换 LLM 模式。
pub fn truecase(text: &str) -> String {
    if text.chars().any(|c| c.is_lowercase()) {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut cap_next = true;
    for c in lower.chars() {
        if cap_next && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            cap_next = false;
        } else {
            out.push(c);
        }
        if matches!(c, '.' | '?' | '!') {
            cap_next = true;
        }
    }
    fix_i_pronoun(&out)
}

fn fix_i_pronoun(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == 'i' {
            let prev_ok = i == 0 || !chars[i - 1].is_alphabetic();
            let next = chars.get(i + 1).copied();
            let standalone = next.is_none() || !next.unwrap().is_alphabetic();
            if prev_ok && standalone {
                result.push('I');
                continue;
            }
        }
        result.push(c);
    }
    result
}

/// 用播放位置反查当前句：最后一个 start <= pos 的行（不依赖 mpv 的 sub-text）
pub fn find_current(lines: &[SubtitleLine], pos_ms: i64) -> Option<usize> {
    let idx = lines.partition_point(|l| l.start_ms <= pos_ms);
    if idx == 0 {
        None
    } else {
        Some(idx - 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "1\n00:00:01,500 --> 00:00:04,000\nHELLO, WORLD.\n\n2\n00:00:05.250 --> 00:00:07,000\nI'M FINE. THANK YOU.\n";

    #[test]
    fn parses_srt_blocks() {
        let lines = parse_srt(SAMPLE).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].start_ms, 1500);
        assert_eq!(lines[0].end_ms, 4000);
        assert_eq!(lines[1].start_ms, 5250);
        assert_eq!(lines[1].text, "I'M FINE. THANK YOU.");
    }

    #[test]
    fn translated_cache_recovers_valid_cues_around_notes() {
        let content = "7\n00:00:01,500 --> 00:00:04,000\n你太不负责任了。\n\n注意:irresponsible 译作“不负责任”.\n\n42\n00:00:05,250 --> 00:00:07,000\n- 对不起。\n- 没关系。\n\n额外说明";
        assert!(parse_srt(content).is_err(), "原文解析仍须严格校验");
        for content in [content.to_string(), content.replace('\n', "\r\n")] {
            let lines = parse_translated_srt(&content);
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0].number, 7);
            assert_eq!(lines[0].start_ms, 1500);
            assert_eq!(lines[0].end_ms, 4000);
            assert_eq!(lines[0].text, "你太不负责任了。");
            assert_eq!(lines[1].number, 42);
            assert_eq!(lines[1].text, "- 对不起。\n- 没关系。");
        }
    }

    #[test]
    fn translated_cache_skips_broken_and_empty_cues_for_retry() {
        let content = format!(
            "说明段\n\n{SAMPLE}\n\n3\nbad timing\n无效译文\n\n4\n00:00:08,000 --> 00:00:09,000\n\n"
        );
        let lines = parse_translated_srt(&content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].number, 1);
        assert_eq!(lines[1].number, 2);
        assert!(parse_translated_srt("只有说明，没有字幕").is_empty());
    }

    #[test]
    fn truecases_all_caps() {
        assert_eq!(truecase("HELLO, WORLD."), "Hello, world.");
        assert_eq!(truecase("WHO ARE YOU? I'M RACHEL."), "Who are you? I'm rachel.");
        assert_eq!(truecase("Yes, I see."), "Yes, I see."); // 已有小写则原样返回
    }

    #[test]
    fn finds_current_line() {
        let lines = parse_srt(SAMPLE).unwrap();
        assert_eq!(find_current(&lines, 500), None);
        assert_eq!(find_current(&lines, 2000), Some(0));
        assert_eq!(find_current(&lines, 6000), Some(1));
    }
}
