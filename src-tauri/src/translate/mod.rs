//! LLM 翻译管线（gpt-subtrans 策略的移植）：
//! 分块（两级：60s 间隙切场景 + 最大间隙递归二分 10~30 行）、摘要链上下文、
//! 术语表防伪、错误明细重试（升温）。autosplit 默认不做——与 gpt-subtrans 默认一致。

pub mod llm;
pub mod parser;
pub mod prompt;

use std::collections::{BTreeMap, HashMap};

use crate::subtitle::SubtitleLine;

#[derive(Debug, Clone)]
pub struct Batch {
    pub scene: u32,
    pub batch: u32,
    pub lines: Vec<SubtitleLine>,
}

pub fn build_batches(
    lines: &[SubtitleLine],
    scene_threshold_ms: i64,
    min_batch: usize,
    max_batch: usize,
) -> Vec<Batch> {
    let mut batches = Vec::new();
    let mut scene_start = 0usize;
    let mut scene_no = 0u32;
    let mut prev_end: Option<i64> = None;

    for (i, line) in lines.iter().enumerate() {
        if let Some(pe) = prev_end {
            if line.start_ms - pe > scene_threshold_ms {
                scene_no += 1;
                append_scene_batches(&mut batches, scene_no, &lines[scene_start..i], min_batch, max_batch);
                scene_start = i;
            }
        }
        prev_end = Some(line.end_ms);
    }
    if scene_start < lines.len() {
        scene_no += 1;
        append_scene_batches(&mut batches, scene_no, &lines[scene_start..], min_batch, max_batch);
    }
    batches
}

fn append_scene_batches(
    out: &mut Vec<Batch>,
    scene: u32,
    lines: &[SubtitleLine],
    min_batch: usize,
    max_batch: usize,
) {
    let mut batch_no = 0u32;
    for chunk in split_at_largest_gaps(lines, min_batch, max_batch) {
        batch_no += 1;
        out.push(Batch {
            scene,
            batch: batch_no,
            lines: chunk,
        });
    }
}

fn split_at_largest_gaps(lines: &[SubtitleLine], min_batch: usize, max_batch: usize) -> Vec<Vec<SubtitleLine>> {
    if lines.len() <= max_batch {
        return vec![lines.to_vec()];
    }
    // 在 [min, len-min) 范围内找最大时间间隙，保证两侧 >= min
    let mut best = min_batch;
    let mut best_gap = i64::MIN;
    let last = lines.len().saturating_sub(min_batch);
    if last > min_batch {
        for i in min_batch..last {
            let gap = lines[i].start_ms - lines[i - 1].end_ms;
            if gap > best_gap {
                best_gap = gap;
                best = i;
            }
        }
    }
    let mut left = split_at_largest_gaps(&lines[..best], min_batch, max_batch);
    left.extend(split_at_largest_gaps(&lines[best..], min_batch, max_batch));
    left
}

// ---------- 编排器 ----------

#[derive(Debug, Clone, serde::Serialize)]
pub struct TranslateProgress {
    pub done_batches: usize,
    pub total_batches: usize,
    pub failed_lines: usize,
}

#[derive(Debug, Default)]
pub struct TranslateOutcome {
    /// number -> 译文；失败的行填占位文本，保证编号完整
    pub translations: BTreeMap<u32, String>,
    pub failed: Vec<u32>,
}

/// 整集翻译：串行逐批（串行对一集 20~30 批的规模已够；并发后续再加）
pub async fn translate_all<C: llm::Chat>(
    client: &C,
    lines: &[SubtitleLine],
    scene_threshold_ms: i64,
    min_batch: usize,
    max_batch: usize,
    mut on_progress: impl FnMut(TranslateProgress),
) -> TranslateOutcome {
    let batches = build_batches(lines, scene_threshold_ms, min_batch, max_batch);
    let total_batches = batches.len();
    let mut out = TranslateOutcome::default();
    let mut history: Vec<String> = Vec::new(); // 摘要链（最多 10 条）
    let mut terminology: HashMap<String, String> = HashMap::new();

    for (i, batch) in batches.iter().enumerate() {
        on_progress(TranslateProgress {
            done_batches: i,
            total_batches,
            failed_lines: out.failed.len(),
        });

        let expected: Vec<u32> = batch.lines.iter().map(|l| l.number).collect();
        let originals = batch
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        let (parsed, errors) = translate_batch(client, batch, &history, &terminology, &expected).await;

        if errors.is_empty() {
            // 术语防伪后并入表（已有条目不覆盖，先到先得）
            let translated_text = parsed
                .translations
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            for (en, zh) in parser::sanitize_terminology(&parsed, &originals, &translated_text) {
                terminology.entry(en).or_insert(zh);
            }
            if let Some(s) = parsed.summary {
                history.push(s);
                if history.len() > 10 {
                    history.remove(0);
                }
            }
        }

        // 已翻的行入库（部分成功也算），缺失的行占位并记入 failed
        for n in &expected {
            match parsed.translations.get(n) {
                Some(t) if !t.trim().is_empty() => {
                    out.translations.insert(*n, t.clone());
                }
                _ => {
                    out.translations
                        .insert(*n, "[翻译失败，可重试]".to_string());
                    out.failed.push(*n);
                }
            }
        }
    }

    on_progress(TranslateProgress {
        done_batches: total_batches,
        total_batches,
        failed_lines: out.failed.len(),
    });
    out
}

/// 单批翻译：首次 → 带错误明细重试（升温 0.1）。返回（解析结果，最终错误列表）
async fn translate_batch<C: llm::Chat>(
    client: &C,
    batch: &Batch,
    history: &[String],
    terminology: &HashMap<String, String>,
    expected: &[u32],
) -> (parser::ParsedBatch, Vec<String>) {
    let mut last = match client
        .chat(
            prompt::SYSTEM_PROMPT,
            &prompt::build_user_prompt(&batch.lines, history, terminology, None),
            0.3,
        )
        .await
    {
        Ok(resp) => {
            let parsed = parser::parse_response(&resp);
            let errors = parser::validate(&parsed, expected);
            if errors.is_empty() {
                return (parsed, errors);
            }
            (parsed, errors)
        }
        Err(e) => (
            parser::ParsedBatch::default(),
            vec![format!("api error: {e}")],
        ),
    };

    // 重试一次：附上错误明细，温度 +0.1
    let retry_user = prompt::build_user_prompt(&batch.lines, history, terminology, Some(&last.1));
    if let Ok(resp) = client.chat(prompt::SYSTEM_PROMPT, &retry_user, 0.4).await {
        let parsed = parser::parse_response(&resp);
        let errors = parser::validate(&parsed, expected);
        // 合并两次结果：重试优先，首次补齐
        let mut merged = parsed;
        for (n, t) in last.0.translations {
            merged.translations.entry(n).or_insert(t);
        }
        last = (merged, errors);
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(number: u32, start_ms: i64, end_ms: i64) -> SubtitleLine {
        SubtitleLine {
            number,
            start_ms,
            end_ms,
            text: format!("line {number}"),
        }
    }

    /// 构造 n 句连续对话（句间隔 500ms）
    fn dialogue(n: u32, from_ms: i64) -> Vec<SubtitleLine> {
        (0..n)
            .map(|i| {
                let start = from_ms + i as i64 * 3500;
                line(i + 1, start, start + 3000)
            })
            .collect()
    }

    #[test]
    fn splits_scenes_at_big_gaps() {
        let mut lines = dialogue(5, 0);
        lines.extend(dialogue(5, 5 * 3500 + 120_000)); // 第二场景隔 120s
        let batches = build_batches(&lines, 60_000, 10, 30);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].scene, 1);
        assert_eq!(batches[1].scene, 2);
    }

    #[test]
    fn splits_oversized_scene_at_largest_gap() {
        let mut lines = dialogue(25, 0); // 连续 25 句，超过 max=20
        // 在第 12 句后制造一个更大的间隙
        let offset = 10_000i64;
        for l in lines.iter_mut().skip(12) {
            l.start_ms += offset;
            l.end_ms += offset;
        }
        let batches = build_batches(&lines, 60_000, 10, 20);
        assert_eq!(batches.len(), 2);
        assert!(batches.iter().all(|b| b.lines.len() >= 10 && b.lines.len() <= 20));
        // 应该在第 12/13 句之间断开
        assert_eq!(batches[0].lines.len(), 12);
    }

    // ---------- 编排器（mock LLM） ----------

    struct MockLlm {
        responses: Vec<String>,
        calls: std::sync::atomic::AtomicUsize,
        prompts: std::sync::Mutex<Vec<String>>,
    }

    impl MockLlm {
        fn new(responses: Vec<&str>) -> Self {
            Self {
                responses: responses.into_iter().map(String::from).collect(),
                calls: Default::default(),
                prompts: Default::default(),
            }
        }
    }

    impl llm::Chat for MockLlm {
        async fn chat(
            &self,
            _system: &str,
            user: &str,
            _temperature: f32,
        ) -> Result<String, llm::LlmError> {
            let i = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.prompts.lock().unwrap().push(user.to_string());
            self.responses
                .get(i)
                .cloned()
                .ok_or_else(|| llm::LlmError::Api("no more responses".into()))
        }
    }

    #[tokio::test]
    async fn orchestrator_retries_and_chains_context() {
        // 两场景各 2 句（间隔 70s > 阈值 60s）
        let mut lines = dialogue(2, 0);
        lines.extend(dialogue(2, 70_000));
        for (i, l) in lines.iter_mut().enumerate() {
            l.number = (i + 1) as u32;
        }

        let mock = MockLlm::new(vec![
            // 批1首次：缺 #2，触发重试
            "#1\nOriginal>\nline 1\nTranslation>\n第一句\n",
            // 批1重试：完整 + 摘要 + 术语
            "#1\nOriginal>\nline 1\nTranslation>\n第一句\n\n#2\nOriginal>\nline 2\nTranslation>\n第二句\n\n<summary>Two opening lines.</summary>\n<terminology>\n</terminology>",
            // 批2首次即完整
            "#3\nOriginal>\nline 3\nTranslation>\n第三句\n\n#4\nOriginal>\nline 4\nTranslation>\n第四句\n",
        ]);

        let mut progress = Vec::new();
        let out = translate_all(&mock, &lines, 60_000, 10, 30, |p| progress.push(p)).await;

        assert!(out.failed.is_empty(), "所有行都应翻译成功: {:?}", out.failed);
        assert_eq!(out.translations.len(), 4);
        assert_eq!(out.translations[&2], "第二句");

        let prompts = mock.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3, "批1首次+重试，批2一次");
        // 重试 prompt 带错误明细
        assert!(prompts[1].contains("missing translation for line #2"));
        // 批2 prompt 带批1的摘要（上下文链生效）
        assert!(prompts[2].contains("Two opening lines."));
        // 进度回调：首帧 total=2，末帧 done=2
        assert_eq!(progress.first().unwrap().total_batches, 2);
        assert_eq!(progress.last().unwrap().done_batches, 2);
    }

    #[tokio::test]
    async fn orchestrator_fills_placeholder_on_failure() {
        let lines = dialogue(2, 0);
        let mock = MockLlm::new(vec!["garbage without lines", "still garbage"]);
        let out = translate_all(&mock, &lines, 60_000, 10, 30, |_| {}).await;
        assert_eq!(out.failed.len(), 2);
        assert!(out.translations[&1].contains("翻译失败"));
        assert!(out.translations[&2].contains("翻译失败"));
    }
}
