//! LLM 翻译管线——分块器（gpt-subtrans 两级策略的移植）：
//! 场景按时间间隙切（默认 60s），场景内超大批在最大间隙处递归二分（10~30 行）。
//! LLM 客户端、摘要链、术语表在 v1.5 实现。

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
}
