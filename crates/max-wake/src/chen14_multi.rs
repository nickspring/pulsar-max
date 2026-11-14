#[derive(Clone, Debug, PartialEq)]
pub struct Chen14MultiConfig {
    pub commands: Vec<Vec<usize>>,
    pub thresholds: Vec<f32>,
    pub w_smooth: usize,
    pub w_max: usize,
    pub interval_after_hit: u64,
    pub zero_after_hit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub command: usize,
    pub frame: u64,
    pub confidence: f32,
}

/// io.sdk's chen14 for multi-word commands, reference `probes/spotter/cmd_ref.py`. Subhits never
/// lead to a hit in subhit mode, so io.sdk's subthreshold levels and subhit flags are left out.
pub struct Chen14Multi {
    cfg: Chen14MultiConfig,
    units: Box<[usize]>,
    sums: Box<[f32]>,
    recent: Box<[f32]>,
    smoothed: Box<[f32]>,
    confidences: Box<[f32]>,
    frame: u64,
    resume_at: u64,
    averaging_from: u64,
    prev_best: Option<usize>,
}

impl Chen14Multi {
    pub fn new(cfg: &Chen14MultiConfig) -> Self {
        assert!(cfg.commands.iter().all(|c| !c.is_empty()), "empty command");
        assert_eq!(
            cfg.commands.len(),
            cfg.thresholds.len(),
            "one threshold per command"
        );
        assert!(cfg.w_smooth > 0 && cfg.w_max > 0, "empty window");
        let units: Box<[usize]> = cfg.commands.iter().flatten().copied().collect();
        let slots = units.len();
        Self {
            sums: vec![0.0; slots].into(),
            recent: vec![0.0; cfg.w_smooth * slots].into(),
            smoothed: vec![0.0; cfg.w_max * slots].into(),
            confidences: vec![0.0; cfg.commands.len()].into(),
            units,
            frame: 0,
            resume_at: 0,
            averaging_from: 0,
            prev_best: None,
            cfg: cfg.clone(),
        }
    }

    pub fn push(&mut self, posteriors: &[f32]) -> Option<Hit> {
        let frame = self.frame;
        self.frame += 1;
        if frame < self.resume_at {
            return None;
        }
        let n = frame + 1;
        self.smooth(posteriors, n);
        self.update_confidences(n);
        let best = self.best();
        let prev = std::mem::replace(&mut self.prev_best, best);
        let command = best.filter(|&b| Some(b) != prev)?;
        let hit = Hit {
            command,
            frame,
            confidence: self.confidences[command],
        };
        self.resume_at = self.frame + self.cfg.interval_after_hit;
        if self.cfg.zero_after_hit {
            self.restart_averaging(self.resume_at);
        }
        Some(hit)
    }

    pub fn reset(&mut self) {
        self.frame = 0;
        self.resume_at = 0;
        self.restart_averaging(0);
    }

    fn restart_averaging(&mut self, from: u64) {
        self.sums.fill(0.0);
        self.recent.fill(0.0);
        self.smoothed.fill(0.0);
        self.averaging_from = from;
        self.prev_best = None;
    }

    fn smooth(&mut self, posteriors: &[f32], n: u64) {
        let slots = self.units.len();
        let recent = &mut self.recent[slot(n, self.cfg.w_smooth) * slots..][..slots];
        let smoothed = &mut self.smoothed[slot(n, self.cfg.w_max) * slots..][..slots];
        // The same off-by-one warm-up as "Алиса": the first frame is divided by 2.
        let divisor = (self.cfg.w_smooth as u64).min(n - self.averaging_from + 1) as f32;
        for (k, &unit) in self.units.iter().enumerate() {
            let p = posteriors[unit];
            // f32 drift shows in the bits, so keep io.sdk's order: add, then subtract.
            self.sums[k] = self.sums[k] + p - recent[k];
            recent[k] = p;
            let s = self.sums[k] / divisor;
            smoothed[k] = if s > 0.0 { s } else { 0.0 };
        }
    }

    fn update_confidences(&mut self, n: u64) {
        let slots = self.units.len();
        let w_max = self.cfg.w_max;
        let oldest = slot(n + 1, w_max);
        let value = |age: usize, k: usize| self.smoothed[(oldest + age) % w_max * slots + k];
        let mut k = 0;
        for (confidence, words) in self.confidences.iter_mut().zip(&self.cfg.commands) {
            let mut from = 0;
            let mut product = 1.0f32;
            for _ in words {
                let mut peak = from;
                for age in from + 1..w_max {
                    if value(age, k) > value(peak, k) {
                        peak = age;
                    }
                }
                product *= value(peak, k);
                from = peak;
                k += 1;
            }
            *confidence = match words.len() {
                1 => product,
                2 => product.sqrt(),
                len => product.powf((1.0 / len as f64) as f32),
            };
        }
    }

    fn best(&self) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, (&c, &t)) in self
            .confidences
            .iter()
            .zip(&self.cfg.thresholds)
            .enumerate()
        {
            let ratio = c / t;
            if c > t && best.is_none_or(|(_, r)| ratio > r) {
                best = Some((i, ratio));
            }
        }
        best.map(|(i, _)| i)
    }
}

fn slot(n: u64, window: usize) -> usize {
    (n % window as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(
        commands: &[&[usize]],
        thresholds: &[f32],
        w_smooth: usize,
        w_max: usize,
    ) -> Chen14MultiConfig {
        Chen14MultiConfig {
            commands: commands.iter().map(|c| c.to_vec()).collect(),
            thresholds: thresholds.to_vec(),
            w_smooth,
            w_max,
            interval_after_hit: 0,
            zero_after_hit: false,
        }
    }

    fn hits(d: &mut Chen14Multi, frames: &[&[f32]]) -> Vec<(u64, usize)> {
        frames
            .iter()
            .filter_map(|p| d.push(p))
            .map(|h| (h.frame, h.command))
            .collect()
    }

    #[test]
    fn warm_up_divides_frame_n_by_n_plus_one_and_clamps_drift() {
        let mut d = Chen14Multi::new(&cfg(&[&[0]], &[0.9], 4, 1));
        let got: Vec<f32> = (0..5)
            .map(|_| {
                d.push(&[1.0]);
                d.confidences[0]
            })
            .collect();
        assert_eq!(got, [0.5, 2.0 / 3.0, 0.75, 1.0, 1.0]);

        let mut d = Chen14Multi::new(&cfg(&[&[0]], &[0.9], 2, 1));
        for p in [0.7, 0.9, 0.0, 0.0] {
            d.push(&[p]);
        }
        assert!(d.sums[0] < 0.0, "{}", d.sums[0]);
        assert_eq!(d.confidences[0].to_bits(), 0);
    }

    #[test]
    fn later_words_are_searched_from_the_earlier_words_peak() {
        let c = cfg(&[&[0, 1]], &[0.5], 1, 8);
        let in_order: [&[f32]; 3] = [&[0.8, 0.0], &[0.0, 0.0], &[0.0, 0.6]];
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &in_order), [(2, 0)]);
        assert_eq!(d.confidences[0], (0.8f32 * 0.6).sqrt());

        let reversed: [&[f32]; 3] = [&[0.0, 0.6], &[0.0, 0.0], &[0.8, 0.0]];
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &reversed), []);
        assert_eq!(d.confidences[0], 0.0);
    }

    #[test]
    fn three_words_take_the_cube_root() {
        let mut d = Chen14Multi::new(&cfg(&[&[0, 1, 2]], &[0.25], 1, 4));
        for p in [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]] {
            d.push(&p);
        }
        assert_eq!(d.confidences[0], 0.125f32.powf((1.0f64 / 3.0) as f32));
    }

    #[test]
    fn the_best_ratio_wins_and_a_takeover_fires_without_a_drop() {
        let c = cfg(&[&[0], &[1]], &[0.25, 0.375], 1, 1);
        let frames: [&[f32]; 6] = [
            &[0.5, 0.5],
            &[0.5, 0.75],
            &[0.5, 0.875],
            &[0.75, 0.875],
            &[0.0, 0.0],
            &[0.5, 0.0],
        ];
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &frames), [(0, 0), (2, 1), (3, 0), (5, 0)]);

        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &[&[0.5, 0.75]]), [(0, 0)]);
    }

    #[test]
    fn a_hit_skips_the_interval_and_zeroing_re_arms_the_command() {
        let mut c = cfg(&[&[0]], &[0.4], 2, 3);
        c.interval_after_hit = 3;
        c.zero_after_hit = true;
        let on: &[f32] = &[1.0];
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &[on; 6]), [(0, 0), (4, 0)]);
        assert_eq!(d.confidences[0], 0.5);

        c.zero_after_hit = false;
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &[on; 6]), [(0, 0)]);
    }

    #[test]
    fn reset_starts_over() {
        let mut c = cfg(&[&[0]], &[0.4], 2, 3);
        c.interval_after_hit = 100;
        let mut d = Chen14Multi::new(&c);
        assert_eq!(hits(&mut d, &[&[1.0], &[1.0]]), [(0, 0)]);
        d.reset();
        assert_eq!(hits(&mut d, &[&[1.0], &[1.0]]), [(0, 0)]);
    }

    struct Case {
        name: String,
        cfg: Chen14MultiConfig,
        frames: Vec<(Vec<f32>, Option<Vec<u32>>)>,
        hits: Vec<(u64, usize)>,
    }

    fn floats(s: &str) -> Vec<f32> {
        s.split([' ', ':']).map(|v| v.parse().unwrap()).collect()
    }

    /// chen14_multi.txt of probes/spotter/golden_command.py.
    fn cases() -> Vec<Case> {
        let mut cases: Vec<Case> = Vec::new();
        let text = include_str!("../tests/fixtures/chen14_multi.txt");
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let (kind, rest) = line.split_once(' ').unwrap();
            let case = cases.last_mut();
            match kind {
                "case" => {
                    let f: Vec<&str> = rest.split(' ').collect();
                    let commands = f[5]
                        .split(':')
                        .map(|c| c.split(',').map(|u| u.parse().unwrap()).collect());
                    cases.push(Case {
                        name: f[0].into(),
                        cfg: Chen14MultiConfig {
                            commands: commands.collect(),
                            thresholds: floats(f[6]),
                            w_smooth: f[1].parse().unwrap(),
                            w_max: f[2].parse().unwrap(),
                            interval_after_hit: f[3].parse().unwrap(),
                            zero_after_hit: f[4] == "1",
                        },
                        frames: Vec::new(),
                        hits: Vec::new(),
                    });
                }
                "p" => case.unwrap().frames.push((floats(rest), None)),
                "c" => {
                    let conf =
                        (rest != "-").then(|| floats(rest).iter().map(|c| c.to_bits()).collect());
                    case.unwrap().frames.last_mut().unwrap().1 = conf;
                }
                "h" => {
                    let (frame, command) = rest.split_once(' ').unwrap();
                    case.unwrap()
                        .hits
                        .push((frame.parse().unwrap(), command.parse().unwrap()));
                }
                _ => panic!("{line}"),
            }
        }
        cases
    }

    #[test]
    fn agrees_with_the_python_reference() {
        let cases = cases();
        assert_eq!(cases.len(), 3);
        for case in cases {
            let mut d = Chen14Multi::new(&case.cfg);
            let mut got = Vec::new();
            for (frame, (posteriors, want)) in case.frames.iter().enumerate() {
                got.extend(d.push(posteriors).map(|h| (h.frame, h.command)));
                if let Some(want) = want {
                    let conf: Vec<u32> = d.confidences.iter().map(|c| c.to_bits()).collect();
                    // powf differs by one ULP between libms (macOS vs glibc x86-64).
                    let close = conf.len() == want.len()
                        && conf.iter().zip(want).all(|(g, w)| g.abs_diff(*w) <= 1);
                    assert!(close, "{} frame {frame}: {conf:?} vs {want:?}", case.name);
                }
            }
            assert_eq!(got, case.hits, "{}", case.name);
            assert!(!case.hits.is_empty());
        }
    }
}
