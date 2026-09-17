//! 提示词构建：行格式约定、上下文（摘要链 + 术语表）、重试错误明细。

use std::collections::HashMap;

use crate::subtitle::SubtitleLine;

pub const SYSTEM_PROMPT: &str = r#"You are a professional subtitle translator. Translate the subtitles from English to Simplified Chinese.

Rules:
- Output every line in this exact format:
#<number>
Original>
<original text>
Translation>
<translated text>
- Translate EVERY line. Keep numbers unchanged. Never merge or split lines.
- 每条 Translation> 下只输出译文，允许连续多行，但段内不得出现空行。不要添加注意事项、翻译解释、前言或 Markdown 代码围栏；术语只放在末尾的 <terminology> 块中。
- Use natural, colloquial Chinese for TV dialogue; concise and faithful; adapt idioms instead of literal translation.
- After all translated lines, output exactly one summary line:
<summary>one English sentence summarizing this batch, as context for later batches</summary>
- Then list proper nouns / fixed terms appearing in THIS batch (one per line, english::中文):
<terminology>
</terminology>
If none, leave the terminology block empty."#;

pub fn build_user_prompt(
    lines: &[SubtitleLine],
    history: &[String],
    terminology: &HashMap<String, String>,
    retry_errors: Option<&[String]>,
) -> String {
    let mut s = String::new();
    if !history.is_empty() {
        s.push_str("Context from previous batches:\n");
        for (i, h) in history.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", i + 1, h));
        }
        s.push('\n');
    }
    if !terminology.is_empty() {
        s.push_str("Terminology (use these translations consistently):\n");
        for (en, zh) in terminology {
            s.push_str(&format!("{en}::{zh}\n"));
        }
        s.push('\n');
    }
    if let Some(errors) = retry_errors {
        s.push_str("Your previous response had these problems — fix them:\n");
        for e in errors {
            s.push_str(&format!("- {e}\n"));
        }
        s.push_str("Do NOT merge lines; every number must appear exactly once.\n\n");
    }
    s.push_str("Translate these lines:\n");
    for l in lines {
        s.push_str(&format!(
            "#{}\nOriginal>\n{}\nTranslation>\n\n",
            l.number,
            l.text.replace('\n', " ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_context_and_lines() {
        let lines = vec![SubtitleLine {
            number: 7,
            start_ms: 1000,
            end_ms: 2000,
            text: "Hello.".into(),
        }];
        let mut terms = HashMap::new();
        terms.insert("Rachel".to_string(), "瑞秋".to_string());
        let p = build_user_prompt(&lines, &["They broke up.".to_string()], &terms, None);
        assert!(p.contains("Context from previous batches"));
        assert!(p.contains("Rachel::瑞秋"));
        assert!(p.contains("#7\nOriginal>\nHello.\nTranslation>"));
    }
}
