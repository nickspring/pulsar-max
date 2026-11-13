use crate::chen14::{Chen14, Decision};
use crate::config::SpotterConfig;
use crate::fbank::{FEATURES, FRAME_LEN, FRAME_SHIFT, Fbank};
use crate::freqfilter::FreqFilter;

pub const BATCH_FRAMES: usize = 6;
pub const LOGITS: usize = 2;
/// Bounds how far the filter runs ahead of the model within one push: its ring must still hold
/// the frames a hit is judged on when the hit is decided.
const STEP: usize = BATCH_FRAMES * FRAME_SHIFT;

pub trait AcousticModel {
    type Error;
    fn run(&mut self, features: &[f32], logits: &mut [f32]) -> Result<(), Self::Error>;
    fn reset(&mut self);
    /// Outputs io.sdk drops after every reset (`model_meta::right_context`).
    fn lookahead_frames(&self) -> usize;
}

/// `end` is the stream position just after the hit frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Detection {
    Wake {
        end: u64,
        confidence: f32,
    },
    Blocked {
        end: u64,
        confidence: f32,
        filter: f32,
    },
}

pub struct Spotter<M> {
    fbank: Fbank,
    model: M,
    chen14: Chen14,
    freq: Option<FreqFilter>,
    features: Vec<f32>,
    logits: [f32; BATCH_FRAMES * LOGITS],
    to_drop: usize,
    base: u64,
    next: Option<u64>,
}

impl<M: AcousticModel> Spotter<M> {
    pub fn new(cfg: &SpotterConfig, model: M) -> Self {
        Self {
            fbank: Fbank::new(),
            chen14: Chen14::new(&cfg.chen14),
            freq: cfg.freq_filter.as_ref().map(FreqFilter::new),
            features: Vec::with_capacity(2 * BATCH_FRAMES * FEATURES),
            logits: [0.0; BATCH_FRAMES * LOGITS],
            to_drop: model.lookahead_frames(),
            base: 0,
            next: None,
            model,
        }
    }

    /// The three channels must stay sample-aligned: the filter gate matches frames by position.
    /// A `pos` that does not continue the previous chunk starts over, as after `reset`, and so
    /// does the push after a model error.
    pub fn push(
        &mut self,
        pos: u64,
        omni: &[i16],
        bnr: &[i16],
        raw: &[i16],
        out: &mut Vec<Detection>,
    ) -> Result<(), M::Error> {
        if self.next != Some(pos) {
            if self.next.is_some() {
                self.reset();
            }
            self.base = pos;
        }
        self.next = Some(pos + omni.len() as u64);
        let steps = omni
            .chunks(STEP)
            .zip(bnr.chunks(STEP))
            .zip(raw.chunks(STEP));
        for ((omni, bnr), raw) in steps {
            if let Some(f) = &mut self.freq {
                f.push(raw);
            }
            self.fbank.push(omni, bnr, &mut self.features);
            if let Err(e) = self.run_batches(out) {
                self.reset();
                return Err(e);
            }
        }
        Ok(())
    }

    fn run_batches(&mut self, out: &mut Vec<Detection>) -> Result<(), M::Error> {
        let batch = BATCH_FRAMES * FEATURES;
        while self.features.len() >= batch {
            self.model.run(&self.features[..batch], &mut self.logits)?;
            self.features.drain(..batch);
            self.decide(out);
        }
        Ok(())
    }

    pub fn reset(&mut self) {
        self.fbank.reset();
        self.model.reset();
        self.chen14.reset();
        if let Some(f) = &mut self.freq {
            f.reset();
        }
        self.features.clear();
        self.to_drop = self.model.lookahead_frames();
        self.next = None;
    }

    fn decide(&mut self, out: &mut Vec<Detection>) {
        for &[background, keyword] in self.logits.as_chunks::<LOGITS>().0 {
            if self.to_drop > 0 {
                self.to_drop -= 1;
                continue;
            }
            let posterior = 1.0 / (1.0 + (background - keyword).exp());
            let Some(Decision::Hit { frame, confidence }) = self.chen14.push(posterior) else {
                continue;
            };
            let hit_sample = frame * FRAME_SHIFT as u64;
            let end = self.base + hit_sample + FRAME_LEN as u64;
            let filter = self
                .freq
                .as_ref()
                .and_then(|f| f.blocking_value(hit_sample));
            out.push(match filter {
                Some(filter) => Detection::Blocked {
                    end,
                    confidence,
                    filter,
                },
                None => Detection::Wake { end, confidence },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chen14::Chen14Config;
    use crate::freqfilter::FreqFilterConfig;

    const LOOKAHEAD_FRAMES: usize = 16;
    const ON: [f32; 2] = [-20.0, 20.0];
    const OFF: [f32; 2] = [20.0, -20.0];

    struct Script {
        lookahead: usize,
        on: std::ops::Range<usize>,
        fail_call: Option<usize>,
        outputs: usize,
        calls: usize,
        resets: usize,
        batches: Vec<Vec<f32>>,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                lookahead: LOOKAHEAD_FRAMES,
                on: 0..0,
                fail_call: None,
                outputs: 0,
                calls: 0,
                resets: 0,
                batches: Vec::new(),
            }
        }
    }

    impl AcousticModel for &mut Script {
        type Error = usize;

        fn run(&mut self, features: &[f32], logits: &mut [f32]) -> Result<(), usize> {
            self.calls += 1;
            if self.fail_call == Some(self.calls) {
                return Err(self.calls);
            }
            self.batches.push(features.to_vec());
            for l in logits.as_chunks_mut::<LOGITS>().0 {
                *l = if self.on.contains(&self.outputs) {
                    ON
                } else {
                    OFF
                };
                self.outputs += 1;
            }
            Ok(())
        }

        fn reset(&mut self) {
            self.resets += 1;
            self.outputs = 0;
        }

        fn lookahead_frames(&self) -> usize {
            self.lookahead
        }
    }

    fn config(freq_filter: Option<FreqFilterConfig>) -> SpotterConfig {
        SpotterConfig {
            chen14: Chen14Config {
                w_smooth: 5,
                w_max: 20,
                threshold: 0.861,
                subthreshold: 0.435,
            },
            freq_filter,
            phrase: "алиса".into(),
            version: "test".into(),
        }
    }

    fn freq_filter(threshold: f32) -> FreqFilterConfig {
        FreqFilterConfig {
            frequencies_hz: vec![3501.0, 2089.0, 814.0, 1355.0, 638.0, 4510.0, 2635.0, 486.0],
            threshold,
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

    fn samples_for_frames(frames: usize) -> usize {
        FRAME_LEN + (frames - 1) * FRAME_SHIFT
    }

    fn noise(len: usize, seed: u32) -> Vec<i16> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 20) as i16 - 2048
            })
            .collect()
    }

    fn feed<M: AcousticModel>(
        s: &mut Spotter<M>,
        start: u64,
        audio: &[i16],
        chunk: usize,
    ) -> Result<Vec<Detection>, M::Error> {
        let mut out = Vec::new();
        let mut pos = start;
        for c in audio.chunks(chunk) {
            s.push(pos, c, c, c, &mut out)?;
            pos += c.len() as u64;
        }
        Ok(out)
    }

    fn ends(detections: &[Detection]) -> Vec<u64> {
        detections
            .iter()
            .map(|d| match *d {
                Detection::Wake { end, .. } | Detection::Blocked { end, .. } => end,
            })
            .collect()
    }

    fn frame_end(frame: u64) -> u64 {
        frame * FRAME_SHIFT as u64 + FRAME_LEN as u64
    }

    #[test]
    fn runs_the_model_on_whole_batches_of_frames() {
        let mut script = Script::default();
        let mut s = Spotter::new(&config(None), &mut script);
        let audio = noise(samples_for_frames(2 * BATCH_FRAMES + 3), 1);
        feed(&mut s, 0, &audio, 333).unwrap();
        drop(s);
        assert_eq!(script.calls, 2);
        assert!(
            script
                .batches
                .iter()
                .all(|b| b.len() == BATCH_FRAMES * FEATURES)
        );

        let mut fbank = Fbank::new();
        let mut expected = Vec::new();
        fbank.push(&audio, &audio, &mut expected);
        let fed: Vec<f32> = script.batches.concat();
        assert_eq!(fed, expected[..fed.len()]);
    }

    #[test]
    fn a_hit_is_placed_at_its_frame_after_the_lookahead_is_dropped() {
        let first_on = 30;
        let mut script = Script {
            on: LOOKAHEAD_FRAMES + first_on..usize::MAX,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let out = feed(&mut s, 5_000, &noise(samples_for_frames(120), 2), 1000).unwrap();
        // p steps to 1, and the 5-frame average passes 0.861 only when it reaches 1.
        assert_eq!(
            out,
            [Detection::Wake {
                end: 5_000 + frame_end(first_on as u64 + 4),
                confidence: 1.0,
            }]
        );
    }

    #[test]
    fn outputs_inside_the_lookahead_never_reach_the_decision() {
        let mut script = Script {
            on: 0..LOOKAHEAD_FRAMES,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let out = feed(&mut s, 0, &noise(samples_for_frames(90), 3), 1600).unwrap();
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn reset_restarts_the_model_and_drops_the_lookahead_again() {
        let mut script = Script {
            on: LOOKAHEAD_FRAMES + 10..LOOKAHEAD_FRAMES + 40,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let audio = noise(samples_for_frames(90), 4);
        let first = feed(&mut s, 0, &audio, 1600).unwrap();
        s.push(
            1_000_000,
            &audio[..250],
            &audio[..250],
            &audio[..250],
            &mut Vec::new(),
        )
        .unwrap();
        s.reset();
        let second = feed(&mut s, 0, &audio, 1600).unwrap();
        drop(s);
        assert_eq!(script.resets, 2);
        assert_eq!(ends(&first), [frame_end(14)]);
        assert_eq!(second, first);
    }

    #[test]
    fn a_gap_in_the_stream_starts_over_from_the_new_position() {
        let mut script = Script {
            on: LOOKAHEAD_FRAMES + 10..LOOKAHEAD_FRAMES + 40,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let audio = noise(samples_for_frames(90), 8);
        let before = samples_for_frames(4 * BATCH_FRAMES);
        assert!(
            feed(&mut s, 100, &audio[..before], 1600)
                .unwrap()
                .is_empty()
        );
        let gap = 100 + before as u64 + 16_000;
        let out = feed(&mut s, gap, &audio, 1600).unwrap();
        drop(s);
        assert_eq!(script.resets, 1);
        assert_eq!(ends(&out), [gap + frame_end(14)]);
    }

    #[test]
    fn a_failed_call_starts_over_from_the_next_chunk() {
        let mut script = Script {
            fail_call: Some(2),
            on: LOOKAHEAD_FRAMES + 30..usize::MAX,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let before = noise(samples_for_frames(3 * BATCH_FRAMES), 5);
        assert_eq!(feed(&mut s, 0, &before, before.len()), Err(2));
        let after = noise(samples_for_frames(120), 6);
        let at = before.len() as u64;
        let out = feed(&mut s, at, &after, 320).unwrap();
        drop(s);
        assert_eq!(script.resets, 1);
        assert_eq!(script.batches.len() + 1, script.calls);
        assert_eq!(ends(&out), [at + frame_end(34)]);
    }

    #[test]
    fn the_gate_judges_the_filter_frames_that_end_by_the_hit_frame() {
        let hit_frame = 113;
        let mut script = Script {
            on: LOOKAHEAD_FRAMES + hit_frame - 4..usize::MAX,
            ..Script::default()
        };
        let cfg = freq_filter(1e9);
        let mut s = Spotter::new(&config(Some(cfg.clone())), &mut script);
        let audio = noise(samples_for_frames(200), 11);
        let out = feed(&mut s, 7_000, &audio, 320).unwrap();
        let [Detection::Blocked { filter, .. }] = out[..] else {
            panic!("{out:?}");
        };

        let mut alone = FreqFilter::new(&cfg);
        alone.push(&audio);
        let hit_sample = hit_frame as u64 * FRAME_SHIFT as u64;
        assert_eq!(Some(filter), alone.value_at(hit_sample));
        // 160 * 113 sits just before a filter frame ends (4032 + 5 * 2832), so judging from any
        // later sample, or from the stream position, picks other frames.
        for wrong in [hit_sample + FRAME_LEN as u64, 7_000 + hit_sample] {
            assert_ne!(alone.value_at(hit_sample), alone.value_at(wrong));
        }
    }

    #[test]
    fn the_lookahead_comes_from_the_model() {
        let lookahead = 11;
        let mut script = Script {
            lookahead,
            on: lookahead + 30..usize::MAX,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(None), &mut script);
        let audio = noise(samples_for_frames(120), 13);
        let first = feed(&mut s, 0, &audio, 320).unwrap();
        let gap = audio.len() as u64 + 16_000;
        let after_gap = feed(&mut s, gap, &audio, 320).unwrap();
        assert_eq!(ends(&first), [frame_end(34)]);
        assert_eq!(ends(&after_gap), [gap + frame_end(34)]);
    }

    #[test]
    fn a_real_threshold_judges_the_hit_frame_window() {
        let hit_frame = 113;
        let audio = noise(samples_for_frames(200), 11);
        let hit_sample = hit_frame as u64 * FRAME_SHIFT as u64;
        let mut alone = FreqFilter::new(&freq_filter(0.0));
        alone.push(&audio);
        let right = alone.value_at(hit_sample).unwrap();
        let next = alone.value_at(hit_sample + FRAME_LEN as u64).unwrap();
        assert_ne!(right, next);
        let threshold = (right + next) / 2.0;

        let mut script = Script {
            on: LOOKAHEAD_FRAMES + hit_frame - 4..usize::MAX,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(Some(freq_filter(threshold))), &mut script);
        let out = feed(&mut s, 0, &audio, 320).unwrap();
        assert_eq!(
            matches!(out[..], [Detection::Blocked { .. }]),
            right < threshold,
            "{out:?}"
        );
    }

    #[test]
    fn chunk_size_does_not_change_detections() {
        let script_for = || Script {
            on: LOOKAHEAD_FRAMES + 100..usize::MAX,
            ..Script::default()
        };
        let cfg = config(Some(freq_filter(1e9)));
        let audio = noise(samples_for_frames(500), 12);
        let mut small = script_for();
        let mut s = Spotter::new(&cfg, &mut small);
        let expected = feed(&mut s, 0, &audio, 320).unwrap();
        assert!(matches!(expected[..], [Detection::Blocked { .. }]));
        let mut whole = script_for();
        let mut s = Spotter::new(&cfg, &mut whole);
        assert_eq!(feed(&mut s, 0, &audio, audio.len()).unwrap(), expected);
    }

    #[test]
    fn the_frequency_filter_demotes_a_hit_at_or_below_its_threshold() {
        let script_for = || Script {
            on: LOOKAHEAD_FRAMES + 100..usize::MAX,
            ..Script::default()
        };
        let audio = noise(samples_for_frames(200), 6);

        let mut passing = script_for();
        let mut s = Spotter::new(&config(Some(freq_filter(-1e9))), &mut passing);
        let out = feed(&mut s, 0, &audio, 1600).unwrap();
        assert!(matches!(out[..], [Detection::Wake { .. }]), "{out:?}");

        let mut blocked = script_for();
        let mut s = Spotter::new(&config(Some(freq_filter(1e9))), &mut blocked);
        let out = feed(&mut s, 0, &audio, 1600).unwrap();
        let [Detection::Blocked { end, filter, .. }] = out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(end, frame_end(104));
        assert!(filter.is_finite() && filter <= 1e9);
    }

    #[test]
    fn after_a_gap_the_filter_judges_only_new_audio() {
        let script_for = || Script {
            on: LOOKAHEAD_FRAMES + 100..usize::MAX,
            ..Script::default()
        };
        let cfg = config(Some(freq_filter(1e9)));
        let before = noise(samples_for_frames(60), 9);
        let after = noise(samples_for_frames(200), 10);
        let gap = before.len() as u64 + 16_000;

        let mut gapped = script_for();
        let mut s = Spotter::new(&cfg, &mut gapped);
        assert!(feed(&mut s, 0, &before, 1600).unwrap().is_empty());
        let out = feed(&mut s, gap, &after, 1600).unwrap();

        let mut fresh = script_for();
        let mut s = Spotter::new(&cfg, &mut fresh);
        let expected = feed(&mut s, gap, &after, 1600).unwrap();
        assert!(matches!(expected[..], [Detection::Blocked { .. }]));
        assert_eq!(out, expected);
    }

    #[test]
    fn a_hit_before_the_first_filter_frame_passes() {
        let mut script = Script {
            on: LOOKAHEAD_FRAMES..usize::MAX,
            ..Script::default()
        };
        let mut s = Spotter::new(&config(Some(freq_filter(1e9))), &mut script);
        let out = feed(&mut s, 0, &noise(samples_for_frames(60), 7), 1600).unwrap();
        assert!(matches!(out[..], [Detection::Wake { .. }]), "{out:?}");
    }
}
