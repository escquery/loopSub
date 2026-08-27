//! LLM 翻译管线（gpt-subtrans 策略的移植）：
//! 分块（两级：60s 间隙切场景 + 最大间隙递归二分 10~30 行）、摘要链上下文、
//! 术语表防伪、错误明细重试（升温）。autosplit 默认不做——与 gpt-subtrans 默认一致。
//! 执行：限并发（默认 4），摘要池/术语表宽松共享；批粒度断点续翻 + 行内容级缓存。

pub mod llm;
pub mod parser;
pub mod prompt;

use std::collections::{BTreeMap, HashMap};

use crate::subtitle::SubtitleLine;

pub const FAILED_TRANSLATION: &str = "[翻译失败，可重试]";

pub fn is_failed_translation(text: &str) -> bool {
    text.trim() == FAILED_TRANSLATION
}

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

/// 行内容级译文缓存：cache/lines/<model>.jsonl。
/// 跨视频复用（换源字幕大部分行可复用）；占位符行不写入（失败后须可重试）。
pub struct LineCache {
    map: HashMap<u64, String>,
    path: std::path::PathBuf,
}

impl LineCache {
    pub fn load(path: std::path::PathBuf) -> Self {
        let mut map = HashMap::new();
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                    if let (Some(h), Some(zh)) = (v["h"].as_u64(), v["zh"].as_str()) {
                        map.insert(h, zh.to_string());
                    }
                }
            }
        }
        Self { map, path }
    }

    pub fn get(&self, original: &str) -> Option<&String> {
        self.map.get(&line_hash(original))
    }

    pub fn insert(&mut self, original: &str, zh: &str) {
        let h = line_hash(original);
        if self.map.contains_key(&h) {
            return;
        }
        self.map.insert(h, zh.to_string());
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            use std::io::Write;
            let _ = writeln!(f, "{}", serde_json::json!({"h": h, "zh": zh}));
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// FNV-1a 64：稳定且跨 Rust 版本一致（DefaultHasher 不保证，升级即缓存失效）
fn line_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 全部行内容指纹：断点进度文件据此判断字幕是否换源（换源则旧进度作废）
pub fn lines_fingerprint(lines: &[SubtitleLine]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for l in lines {
        for b in l.text.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^= 0xff; // 行分隔，防拼接歧义
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// 翻译过程事件（仅编排器主任务触发，回调无需 Send）
pub enum TranslateEvent {
    /// 进度快照（前端 translate-progress 事件 payload）
    Progress(TranslateProgress),
    /// 某批完成：行号 -> 译文（仅成功行 + 行缓存命中行；外层据此落盘断点进度）
    BatchDone(u32, BTreeMap<u32, String>),
}

/// 将已生成 SRT 中的成功译文并入批断点。占位符不并入，因此再次执行整集
/// 翻译时只会请求失败/缺失行；已有中断进度优先于旧 SRT。
pub fn merge_cached_translations_into_resume(
    lines: &[SubtitleLine],
    cached: &BTreeMap<u32, String>,
    scene_threshold_ms: i64,
    min_batch: usize,
    max_batch: usize,
    resume: &mut BTreeMap<u32, BTreeMap<u32, String>>,
) {
    for (idx, batch) in build_batches(lines, scene_threshold_ms, min_batch, max_batch)
        .into_iter()
        .enumerate()
    {
        let finished = resume.entry(idx as u32).or_default();
        for line in batch.lines {
            if let Some(zh) = cached
                .get(&line.number)
                .filter(|zh| !is_failed_translation(zh))
            {
                finished.entry(line.number).or_insert_with(|| zh.clone());
            }
        }
    }
    resume.retain(|_, rows| !rows.is_empty());
}

pub struct TranslateOpts<'a, C: llm::Chat + Clone + 'static> {
    pub client: &'a C,
    pub lines: &'a [SubtitleLine],
    pub scene_threshold_ms: i64,
    pub min_batch: usize,
    pub max_batch: usize,
    /// 并发批数（设置页 llm.concurrency）；1 = 严格串行（摘要链完整）
    pub concurrency: usize,
    /// 断点续翻：已完成批（批号 = build_batches 顺序号；行号 -> 译文，仅成功行）
    pub resume: BTreeMap<u32, BTreeMap<u32, String>>,
    pub line_cache: Option<&'a mut LineCache>,
}

/// 整集翻译：限并发执行。
/// 摘要链宽松化：并发批从“已完成批的摘要池”取最近 10 条，窗口内互不可见——
/// 场景切分本就弱化跨批依赖（gpt-subtrans 同款取舍）；concurrency=1 时严格串行。
/// 结果按行号 BTreeMap 合并，与完成顺序无关。
pub async fn translate_all<C: llm::Chat + Clone + 'static>(
    mut opts: TranslateOpts<'_, C>,
    mut on_event: impl FnMut(TranslateEvent),
) -> TranslateOutcome {
    let batches = build_batches(opts.lines, opts.scene_threshold_ms, opts.min_batch, opts.max_batch);
    let total = batches.len();
    let mut out = TranslateOutcome::default();
    let mut done = 0usize;
    // 待执行批在预处理阶段已完成的行（续翻 + 缓存命中），批完成时合并落盘
    let mut cached_parts: HashMap<u32, BTreeMap<u32, String>> = HashMap::new();
    let mut pending: Vec<(u32, Batch)> = Vec::new();

    for (idx, batch) in batches.into_iter().enumerate() {
        let idx = idx as u32;
        let mut finished: BTreeMap<u32, String> = BTreeMap::new();
        let mut lines_todo = batch.lines.clone();

        // 断点续翻：已成功行直接回填（上次失败的行不在进度里，自然进入重翻）
        let resume_len = opts.resume.get(&idx).map_or(0, |m| m.len());
        if let Some(done_lines) = opts.resume.get(&idx) {
            for (n, t) in done_lines {
                out.translations.insert(*n, t.clone());
                finished.insert(*n, t.clone());
            }
            lines_todo.retain(|l| !done_lines.contains_key(&l.number));
        }
        // 行内容缓存：命中行直接出结果，残余行才调 LLM
        if let Some(cache) = opts.line_cache.as_ref() {
            let mut rest = Vec::with_capacity(lines_todo.len());
            for l in lines_todo {
                match cache.get(&l.text) {
                    Some(zh) => {
                        out.translations.insert(l.number, zh.clone());
                        finished.insert(l.number, zh.clone());
                    }
                    None => rest.push(l),
                }
            }
            lines_todo = rest;
        }

        if lines_todo.is_empty() {
            done += 1;
            if finished.len() > resume_len {
                // 有新增完成行（缓存补齐）→ 落盘；纯续翻跳过（数据已在盘上）
                on_event(TranslateEvent::BatchDone(idx, finished));
            }
            on_event(TranslateEvent::Progress(TranslateProgress {
                done_batches: done,
                total_batches: total,
                failed_lines: out.failed.len(),
            }));
            continue;
        }
        if !finished.is_empty() {
            cached_parts.insert(idx, finished);
        }
        pending.push((idx, Batch {
            lines: lines_todo,
            ..batch
        }));
    }

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(opts.concurrency.max(1)));
    let history = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let terminology = std::sync::Arc::new(std::sync::Mutex::new(HashMap::<String, String>::new()));
    let mut set = tokio::task::JoinSet::new();

    for (idx, batch) in pending {
        let client = opts.client.clone();
        let sem = sem.clone();
        let history = history.clone();
        let terminology = terminology.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore closed");
            // 快照此刻已完成的摘要/术语（并发窗口内互不可见，尽力而为）
            let hist = history.lock().unwrap().clone();
            let term = terminology.lock().unwrap().clone();
            let expected: Vec<u32> = batch.lines.iter().map(|l| l.number).collect();
            let originals_joined = batch
                .lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let (parsed, errors) = translate_batch(&client, &batch, &hist, &term, &expected).await;

            if errors.is_empty() {
                // 术语防伪后并入共享表（先到先得）；摘要入池（上限 10 条）
                let translated_joined = parsed
                    .translations
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                {
                    let mut t = terminology.lock().unwrap();
                    for (en, zh) in parser::sanitize_terminology(&parsed, &originals_joined, &translated_joined) {
                        t.entry(en).or_insert(zh);
                    }
                }
                if let Some(s) = &parsed.summary {
                    let mut h = history.lock().unwrap();
                    h.push(s.clone());
                    if h.len() > 10 {
                        h.remove(0);
                    }
                }
            }

            let rows: Vec<(u32, String, Option<String>)> = batch
                .lines
                .iter()
                .map(|l| {
                    let zh = parsed
                        .translations
                        .get(&l.number)
                        .filter(|t| !t.trim().is_empty())
                        .cloned();
                    (l.number, l.text.clone(), zh)
                })
                .collect();
            (idx, rows)
        });
    }

    while let Some(joined) = set.join_next().await {
        let (idx, rows) = joined.expect("translate task panicked");
        let mut batch_map = cached_parts.remove(&idx).unwrap_or_default();
        for (n, original, zh) in rows {
            match zh {
                Some(t) => {
                    out.translations.insert(n, t.clone());
                    batch_map.insert(n, t.clone());
                    if let Some(c) = opts.line_cache.as_mut() {
                        c.insert(&original, &t);
                    }
                }
                None => {
                    out.translations.insert(n, FAILED_TRANSLATION.to_string());
                    out.failed.push(n);
                }
            }
        }
        done += 1;
        on_event(TranslateEvent::BatchDone(idx, batch_map));
        on_event(TranslateEvent::Progress(TranslateProgress {
            done_batches: done,
            total_batches: total,
            failed_lines: out.failed.len(),
        }));
    }

    out.failed.sort_unstable(); // 并发完成顺序不定，输出保持确定性
    on_event(TranslateEvent::Progress(TranslateProgress {
        done_batches: total,
        total_batches: total,
        failed_lines: out.failed.len(),
    }));
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

    /// 共享内部状态（Clone 后仍指向同一份，模拟同一客户端的并发调用）
    #[derive(Clone)]
    struct MockLlm {
        inner: std::sync::Arc<MockInner>,
    }

    struct MockInner {
        scripted: Vec<String>,
        echo: bool,
        delay: std::time::Duration,
        calls: std::sync::atomic::AtomicUsize,
        prompts: std::sync::Mutex<Vec<String>>,
        in_flight: std::sync::atomic::AtomicUsize,
        max_in_flight: std::sync::atomic::AtomicUsize,
    }

    impl MockLlm {
        /// 预置响应模式：第 i 次调用返回第 i 条（串行测试用，顺序确定）
        fn scripted(responses: Vec<&str>) -> Self {
            Self {
                inner: std::sync::Arc::new(MockInner {
                    scripted: responses.into_iter().map(String::from).collect(),
                    echo: false,
                    delay: std::time::Duration::ZERO,
                    calls: Default::default(),
                    prompts: Default::default(),
                    in_flight: Default::default(),
                    max_in_flight: Default::default(),
                }),
            }
        }

        /// 回显模式：从 prompt 提取每行 #n/Original> 生成对应 Translation
        ///（并发测试用——完成顺序不定，不能依赖预置序号）
        fn echo(delay_ms: u64) -> Self {
            let mut m = Self::scripted(vec![]);
            let inner = std::sync::Arc::get_mut(&mut m.inner).unwrap();
            inner.echo = true;
            inner.delay = std::time::Duration::from_millis(delay_ms);
            m
        }

        fn calls(&self) -> usize {
            self.inner.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn max_in_flight(&self) -> usize {
            self.inner.max_in_flight.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    fn echo_response(user: &str) -> String {
        let re = regex::Regex::new(r"#(\d+)\nOriginal>\n([^\n]+)\nTranslation>").unwrap();
        let mut s = String::new();
        for cap in re.captures_iter(user) {
            s.push_str(&format!(
                "#{}\nOriginal>\n{}\nTranslation>\n译{}\n\n",
                &cap[1], &cap[2], &cap[2]
            ));
        }
        s.push_str("<summary>echo batch summary</summary>\n<terminology>\n</terminology>");
        s
    }

    impl llm::Chat for MockLlm {
        async fn chat(
            &self,
            _system: &str,
            user: &str,
            _temperature: f32,
        ) -> Result<String, llm::LlmError> {
            let cur = self.inner.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            self.inner
                .max_in_flight
                .fetch_max(cur, std::sync::atomic::Ordering::SeqCst);
            if self.inner.delay > std::time::Duration::ZERO {
                tokio::time::sleep(self.inner.delay).await;
            }
            let i = self.inner.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.prompts.lock().unwrap().push(user.to_string());
            let resp = if self.inner.echo {
                Ok(echo_response(user))
            } else {
                self.inner
                    .scripted
                    .get(i)
                    .cloned()
                    .ok_or_else(|| llm::LlmError::Api("no more responses".into()))
            };
            self.inner.in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            resp
        }
    }

    fn opts<'a, C: llm::Chat + Clone>(client: &'a C, lines: &'a [SubtitleLine]) -> TranslateOpts<'a, C> {
        TranslateOpts {
            client,
            lines,
            scene_threshold_ms: 60_000,
            min_batch: 10,
            max_batch: 30,
            concurrency: 1,
            resume: Default::default(),
            line_cache: None,
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

        let mock = MockLlm::scripted(vec![
            // 批1首次：缺 #2，触发重试
            "#1\nOriginal>\nline 1\nTranslation>\n第一句\n",
            // 批1重试：完整 + 摘要 + 术语
            "#1\nOriginal>\nline 1\nTranslation>\n第一句\n\n#2\nOriginal>\nline 2\nTranslation>\n第二句\n\n<summary>Two opening lines.</summary>\n<terminology>\n</terminology>",
            // 批2首次即完整
            "#3\nOriginal>\nline 3\nTranslation>\n第三句\n\n#4\nOriginal>\nline 4\nTranslation>\n第四句\n",
        ]);

        let progress = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let progress2 = progress.clone();
        let out = translate_all(opts(&mock, &lines), move |ev| {
            if let TranslateEvent::Progress(p) = ev {
                progress2.borrow_mut().push(p);
            }
        })
        .await;

        assert!(out.failed.is_empty(), "所有行都应翻译成功: {:?}", out.failed);
        assert_eq!(out.translations.len(), 4);
        assert_eq!(out.translations[&2], "第二句");

        let prompts = mock.inner.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 3, "批1首次+重试，批2一次");
        // 重试 prompt 带错误明细
        assert!(prompts[1].contains("missing translation for line #2"));
        // 串行（concurrency=1）时批2 prompt 带批1的摘要（上下文链完整）
        assert!(prompts[2].contains("Two opening lines."));
        // 进度回调：首帧 total=2，末帧 done=2
        let progress = progress.borrow();
        assert_eq!(progress.first().unwrap().total_batches, 2);
        assert_eq!(progress.last().unwrap().done_batches, 2);
    }

    #[test]
    fn cached_resume_excludes_failure_placeholders_and_keeps_newer_progress() {
        let lines = dialogue(4, 0);
        let cached = BTreeMap::from([
            (1, "旧译文".to_string()),
            (2, FAILED_TRANSLATION.to_string()),
            (3, "第三句".to_string()),
        ]);
        let mut resume = BTreeMap::from([(0, BTreeMap::from([(1, "新译文".to_string())]))]);
        merge_cached_translations_into_resume(&lines, &cached, 60_000, 1, 2, &mut resume);

        let merged: BTreeMap<_, _> = resume.into_values().flatten().collect();
        assert_eq!(merged.get(&1).unwrap(), "新译文");
        assert!(!merged.contains_key(&2), "失败占位符必须进入重试");
        assert_eq!(merged.get(&3).unwrap(), "第三句");
        assert!(!merged.contains_key(&4), "缺失行必须进入重试");
    }

    #[tokio::test]
    async fn orchestrator_fills_placeholder_on_failure() {
        let lines = dialogue(2, 0);
        let mock = MockLlm::scripted(vec!["garbage without lines", "still garbage"]);
        let out = translate_all(opts(&mock, &lines), |_| {}).await;
        assert_eq!(out.failed.len(), 2);
        assert!(out.translations[&1].contains("翻译失败"));
        assert!(out.translations[&2].contains("翻译失败"));
    }

    #[tokio::test]
    async fn concurrency_limit_respected() {
        // min=max=1 → 20 个单行批；echo 模式 + 15ms 延迟制造并发窗口
        let lines = dialogue(20, 0);
        let mock = MockLlm::echo(15);
        let mut o = opts(&mock, &lines);
        o.min_batch = 1;
        o.max_batch = 1;
        o.concurrency = 4;
        let out = translate_all(o, |_| {}).await;

        assert!(out.failed.is_empty());
        assert_eq!(out.translations.len(), 20);
        assert_eq!(out.translations[&7], "译line 7");
        assert!(mock.max_in_flight() > 1, "确实发生了并发");
        assert!(mock.max_in_flight() <= 4, "并发峰值不超过上限");
    }

    #[tokio::test]
    async fn resume_skips_done_batches() {
        // min=1/max=2 下 4 行分为 3 批：[1] [2] [3,4]；预置批0已完成
        let lines = dialogue(4, 0);
        let mock = MockLlm::echo(0);
        let mut o = opts(&mock, &lines);
        o.min_batch = 1;
        o.max_batch = 2;
        o.resume = [(0u32, [(1u32, "第一".to_string())].into_iter().collect())]
            .into_iter()
            .collect();
        let out = translate_all(o, |_| {}).await;

        assert_eq!(mock.calls(), 2, "批0 已完成不调用 LLM，只翻批1/批2");
        assert_eq!(out.translations[&1], "第一", "续翻行原样回填");
        assert_eq!(out.translations[&3], "译line 3");
        assert!(out.failed.is_empty());
    }

    #[tokio::test]
    async fn line_cache_hit_skips_llm() {
        let path = std::env::temp_dir().join("loopsub_linecache_hit_test.jsonl");
        std::fs::remove_file(&path).ok();
        let mut cache = LineCache::load(path.clone());
        cache.insert("line 1", "一");
        cache.insert("line 2", "二");

        let lines = dialogue(2, 0); // text 即 "line 1" / "line 2"
        let mock = MockLlm::echo(0);
        let mut o = opts(&mock, &lines);
        o.line_cache = Some(&mut cache);
        let out = translate_all(o, |_| {}).await;

        assert_eq!(mock.calls(), 0, "全部命中行缓存，零 LLM 调用");
        assert_eq!(out.translations[&1], "一");
        assert_eq!(out.translations[&2], "二");
        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn placeholder_not_cached() {
        let path = std::env::temp_dir().join("loopsub_linecache_placeholder_test.jsonl");
        std::fs::remove_file(&path).ok();
        let mut cache = LineCache::load(path.clone());

        let lines = dialogue(2, 0);
        let mock = MockLlm::scripted(vec!["garbage", "still garbage"]);
        let mut o = opts(&mock, &lines);
        o.line_cache = Some(&mut cache);
        let out = translate_all(o, |_| {}).await;

        assert_eq!(out.failed.len(), 2);
        assert!(cache.get("line 1").is_none(), "占位符行不入行缓存（否则永远无法重试）");
        assert!(cache.get("line 2").is_none());
        assert_eq!(cache.len(), 0);
        std::fs::remove_file(&path).ok();
    }
}
