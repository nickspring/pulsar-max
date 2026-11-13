#[derive(Clone, Debug, PartialEq)]
pub struct Chen14Config {
    pub w_smooth: usize,
    pub w_max: usize,
    pub threshold: f32,
    pub subthreshold: f32,
}

/// `frame` indexes posteriors since `new`/`reset`. A subhit is decided one frame after the peak
/// and, as in io.sdk, carries the peak's frame but the deciding frame's confidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Decision {
    Hit { frame: u64, confidence: f32 },
    Subhit { frame: u64, confidence: f32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
enum Level {
    #[default]
    Below,
    Sub,
    Hit,
}

/// Field order is io.sdk's ranking of frames: level first, then ratio.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
struct Frame {
    level: Level,
    ratio: f32,
}

pub struct Chen14 {
    cfg: Chen14Config,
    posteriors: Vec<f32>,
    sum: f32,
    smoothed: Vec<f32>,
    frames: u64,
    prev: Frame,
    subhit_reported: bool,
}

impl Chen14 {
    pub fn new(cfg: &Chen14Config) -> Self {
        Self {
            cfg: cfg.clone(),
            posteriors: vec![0.0; cfg.w_smooth],
            sum: 0.0,
            smoothed: vec![0.0; cfg.w_max],
            frames: 0,
            prev: Frame::default(),
            subhit_reported: false,
        }
    }

    /// One keyword posterior per 10 ms frame, in order from frame 0.
    pub fn push(&mut self, posterior: f32) -> Option<Decision> {
        let confidence = self.confidence(posterior);
        self.decide(confidence)
    }

    pub fn reset(&mut self) {
        *self = Self::new(&self.cfg);
    }

    fn confidence(&mut self, posterior: f32) -> f32 {
        self.frames += 1;
        let window = self.posteriors.len() as u64;
        let slot = (self.frames % window) as usize;
        // The running sum drifts in f32, so io.sdk's order (add, then subtract) shows in the bits.
        self.sum += posterior;
        self.sum -= self.posteriors[slot];
        self.posteriors[slot] = posterior;
        // io.sdk's warm-up quirk: frame n (from 1) is divided by n + 1, not n.
        let divisor = (self.frames + 1).min(window) as f32;
        let slot = (self.frames % self.smoothed.len() as u64) as usize;
        self.smoothed[slot] = (self.sum / divisor).max(0.0);
        self.smoothed
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max)
    }

    fn decide(&mut self, confidence: f32) -> Option<Decision> {
        let level = if confidence > self.cfg.threshold {
            Level::Hit
        } else if confidence > self.cfg.subthreshold {
            Level::Sub
        } else {
            Level::Below
        };
        let cur = Frame {
            level,
            ratio: confidence / self.cfg.threshold,
        };
        let prev = std::mem::replace(&mut self.prev, cur);
        let rising = cur > prev;
        let frame = self.frames - 1;
        match (prev.level, cur.level) {
            (Level::Hit, Level::Hit) | (Level::Below, Level::Below | Level::Sub) => None,
            (_, Level::Hit) => {
                self.subhit_reported = true;
                Some(Decision::Hit { frame, confidence })
            }
            // io.sdk leaves the flag set after a subhit on the drop, muting the next episode's.
            (Level::Sub, _) if !self.subhit_reported && !rising => {
                self.subhit_reported = true;
                Some(Decision::Subhit {
                    frame: frame - 1,
                    confidence,
                })
            }
            (_, Level::Below) => {
                self.subhit_reported = false;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chen(w_smooth: usize, w_max: usize, threshold: f32, subthreshold: f32) -> Chen14 {
        Chen14::new(&Chen14Config {
            w_smooth,
            w_max,
            threshold,
            subthreshold,
        })
    }

    fn decisions(chen: &mut Chen14, posteriors: &[f32]) -> Vec<Decision> {
        posteriors.iter().filter_map(|&p| chen.push(p)).collect()
    }

    fn confidences(chen: &mut Chen14, posteriors: &[f32]) -> Vec<f32> {
        posteriors.iter().map(|&p| chen.confidence(p)).collect()
    }

    fn hit(frame: u64, confidence: f32) -> Decision {
        Decision::Hit { frame, confidence }
    }

    fn subhit(frame: u64, confidence: f32) -> Decision {
        Decision::Subhit { frame, confidence }
    }

    #[test]
    fn smoothing_averages_the_last_w_smooth_posteriors() {
        let mut c = chen(2, 1, 0.5, 0.25);
        let got = confidences(&mut c, &[1.0, 0.0, 0.0, 0.5, 0.25]);
        assert_eq!(got, [0.5, 0.5, 0.0, 0.25, 0.375]);
    }

    #[test]
    fn warm_up_divides_frame_n_by_n_plus_one() {
        let ones = [1.0; 5];
        assert_eq!(
            confidences(&mut chen(4, 1, 0.7, 0.6), &ones),
            [0.5, 2.0 / 3.0, 0.75, 1.0, 1.0]
        );
        assert_eq!(decisions(&mut chen(4, 1, 0.7, 0.6), &ones), [hit(2, 0.75)]);
    }

    #[test]
    fn float_drift_below_zero_is_clamped() {
        let mut c = chen(2, 1, 0.5, 0.25);
        let got = confidences(&mut c, &[0.7, 0.9, 0.0, 0.0]);
        assert!(c.sum < 0.0, "{}", c.sum);
        assert_eq!(got[3].to_bits(), 0);
    }

    #[test]
    fn confidence_is_the_max_over_the_last_w_max_frames() {
        let mut c = chen(1, 3, 0.5, 0.25);
        let got = confidences(&mut c, &[0.5, 0.25, 0.125, 0.125, 0.0, 0.75]);
        assert_eq!(got, [0.5, 0.5, 0.5, 0.25, 0.125, 0.75]);
    }

    #[test]
    fn levels_need_a_strictly_greater_confidence() {
        let at_thresholds = [0.25, 0.5, 0.625];
        assert_eq!(
            decisions(&mut chen(1, 1, 0.5, 0.25), &at_thresholds),
            [hit(2, 0.625)]
        );
        let dip_to_subthreshold = [0.375, 0.375, 0.25, 0.375, 0.375];
        assert_eq!(
            decisions(&mut chen(1, 1, 0.5, 0.25), &dip_to_subthreshold),
            [subhit(0, 0.375), subhit(3, 0.375)]
        );
    }

    #[test]
    fn a_hit_fires_on_the_rising_edge_and_re_arms_at_or_below_the_threshold() {
        let mut c = chen(1, 1, 0.5, 0.25);
        let got = decisions(&mut c, &[0.375, 0.75, 0.875, 0.5, 0.75, 0.125, 0.625]);
        assert_eq!(got, [hit(1, 0.75), hit(4, 0.75), hit(6, 0.625)]);
    }

    #[test]
    fn after_a_hit_subhits_wait_for_a_drop_to_the_subthreshold() {
        let mut c = chen(1, 1, 0.5, 0.25);
        let got = decisions(&mut c, &[0.75, 0.375, 0.375, 0.25, 0.375, 0.375]);
        assert_eq!(got, [hit(0, 0.75), subhit(4, 0.375)]);
    }

    #[test]
    fn a_subhit_reports_the_peak_once_per_episode() {
        let mut c = chen(1, 1, 0.75, 0.25);
        let first = [0.375, 0.5, 0.5, 0.625, 0.375, 0.125];
        assert_eq!(decisions(&mut c, &first), [subhit(1, 0.5)]);
        assert_eq!(decisions(&mut c, &[0.5, 0.375]), [subhit(6, 0.375)]);
    }

    #[test]
    fn a_subhit_decided_on_the_drop_mutes_the_next_episode() {
        let mut c = chen(1, 1, 0.75, 0.25);
        let got = decisions(&mut c, &[0.5, 0.125, 0.5, 0.375, 0.125, 0.5, 0.375]);
        assert_eq!(got, [subhit(0, 0.125), subhit(5, 0.375)]);
    }

    #[test]
    fn reset_restarts_frames_warm_up_and_episodes() {
        let mut c = chen(2, 2, 0.5, 0.25);
        assert_eq!(decisions(&mut c, &[1.0, 1.0]), [hit(1, 1.0)]);
        c.reset();
        assert_eq!(decisions(&mut c, &[1.0, 1.0]), [hit(1, 1.0)]);

        let mut c = chen(1, 1, 0.5, 0.25);
        assert_eq!(decisions(&mut c, &[0.75]), [hit(0, 0.75)]);
        c.reset();
        assert_eq!(decisions(&mut c, &[0.75]), [hit(0, 0.75)]);

        let mut c = chen(1, 1, 0.5, 0.25);
        assert_eq!(decisions(&mut c, &[0.375, 0.375]), [subhit(0, 0.375)]);
        c.reset();
        assert_eq!(decisions(&mut c, &[0.375, 0.375]), [subhit(0, 0.375)]);
    }

    const ONE: i64 = 1 << 24;

    /// `posteriors()` of probes/spotter/chen14_ref.py, which generated the golden values below.
    fn synthetic(n: i64, seed: u64, noise_shift: u32, bumps: &[(i64, i64, i64)]) -> Vec<f32> {
        let mut x = seed;
        (0..n)
            .map(|t| {
                x = x
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let env = bumps
                    .iter()
                    .map(|&(c, w, h)| h * (w - (t - c).abs()).max(0) / w)
                    .fold(0, i64::max);
                ONE.min(env + (x >> noise_shift) as i64) as f32 / ONE as f32
            })
            .collect()
    }

    fn agrees_with_reference(
        cfg: &Chen14Config,
        posteriors: &[f32],
        want_decisions: &[(bool, u64, u32)],
        want_confidence: &[(usize, u32)],
    ) {
        let mut c = Chen14::new(cfg);
        let mut got_confidence = Vec::new();
        let mut got_decisions = Vec::new();
        for &p in posteriors {
            let confidence = c.confidence(p);
            got_confidence.push(confidence.to_bits());
            got_decisions.extend(c.decide(confidence));
        }
        let want: Vec<_> = want_decisions
            .iter()
            .map(|&(is_hit, frame, bits)| match is_hit {
                true => hit(frame, f32::from_bits(bits)),
                false => subhit(frame, f32::from_bits(bits)),
            })
            .collect();
        assert_eq!(got_decisions, want);
        for &(frame, bits) in want_confidence {
            assert_eq!(got_confidence[frame], bits, "frame {frame}");
        }
    }

    #[test]
    fn agrees_with_the_python_reference_at_station_settings() {
        let bumps = [
            (50, 14, ONE / 2),
            (130, 16, 15 * ONE / 16),
            (180, 16, 15 * ONE / 16),
            (155, 70, 5 * ONE / 8),
            (260, 8, 7 * ONE / 8),
            (330, 12, 11 * ONE / 16),
        ];
        let cfg = Chen14Config {
            w_smooth: 5,
            w_max: 20,
            threshold: 0.861,
            subthreshold: 0.435,
        };
        agrees_with_reference(
            &cfg,
            &synthetic(400, 1, 43, &bumps),
            STATION_DECISIONS,
            STATION_CONFIDENCE,
        );
    }

    #[test]
    fn agrees_with_the_python_reference_with_one_frame_windows() {
        let bumps = [(40, 12, ONE / 2), (100, 30, 5 * ONE / 8), (160, 8, ONE / 4)];
        let cfg = Chen14Config {
            w_smooth: 3,
            w_max: 1,
            threshold: 0.75,
            subthreshold: 0.375,
        };
        agrees_with_reference(
            &cfg,
            &synthetic(200, 3, 41, &bumps),
            FAST_DECISIONS,
            FAST_CONFIDENCE,
        );
    }

    const STATION_DECISIONS: &[(bool, u64, u32)] = &[
        (false, 52, 0x3efe3efd),
        (true, 130, 0x3f5fcdc2),
        (true, 180, 0x3f614c46),
        (false, 262, 0x3f51d153),
        (false, 331, 0x3f30fa40),
    ];
    const STATION_CONFIDENCE: &[(usize, u32)] = &[
        (0, 0x3cd8aed0),
        (20, 0x3d9ee485),
        (40, 0x3de0b620),
        (60, 0x3efe3efd),
        (80, 0x3e7d9fd0),
        (100, 0x3e42516d),
        (120, 0x3ec61d4f),
        (140, 0x3f6cb760),
        (160, 0x3f2dbb1a),
        (180, 0x3f614c46),
        (200, 0x3f69c25a),
        (220, 0x3e9fc4b7),
        (240, 0x3df855db),
        (260, 0x3f38d105),
        (280, 0x3f51d153),
        (300, 0x3dc01828),
        (320, 0x3dfd33b8),
        (340, 0x3f30fa40),
        (360, 0x3e78b9fa),
        (380, 0x3da5f950),
    ];
    const FAST_DECISIONS: &[(bool, u64, u32)] = &[
        (false, 11, 0x3eb350ab),
        (false, 33, 0x3ed0c41c),
        (true, 41, 0x3f4e5075),
        (false, 66, 0x3ed49805),
        (false, 76, 0x3ebdbd83),
        (true, 90, 0x3f4073f7),
        (true, 100, 0x3f4dd99f),
        (true, 108, 0x3f414ce3),
        (false, 127, 0x3ec3f96d),
        (false, 147, 0x3ed709bb),
        (false, 161, 0x3ed0c655),
        (false, 166, 0x3ea1718f),
    ];
    const FAST_CONFIDENCE: &[(usize, u32)] = &[
        (0, 0x3ce7dac0),
        (20, 0x3e796ee3),
        (40, 0x3f2931b3),
        (60, 0x3e8523c3),
        (80, 0x3ec4c0a4),
        (100, 0x3f4dd99f),
        (120, 0x3f1a102a),
        (140, 0x3e134113),
        (160, 0x3eed4bc0),
        (180, 0x3e661839),
    ];
}
