use std::cmp::Ordering;
use std::ops::Range;

use crate::fbank::RATE;
use crate::spectrum::Spectrum;

#[derive(Clone, Debug, PartialEq)]
pub struct FreqFilterConfig {
    pub frequencies_hz: Vec<f32>,
    pub threshold: f32,
    pub frame_ms: f32,
    pub shift_ms: f32,
    pub left_context_ms: f32,
    pub buffer_ms: f32,
    pub preemph: f32,
    pub dither: f32,
    pub inside_smooth: u32,
    pub outside_smooth: u32,
    pub lower_ratio: f32,
    pub higher_ratio: f32,
    pub dyn_smooth: bool,
}

/// io.sdk's anti-advertisement filter: see docs/max.md, "Частотный фильтр стока".
pub struct FreqFilter {
    spectrum: Spectrum,
    bands: Box<[Bands]>,
    power: Box<[f32]>,
    scratch: Vec<f32>,
    pending: Vec<i16>,
    skip: usize,
    frame_len: usize,
    frame_shift: usize,
    next_end: u64,
    ring: Ring,
    threshold: f32,
}

impl FreqFilter {
    pub fn new(cfg: &FreqFilterConfig) -> Self {
        let frame_len = ms_to_samples(cfg.frame_ms).max(1);
        let frame_shift = ms_to_samples(cfg.shift_ms).max(1);
        let spectrum = Spectrum::new(frame_len, cfg.preemph, cfg.dither, true);
        let bins = spectrum.bins();
        let fft_len = (bins - 1) * 2;
        let coefficient = if cfg.dyn_smooth {
            dyn_smooth_coefficient(&cfg.frequencies_hz)
        } else {
            0.0
        };
        let bands = cfg
            .frequencies_hz
            .iter()
            .map(|&f| {
                let (h_in, h_out) = if coefficient != 0.0 {
                    (
                        scaled_half_width(cfg.inside_smooth, f, coefficient),
                        scaled_half_width(cfg.outside_smooth, f, coefficient),
                    )
                } else {
                    (i64::from(cfg.inside_smooth), i64::from(cfg.outside_smooth))
                };
                Bands {
                    lower: band(fft_bin(fft_len, cfg.lower_ratio * f), h_out, bins),
                    inside: band(fft_bin(fft_len, f), h_in, bins),
                    upper: band(fft_bin(fft_len, cfg.higher_ratio * f), h_out, bins),
                }
            })
            .collect();
        let ring_frames = ((cfg.buffer_ms - cfg.frame_ms + cfg.shift_ms) / cfg.shift_ms) as i64;
        let left = ((cfg.left_context_ms - cfg.frame_ms) / cfg.shift_ms).max(0.0) as usize;
        Self {
            spectrum,
            bands,
            power: vec![0.0; bins].into_boxed_slice(),
            scratch: Vec::new(),
            pending: Vec::new(),
            skip: 0,
            frame_len,
            frame_shift,
            next_end: frame_len as u64,
            // The upper bound only keeps a malformed config (zero shift) from a huge allocation.
            ring: Ring::new(ring_frames.clamp(1, 1 << 16) as usize, left),
            threshold: cfg.threshold,
        }
    }

    pub fn push(&mut self, raw: &[i16]) {
        let skipped = self.skip.min(raw.len());
        self.skip -= skipped;
        self.pending.extend_from_slice(&raw[skipped..]);
        let mut start = 0;
        while start + self.frame_len <= self.pending.len() {
            self.spectrum.power(
                &self.pending[start..start + self.frame_len],
                &mut self.power,
            );
            let score = self.bands.iter().fold(0.0f32, |sum, bands| {
                sum + bands.feature(&self.power, &mut self.scratch)
            });
            self.ring.push(self.next_end, score);
            self.next_end += self.frame_shift as u64;
            start += self.frame_shift;
        }
        if start > self.pending.len() {
            self.skip = start - self.pending.len();
            start = self.pending.len();
        }
        self.pending.drain(..start);
    }

    /// None until a filter frame ends at or before `hit_sample`: the hit then passes.
    pub fn value_at(&self, hit_sample: u64) -> Option<f32> {
        self.ring.value_at(hit_sample)
    }

    /// The gate's value when it demotes the hit, i.e. when it is not above the threshold.
    pub fn blocking_value(&self, hit_sample: u64) -> Option<f32> {
        self.value_at(hit_sample)
            .filter(|value| value.partial_cmp(&self.threshold) != Some(Ordering::Greater))
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.skip = 0;
        self.next_end = self.frame_len as u64;
        self.ring.clear();
        self.spectrum.rewind_dither();
    }
}

fn ms_to_samples(ms: f32) -> usize {
    (f64::from(RATE as f32 * ms) / 1000.0) as usize
}

fn fft_bin(fft_len: usize, hz: f32) -> i64 {
    ((fft_len as f32 * hz) / RATE as f32) as i64
}

fn dyn_smooth_coefficient(frequencies: &[f32]) -> f32 {
    let max = frequencies
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let min = frequencies.iter().copied().fold(f32::INFINITY, f32::min);
    (max.ln() - min.ln()) / (max - min)
}

// Half away from zero like frinta; a NaN coefficient (one frequency) casts to 0 like fcvtzs.
fn scaled_half_width(smooth: u32, hz: f32, coefficient: f32) -> i64 {
    (smooth as f32 * hz * coefficient).round() as i64
}

fn band(center: i64, half_width: i64, bins: usize) -> Range<usize> {
    let end = center
        .saturating_add(half_width)
        .saturating_add(1)
        .clamp(0, bins as i64) as usize;
    let start = center.saturating_sub(half_width).clamp(0, end as i64) as usize;
    start..end
}

struct Bands {
    lower: Range<usize>,
    inside: Range<usize>,
    upper: Range<usize>,
}

impl Bands {
    fn feature(&self, power: &[f32], scratch: &mut Vec<f32>) -> f32 {
        let lower = median(&power[self.lower.clone()], scratch);
        let inside = mean(&power[self.inside.clone()]);
        let upper = median(&power[self.upper.clone()], scratch);
        // Not f32::min: as the binary's fcsel, a NaN lower median stays NaN.
        let outside = if upper < lower { upper } else { lower };
        inside.ln() - outside.ln()
    }
}

fn mean(values: &[f32]) -> f32 {
    values.iter().fold(0.0f32, |acc, &v| acc + v) / values.len() as f32
}

fn median(values: &[f32], scratch: &mut Vec<f32>) -> f32 {
    let n = values.len();
    if n == 0 {
        return f32::NAN;
    }
    scratch.clear();
    scratch.extend_from_slice(values);
    let (below, &mut mid, _) = scratch.select_nth_unstable_by(n / 2, f32::total_cmp);
    if n % 2 == 1 {
        return mid;
    }
    let lower = below.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    (lower + mid) * 0.5
}

// The binary's value for an empty window: an all-NaN window passes with it.
const EMPTY_VALUE: f32 = 1e10;

struct Ring {
    ends: Box<[u64]>,
    scores: Box<[f32]>,
    written: u64,
    left: usize,
}

impl Ring {
    fn new(capacity: usize, left: usize) -> Self {
        Self {
            ends: vec![0; capacity].into_boxed_slice(),
            scores: vec![0.0; capacity].into_boxed_slice(),
            written: 0,
            left,
        }
    }

    fn clear(&mut self) {
        self.written = 0;
    }

    fn slot(&self, frame: u64) -> usize {
        (frame % self.ends.len() as u64) as usize
    }

    fn push(&mut self, end: u64, score: f32) {
        let slot = self.slot(self.written);
        self.ends[slot] = end;
        self.scores[slot] = score;
        self.written += 1;
    }

    // Mirrors probes/spotter/gate.py, whose error paths let the hit pass.
    fn value_at(&self, hit_sample: u64) -> Option<f32> {
        let capacity = self.ends.len() as u64;
        let oldest = self.written.saturating_sub(capacity);
        let full = self.written >= capacity;
        let newest = (oldest..self.written)
            .rev()
            .find(|&j| self.ends[self.slot(j)] <= hit_sample)?;
        let mut value = EMPTY_VALUE;
        for k in 0..=self.left as u64 {
            let Some(j) = newest.checked_sub(k) else {
                break;
            };
            if full && j == oldest && k < self.left as u64 {
                return None;
            }
            let score = self.scores[self.slot(j)];
            if score < value {
                value = score;
            }
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALISA_HZ: [f32; 8] = [3501.0, 2089.0, 814.0, 1355.0, 638.0, 4510.0, 2635.0, 486.0];

    fn alisa() -> FreqFilterConfig {
        FreqFilterConfig {
            frequencies_hz: ALISA_HZ.to_vec(),
            threshold: -3.6,
            frame_ms: 252.0,
            shift_ms: 177.0,
            left_context_ms: 979.0,
            buffer_ms: 3000.0,
            preemph: 0.72,
            dither: 0.1,
            inside_smooth: 3,
            outside_smooth: 99,
            lower_ratio: 0.68,
            higher_ratio: 1.45,
            dyn_smooth: true,
        }
    }

    struct Golden {
        biquads: Vec<(String, [f64; 5])>,
        features: Vec<(String, Vec<f32>)>,
        gates: Vec<(String, usize, u64, Option<f32>)>,
    }

    fn golden() -> Golden {
        let mut g = Golden {
            biquads: Vec::new(),
            features: Vec::new(),
            gates: Vec::new(),
        };
        for line in include_str!("../tests/fixtures/freqfilter.txt").lines() {
            let mut it = line.split_whitespace();
            let (Some(kind), Some(case)) = (it.next(), it.next()) else {
                continue;
            };
            let case = case.to_string();
            match kind {
                "biquad" => {
                    let c: Vec<f64> = it.map(|v| v.parse().unwrap()).collect();
                    g.biquads.push((case, c.try_into().unwrap()));
                }
                "features" => {
                    let _frame = it.next();
                    g.features
                        .push((case, it.map(|v| v.parse().unwrap()).collect()));
                }
                "gate" => {
                    let pushed = it.next().unwrap().parse().unwrap();
                    let hit: u64 = it.next().unwrap().parse().unwrap();
                    let value = it.next().unwrap();
                    let value = (value != "none").then(|| value.parse().unwrap());
                    g.gates.push((case, pushed, hit, value));
                }
                _ => {}
            }
        }
        g
    }

    // probes/spotter/golden_signal.py, ff_input.
    fn golden_input(golden: &Golden, case: &str) -> Vec<i16> {
        let mut x = 3u32;
        let mut s: Vec<f64> = (0..56_000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                f64::from((x >> 16) as i32 - 32768) / 16.0
            })
            .collect();
        for (_, [b0, b1, b2, a1, a2]) in golden.biquads.iter().filter(|(c, _)| c == case) {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            for v in s.iter_mut() {
                let x = *v;
                let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
                (x2, x1, y2, y1) = (x1, x, y1, y);
                *v = y;
            }
        }
        s.iter()
            .map(|v| v.round_ties_even().clamp(-32768.0, 32767.0) as i16)
            .collect()
    }

    const CASES: [&str; 3] = ["noise", "notched", "boosted"];

    fn ranges(ff: &FreqFilter) -> Vec<[(usize, usize); 3]> {
        ff.bands
            .iter()
            .map(|b| [&b.lower, &b.inside, &b.upper].map(|r| (r.start, r.end)))
            .collect()
    }

    #[test]
    fn alisa_geometry_matches_freqfilter_py() {
        let ff = FreqFilter::new(&alisa());
        assert_eq!((ff.frame_len, ff.frame_shift), (4032, 2832));
        assert_eq!(ff.power.len(), 2049);
        assert_eq!((ff.ring.ends.len(), ff.ring.left), (16, 4));
        assert_eq!(
            ranges(&ff),
            [
                [(417, 802), (890, 903), (1107, 1492)],
                [(249, 478), (531, 538), (661, 890)],
                [(96, 187), (207, 210), (257, 348)],
                [(161, 310), (344, 349), (428, 577)],
                [(76, 147), (162, 165), (201, 272)],
                [(538, 1033), (1147, 1162), (1427, 1922)],
                [(314, 603), (670, 679), (834, 1123)],
                [(57, 112), (123, 126), (153, 208)],
            ]
        );
    }

    #[test]
    fn frame_features_match_freqfilter_py() {
        let golden = golden();
        for case in CASES {
            let input = golden_input(&golden, case);
            let mut ff = FreqFilter::new(&alisa());
            let want: Vec<&Vec<f32>> = golden
                .features
                .iter()
                .filter(|(c, _)| c == case)
                .map(|(_, f)| f)
                .collect();
            assert_eq!(want.len(), 19, "{case}");
            for (j, want) in want.iter().enumerate() {
                let frame = &input[j * ff.frame_shift..j * ff.frame_shift + ff.frame_len];
                ff.spectrum.power(frame, &mut ff.power);
                for (k, bands) in ff.bands.iter().enumerate() {
                    let got = bands.feature(&ff.power, &mut ff.scratch);
                    assert!(
                        (got - want[k]).abs() < 1e-5,
                        "{case} frame {j} freq {k}: {got} vs {}",
                        want[k]
                    );
                }
            }
        }
    }

    #[test]
    fn streaming_gate_matches_gate_py() {
        let golden = golden();
        for case in CASES {
            let input = golden_input(&golden, case);
            let mut ff = FreqFilter::new(&alisa());
            let mut pushed = 0;
            let mut checked = 0;
            for (_, at, hit, want) in golden.gates.iter().filter(|(c, ..)| c == case) {
                ff.push(&input[pushed..*at]);
                pushed = *at;
                let got = ff.value_at(hit * 160);
                match (got, want) {
                    (Some(g), Some(w)) => assert!((g - w).abs() < 2e-5, "{case} {at} {hit}"),
                    _ => assert_eq!(got, *want, "{case} {at} {hit}"),
                }
                checked += 1;
            }
            assert!(checked > 80, "{case}");
        }
    }

    #[test]
    fn notches_block_and_plain_or_boosted_noise_pass() {
        let golden = golden();
        for (case, blocked) in [("noise", false), ("notched", true), ("boosted", false)] {
            let mut ff = FreqFilter::new(&alisa());
            ff.push(&golden_input(&golden, case));
            let hit = 50_000;
            assert!(ff.value_at(hit).is_some());
            assert_eq!(ff.blocking_value(hit).is_some(), blocked, "{case}");
        }
    }

    #[test]
    fn reset_restarts_from_a_fresh_filter() {
        let golden = golden();
        let input = golden_input(&golden, "notched");
        let mut ff = FreqFilter::new(&alisa());
        ff.push(&input[..30_000]);
        ff.reset();
        assert_eq!(ff.value_at(u64::MAX), None);
        ff.push(&input[..20_000]);
        let mut fresh = FreqFilter::new(&alisa());
        fresh.push(&input[..20_000]);
        for hit in (0..20_000).step_by(160) {
            assert_eq!(ff.value_at(hit), fresh.value_at(hit));
        }
    }

    #[test]
    fn shift_longer_than_frame_skips_the_gap() {
        let cfg = FreqFilterConfig {
            frame_ms: 16.0,
            shift_ms: 40.0,
            left_context_ms: 16.0,
            ..alisa()
        };
        let input: Vec<i16> = (0..3000).map(|i| ((i * 37) % 200 - 100) as i16).collect();
        let mut whole = FreqFilter::new(&cfg);
        whole.push(&input);
        let mut chunked = FreqFilter::new(&cfg);
        for chunk in input.chunks(100) {
            chunked.push(chunk);
        }
        assert_eq!(chunked.ring.written, 5);
        assert_eq!(chunked.ring.ends[..5], [256, 896, 1536, 2176, 2816]);
        assert_eq!(chunked.ring.scores, whole.ring.scores);
    }

    #[test]
    fn fixed_half_widths_without_dyn_smooth() {
        let ff = FreqFilter::new(&FreqFilterConfig {
            dyn_smooth: false,
            ..alisa()
        });
        assert_eq!(
            ranges(&ff),
            [
                [(510, 709), (893, 900), (1200, 1399)],
                [(264, 463), (531, 538), (676, 875)],
                [(42, 241), (205, 212), (203, 402)],
                [(136, 335), (343, 350), (403, 602)],
                [(12, 211), (160, 167), (137, 336)],
                [(686, 885), (1151, 1158), (1575, 1774)],
                [(359, 558), (671, 678), (879, 1078)],
                [(0, 184), (121, 128), (81, 280)],
            ]
        );
    }

    #[test]
    fn single_frequency_has_zero_dyn_half_widths() {
        let ff = FreqFilter::new(&FreqFilterConfig {
            frequencies_hz: vec![1000.0],
            ..alisa()
        });
        assert_eq!(ff.bands[0].inside, 256..257);
        assert_eq!(ff.bands[0].lower, 174..175);
    }

    #[test]
    fn bands_are_clipped_to_the_spectrum() {
        assert_eq!(band(10, 20, 2049), 0..31);
        assert_eq!(band(2040, 20, 2049), 2020..2049);
        assert_eq!(band(2100, 20, 2049), 2049..2049);
        assert_eq!(band(-50, 20, 2049), 0..0);
    }

    #[test]
    fn median_and_mean() {
        let mut scratch = Vec::new();
        assert_eq!(median(&[5.0, 1.0, 3.0], &mut scratch), 3.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0], &mut scratch), 2.5);
        assert_eq!(median(&[7.0, 1.0], &mut scratch), 4.0);
        assert!(median(&[], &mut scratch).is_nan());
        assert_eq!(mean(&[1.0, 2.0, 6.0]), 3.0);
    }

    #[test]
    fn nan_medians_choose_like_the_binary() {
        let power = [1.0, 2.0, f32::NAN, 8.0];
        let upper_nan = Bands {
            lower: 1..2,
            inside: 3..4,
            upper: 2..3,
        };
        assert_eq!(
            upper_nan.feature(&power, &mut Vec::new()),
            2.0f32.ln() * 2.0
        );
        let lower_nan = Bands {
            lower: 2..3,
            inside: 3..4,
            upper: 1..2,
        };
        assert!(lower_nan.feature(&power, &mut Vec::new()).is_nan());
    }

    fn end(j: u64) -> u64 {
        4032 + 2832 * j
    }

    fn ring_with(scores: &[f32]) -> Ring {
        let mut ring = Ring::new(16, 4);
        for (j, &s) in scores.iter().enumerate() {
            ring.push(end(j as u64), s);
        }
        ring
    }

    #[test]
    fn ring_follows_gate_py() {
        let hit = 300 * 160;
        let m = ((hit - 4032) / 2832) as usize;
        let clean = vec![0.0f32; m + 2];
        assert_eq!(ring_with(&clean).value_at(hit), Some(0.0));

        let mut marked = clean.clone();
        marked[m - 2] = -4.0;
        assert_eq!(ring_with(&marked).value_at(hit), Some(-4.0));

        let mut too_old = clean.clone();
        too_old[m - 5] = -4.0;
        assert_eq!(ring_with(&too_old).value_at(hit), Some(0.0));

        let mut future = clean.clone();
        future[m + 1] = -4.0;
        assert_eq!(ring_with(&future).value_at(hit), Some(0.0));

        assert_eq!(ring_with(&[]).value_at(1600), None);
        assert_eq!(ring_with(&clean[..2]).value_at(1600), None);
        assert_eq!(
            ring_with(&[-1.0, -2.0, -0.5]).value_at(61 * 160),
            Some(-2.0)
        );

        let long = vec![0.0f32; 40];
        assert_eq!(ring_with(&long).value_at(20 * 160), None);
        let late = end(40 - 16 + 1);
        assert_eq!(ring_with(&long).value_at(late), None);
        assert_eq!(ring_with(&long).value_at(end(40 - 16 + 4)), Some(0.0));

        let mut nan = clean.clone();
        nan[m] = f32::NAN;
        assert_eq!(ring_with(&nan).value_at(hit), Some(0.0));
        assert_eq!(ring_with(&[f32::NAN]).value_at(end(0)), Some(EMPTY_VALUE));
    }

    #[test]
    fn blocks_on_or_below_the_threshold() {
        let mut ff = FreqFilter::new(&alisa());
        assert!(ff.blocking_value(u64::MAX).is_none());
        for (score, blocked) in [(-3.5, false), (-3.6, true), (-10.0, true)] {
            ff.ring = ring_with(&[score]);
            assert_eq!(ff.blocking_value(end(0)).is_some(), blocked, "{score}");
        }
        ff.ring = ring_with(&[f32::NAN]);
        assert!(ff.blocking_value(end(0)).is_none());
    }
}
