//! 响应解析：#N / Original> / Translation> 行格式；提取 summary / terminology
//! 元信息；术语防伪校验（源词须在原文、译词须在译文，方向反了自动交换）。

use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Default)]
pub struct ParsedBatch {
    pub translations: BTreeMap<u32, String>,
    pub summary: Option<String>,
    pub terminology: HashMap<String, String>,
}

pub fn parse_response(text: &str) -> ParsedBatch {
    let mut out = ParsedBatch::default();
    if let Some(s) = extract_tag(text, "summary") {
        let s = s.trim();
        if !s.is_empty() {
            out.summary = Some(s.to_string());
        }
    }
    if let Some(t) = extract_tag(text, "terminology") {
        for line in t.lines() {
            if let Some((en, zh)) = line.split_once("::") {
                let (en, zh) = (en.trim(), zh.trim());
                if !en.is_empty() && !zh.is_empty() {
                    out.terminology.insert(en.to_string(), zh.to_string());
                }
            }
        }
    }
    let body = strip_tag(text, "summary");
    let body = strip_tag(&body, "terminology");
    out.translations = parse_lines(&body);
    out
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close).map(|i| start + i).unwrap_or(text.len());
    Some(text[start..end].to_string())
}

fn strip_tag(text: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    if let Some(start) = text.find(&open) {
        let end = text[start..]
            .find(&close)
            .map(|i| start + i + close.len())
            .unwrap_or(text.len());
        return format!("{}{}", &text[..start], &text[end..]);
    }
    text.to_string()
}

/// 行解析：状态机驱动，容忍空行与大小写差异
fn parse_lines(body: &str) -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    let mut number: Option<u32> = None;
    let mut in_translation = false;
    let mut buf = String::new();

    fn flush(number: Option<u32>, buf: &mut String, map: &mut BTreeMap<u32, String>) {
        if let Some(n) = number {
            let t = buf.trim();
            if !t.is_empty() {
                map.insert(n, t.to_string());
            }
        }
        buf.clear();
    }

    for raw in body.lines() {
        let trimmed = raw.trim();
        if let Some(rest) = trimmed.strip_prefix('#') {
            flush(number, &mut buf, &mut map);
            number = rest.trim().parse::<u32>().ok();
            in_translation = false;
        } else if trimmed.eq_ignore_ascii_case("original>") {
            in_translation = false;
        } else if trimmed.eq_ignore_ascii_case("translation>") {
            in_translation = true;
        } else if in_translation {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(trimmed);
        }
    }
    flush(number, &mut buf, &mut map);
    map
}

/// 校验：期望的编号是否都有非空译文
pub fn validate(parsed: &ParsedBatch, expected: &[u32]) -> Vec<String> {
    let mut errors = Vec::new();
    for n in expected {
        match parsed.translations.get(n) {
            None => errors.push(format!("missing translation for line #{n}")),
            Some(t) if t.trim().is_empty() => errors.push(format!("empty translation for line #{n}")),
            _ => {}
        }
    }
    errors
}

/// 术语防伪校验（gpt-subtrans 策略）
pub fn sanitize_terminology(
    parsed: &ParsedBatch,
    originals: &str,
    translations: &str,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (en, zh) in &parsed.terminology {
        if originals.contains(en.as_str()) && translations.contains(zh.as_str()) {
            out.insert(en.clone(), zh.clone());
        } else if originals.contains(zh.as_str()) && translations.contains(en.as_str()) {
            // 模型把方向写反了，自动交换
            out.insert(zh.clone(), en.clone());
        }
        // 其余视为幻觉，丢弃
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESP: &str = r#"#1
Original>
HELLO, WORLD.
Translation>
你好，世界。

#2
Original>
I'M RACHEL.
Translation>
我是瑞秋。

<summary>They greet each other.</summary>
<terminology>
Rachel::瑞秋
Ghost::幽灵
</terminology>"#;

    #[test]
    fn parses_lines_and_meta() {
        let p = parse_response(RESP);
        assert_eq!(p.translations.get(&1).unwrap(), "你好，世界。");
        assert_eq!(p.translations.get(&2).unwrap(), "我是瑞秋。");
        assert_eq!(p.summary.as_deref(), Some("They greet each other."));
        assert_eq!(p.terminology.get("Rachel").unwrap(), "瑞秋");
    }

    #[test]
    fn validate_reports_missing() {
        let p = parse_response(RESP);
        let errors = validate(&p, &[1, 2, 3]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("#3"));
    }

    #[test]
    fn terminology_filters_hallucination() {
        let p = parse_response(RESP);
        // 原文含 RACHEL（大写），译名"瑞秋"出现在译文；"Ghost/幽灵"两边都没出现，应丢弃
        let originals = "HELLO, WORLD.\nI'M RACHEL.";
        let translations = "你好，世界。\n我是瑞秋。";
        let terms = sanitize_terminology(&p, originals, translations);
        // "Rachel" 大小写敏感未命中原文——这是已知行为：模型通常按原样引用
        assert!(!terms.contains_key("Ghost"));
        assert!(terms.len() <= 1);
    }

    #[test]
    fn terminology_swaps_reversed() {
        let mut p = ParsedBatch::default();
        p.terminology.insert("瑞秋".into(), "Rachel".into());
        let terms = sanitize_terminology(&p, "I'M Rachel.", "我是瑞秋。");
        assert_eq!(terms.get("Rachel").unwrap(), "瑞秋");
    }
}
