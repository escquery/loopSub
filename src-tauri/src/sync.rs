//! 字幕-音频自动对齐（ffsubsync 思路的自研简化版，零外部依赖）：
//! 1) ffmpeg 提 8kHz 单声道 PCM → 40ms 帧 RMS 能量包络 A(t)
//! 2) 字幕时间轴按同一栅格画"认为在说话"的包络 B(t)
//! 3) 分段做归一化互相关，峰值位置 = 该段字幕偏移；
//!    多段偏移做加权最小二乘回归：截距 → mpv sub-delay，斜率 → sub-speed。
//!    恒定偏移（换源差几秒）与线性漂移（23.976/25fps 帧率不匹配）都能修。
//!
//! 结果只映射到 mpv 的 sub-delay / sub-speed 属性，不改写字幕文件，随时可复位。

/// ffmpeg 提取的 PCM 采样率（8kHz 对语音能量足够，且压低数据量）
pub const SAMPLE_RATE: u32 = 8000;
/// 包络帧长：40ms/帧 → 时间精度 ±20ms（远低于 100ms 人感阈值），互相关计算量可控
pub const FRAME_MS: usize = 40;
const FRAME_SAMPLES: usize = SAMPLE_RATE as usize * FRAME_MS / 1000; // 320

/// 分段数：每段独立估偏移；≥3 段成功才尝试漂移回归
const N_SEGMENTS: usize = 5;
/// 段采纳双重条件（合成信号实测标定）：
/// - corr ≥ 0.35：强匹配直接采纳（真实对白 0.45+，错源假峰 ≤0.28，间隔充足）
/// - corr ∈ [0.10, 0.35) 模糊区：要求峰显著性 sig ≥ 4.0（尖峰才采纳；
///   漂移场景的宽峰 sig 仅 2.6~4.2，但 corr 高，走强匹配通道）
const MIN_PEAK_CORR_STRONG: f64 = 0.35;
const MIN_PEAK_CORR: f64 = 0.10;
const MIN_PEAK_SIG: f64 = 4.0;
/// 斜率显著阈值：|β| 小于 0.05%（一小时差 1.8s）视为恒定偏移，不动 sub-speed
const MIN_DRIFT_SLOPE: f64 = 0.0005;
/// speed 安全钳制：超出 ±10% 必是误估
const SPEED_CLAMP: (f64, f64) = (0.9, 1.1);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncEstimate {
    /// mpv sub-delay（秒）：正 = 字幕需要延后显示
    pub delay_s: f64,
    /// mpv sub-speed（倍率）：1.0 = 无漂移
    pub speed: f64,
    /// 是否检出线性漂移
    pub drift: bool,
    /// 估计成功的分段数 / 总分段数（置信参考）
    pub segments_ok: usize,
    pub segments_total: usize,
}

/// PCM(s16le, 8kHz mono) → 逐帧 RMS 能量包络（尾不足一帧丢弃）
pub fn energy_envelope(pcm: &[i16]) -> Vec<f32> {
    pcm.chunks_exact(FRAME_SAMPLES)
        .map(|frame| {
            let sum: f64 = frame
                .iter()
                .map(|&s| (s as f64 / 32768.0).powi(2))
                .sum();
            (sum / frame.len() as f64).sqrt() as f32
        })
        .collect()
}

/// 字幕时间轴 → 区间置 1 的包络（与音频包络同栅格；超出范围的行截断）
pub fn subtitle_envelope(lines: &[crate::subtitle::SubtitleLine], n_frames: usize) -> Vec<f32> {
    let mut env = vec![0f32; n_frames];
    for l in lines {
        let a = (l.start_ms as usize / FRAME_MS).min(n_frames);
        let b = ((l.end_ms as usize + FRAME_MS - 1) / FRAME_MS).min(n_frames);
        for e in env.iter_mut().take(b).skip(a) {
            *e = 1.0;
        }
    }
    env
}

/// 归一化互相关：b 相对 a 平移 shift 帧（正 = b 需要右移/字幕需要延后）。
/// 返回 (最佳 shift, 峰值相关系数, 峰显著性)；能量过低返回 None。
/// 直接滑窗 O(n·m)：全片 1h@40ms=9 万帧 × ±30s=±750 shift ≈ 1.4 亿次乘加，可接受。
fn norm_xcorr_peak(a: &[f32], b: &[f32], max_shift: i64) -> Option<(i64, f64, f64)> {
    let n = a.len().min(b.len()) as i64;
    if n < max_shift * 2 + 10 {
        return None; // 片长不足以支撑搜索窗
    }
    let mean_a: f64 = a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
    let mean_b: f64 = b.iter().map(|&v| v as f64).sum::<f64>() / b.len() as f64;
    let norm_a: f64 = a
        .iter()
        .map(|&v| (v as f64 - mean_a).powi(2))
        .sum::<f64>()
        .sqrt();
    let norm_b: f64 = b
        .iter()
        .map(|&v| (v as f64 - mean_b).powi(2))
        .sum::<f64>()
        .sqrt();
    if norm_a < 1e-6 || norm_b < 1e-6 {
        return None; // 全静音或无字幕
    }
    let mut corrs = Vec::with_capacity((max_shift * 2 + 1) as usize);
    for shift in -max_shift..=max_shift {
        let mut dot = 0f64;
        // t 取两边都合法的范围：a[t]·b[t-shift]
        let lo = shift.max(0);
        let hi = (n + shift.min(0)).min(n);
        for t in lo..hi {
            dot += (a[t as usize] as f64 - mean_a) * (b[(t - shift) as usize] as f64 - mean_b);
        }
        corrs.push(dot / (norm_a * norm_b));
    }
    let (best_idx, &best_corr) = corrs
        .iter()
        .enumerate()
        .max_by(|(_, x), (_, y)| x.partial_cmp(y).unwrap())
        .unwrap();
    let best_shift = best_idx as i64 - max_shift;
    let mean = corrs.iter().sum::<f64>() / corrs.len() as f64;
    let var = corrs.iter().map(|c| (c - mean).powi(2)).sum::<f64>() / corrs.len() as f64;
    let significance = if var > 1e-12 {
        (best_corr - mean) / var.sqrt()
    } else {
        0.0
    };
    Some((best_shift, best_corr, significance))
}

/// 加权最小二乘拟合 δ(t) = α + β·t，权重 = 段相关系数
fn weighted_regression(xs: &[f64], ys: &[f64], ws: &[f64]) -> (f64, f64) {
    let sw: f64 = ws.iter().sum();
    let sx: f64 = xs.iter().zip(ws).map(|(x, w)| x * w).sum::<f64>() / sw;
    let sy: f64 = ys.iter().zip(ws).map(|(y, w)| y * w).sum::<f64>() / sw;
    let mut sxx = 0f64;
    let mut sxy = 0f64;
    for ((x, y), w) in xs.iter().zip(ys).zip(ws) {
        sxx += w * (x - sx).powi(2);
        sxy += w * (x - sx) * (y - sy);
    }
    if sxx.abs() < 1e-12 {
        return (sy, 0.0);
    }
    let beta = sxy / sxx;
    (sy - beta * sx, beta)
}

/// 主入口：分段互相关 + 回归。search_s 为段内搜索半径（默认 ±30s）。
/// 成功段 <2 视为不可靠，返回 None（交由调用方提示手动微调）。
pub fn estimate(audio: &[f32], subs: &[f32], search_s: f64) -> Option<SyncEstimate> {
    let max_shift = (search_s * 1000.0 / FRAME_MS as f64) as i64;
    let n = audio.len().min(subs.len());
    if n < (max_shift as usize) * 2 + 10 {
        return None;
    }
    let frame_s = FRAME_MS as f64 / 1000.0;
    let seg_step = n / N_SEGMENTS;
    // 段分析窗 = 段中心 ± (段距/2 + 搜索半径)：相邻段重叠，保证搜索窗两端仍有足够样本
    let half_win = seg_step / 2 + max_shift as usize;

    // 逐段估计：段中心时间 t_i（秒）、偏移 δ_i（秒）、相关系数 w_i
    let mut ts = Vec::new();
    let mut ds = Vec::new();
    let mut ws = Vec::new();
    for i in 0..N_SEGMENTS {
        let center = i * seg_step + seg_step / 2;
        let lo = center.saturating_sub(half_win);
        let hi = (center + half_win).min(n);
        let a_seg = &audio[lo..hi];
        let b_seg = &subs[lo..hi];
        if let Some((shift, corr, sig)) = norm_xcorr_peak(a_seg, b_seg, max_shift) {
            let strong = corr >= MIN_PEAK_CORR_STRONG;
            let weak_but_sharp = corr >= MIN_PEAK_CORR && sig >= MIN_PEAK_SIG;
            if strong || weak_but_sharp {
                ts.push((lo + (hi - lo) / 2) as f64 * frame_s);
                ds.push(shift as f64 * frame_s);
                ws.push(corr);
            }
        }
    }
    if ts.len() < 2 {
        return None;
    }

    let (alpha, beta) = weighted_regression(&ts, &ds, &ws);
    let (delay_s, speed, drift) = if ts.len() >= 3 && beta.abs() >= MIN_DRIFT_SLOPE {
        let sp = (1.0 + beta).clamp(SPEED_CLAMP.0, SPEED_CLAMP.1);
        (alpha, sp, true)
    } else {
        // 恒定偏移：相关系数加权平均
        let wsum: f64 = ws.iter().sum();
        let avg = ds.iter().zip(&ws).map(|(d, w)| d * w).sum::<f64>() / wsum;
        (avg, 1.0, false)
    };
    Some(SyncEstimate {
        delay_s,
        speed,
        drift,
        segments_ok: ts.len(),
        segments_total: N_SEGMENTS,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subtitle::SubtitleLine;

    /// LCG 伪随机（不引 rand 依赖）
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((self.0 >> 33) as f64) / ((1u64 << 31) as f64)
        }
    }

    /// 生成"语音模式"区间表：随机分布的说话段（秒）；占空比 ~40% 接近真实对白
    fn speech_segments(seed: u64, dur_s: f64) -> Vec<(f64, f64)> {
        let mut rng = Rng(seed);
        let mut segs = Vec::new();
        let mut t = 2.0;
        while t < dur_s - 4.0 {
            let speech = 0.8 + rng.next() * 2.5; // 说话 0.8~3.3s
            segs.push((t, t + speech));
            t += speech + 1.0 + rng.next() * 3.0; // 沉默 1.0~4.0s
        }
        segs
    }

    /// 按区间表在时间 t 采样包络值（说话=1，沉默=0.01 底噪）
    fn pattern_at(segs: &[(f64, f64)], t: f64) -> f32 {
        if segs.iter().any(|&(a, b)| t >= a && t < b) {
            1.0
        } else {
            0.01
        }
    }

    fn envelope_from_pattern(segs: &[(f64, f64)], n_frames: usize, warp: &dyn Fn(f64) -> f64) -> Vec<f32> {
        (0..n_frames)
            .map(|i| pattern_at(segs, warp(i as f64 * FRAME_MS as f64 / 1000.0)))
            .collect()
    }

    fn env_with_offset(segs: &[(f64, f64)], n: usize, offset_s: f64) -> Vec<f32> {
        // offset_s > 0：字幕包络提前 offset 秒（字幕早了，需要正的 sub-delay 补候）
        envelope_from_pattern(segs, n, &move |t| t + offset_s)
    }

    const DUR_S: f64 = 300.0; // 5 分钟合成片
    const N: usize = (DUR_S * 1000.0) as usize / FRAME_MS;

    #[test]
    fn constant_offset_recovered() {
        let segs = speech_segments(42, DUR_S);
        let audio = envelope_from_pattern(&segs, N, &|t| t);
        let subs = env_with_offset(&segs, N, 3.2); // 字幕早 3.2s → 需 delay +3.2
        let est = estimate(&audio, &subs, 30.0).expect("应估出");
        assert!((est.delay_s - 3.2).abs() < 0.05, "delay={}", est.delay_s);
        assert_eq!(est.speed, 1.0);
        assert!(!est.drift);
        assert_eq!(est.segments_ok, N_SEGMENTS);
    }

    #[test]
    fn negative_offset_recovered() {
        let segs = speech_segments(7, DUR_S);
        let audio = envelope_from_pattern(&segs, N, &|t| t);
        let subs = env_with_offset(&segs, N, -2.0); // 字幕晚 2s → 需 delay -2.0
        let est = estimate(&audio, &subs, 30.0).expect("应估出");
        assert!((est.delay_s + 2.0).abs() < 0.05, "delay={}", est.delay_s);
    }

    #[test]
    fn drift_recovered() {
        // 帧率不匹配：字幕时间轴 ×(1+2%)，另有 +1.5s 恒定差
        let segs = speech_segments(99, DUR_S);
        let audio = envelope_from_pattern(&segs, N, &|t| t);
        let subs = envelope_from_pattern(&segs, N, &|t| 1.02 * t + 1.5);
        let est = estimate(&audio, &subs, 30.0).expect("应估出");
        assert!(est.drift, "应检出漂移: {:?}", est);
        assert!((est.speed - 1.02).abs() < 0.004, "speed={}", est.speed);
        assert!((est.delay_s - 1.5).abs() < 0.2, "delay={}", est.delay_s);
    }

    #[test]
    fn silence_returns_none() {
        let audio = vec![0.0f32; N];
        let subs = vec![1.0f32; N];
        assert!(estimate(&audio, &subs, 30.0).is_none());
    }

    #[test]
    fn mismatch_returns_none() {
        // 完全不同的语音模式（另一条音轨）：相关应低于阈值
        let audio_segs = speech_segments(1, DUR_S);
        let subs_segs = speech_segments(2, DUR_S);
        let audio = envelope_from_pattern(&audio_segs, N, &|t| t);
        let subs = envelope_from_pattern(&subs_segs, N, &|t| t);
        assert!(estimate(&audio, &subs, 30.0).is_none());
    }

    #[test]
    fn energy_envelope_rms() {
        // 一帧满幅 + 一帧静音 → [1.0, 0.0]
        let mut pcm = vec![0i16; FRAME_SAMPLES * 2];
        for s in pcm.iter_mut().take(FRAME_SAMPLES) {
            *s = 32767;
        }
        let env = energy_envelope(&pcm);
        assert_eq!(env.len(), 2);
        assert!((env[0] - 1.0).abs() < 0.01);
        assert_eq!(env[1], 0.0);
    }

    #[test]
    fn subtitle_envelope_marks_ranges() {
        let lines = vec![
            SubtitleLine { number: 1, start_ms: 1000, end_ms: 2000, text: "a".into() },
            SubtitleLine { number: 2, start_ms: 2100, end_ms: 2200, text: "b".into() },
        ];
        let env = subtitle_envelope(&lines, 100); // 100 帧 = 4s
        assert_eq!(env[24], 0.0); // 0.96s 未开始
        assert_eq!(env[25], 1.0); // 1.0s 起点
        assert_eq!(env[49], 1.0); // 1.96s
        assert_eq!(env[50], 0.0); // [1000,2000)ms 不含帧 50（2000-2040ms）
        assert_eq!(env[51], 0.0);
        assert_eq!(env[53], 1.0); // 2.12s 第二句
        assert_eq!(env[56], 0.0);
    }
}
