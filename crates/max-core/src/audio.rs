use std::f64::consts::PI;

const IN_RATE_HZ: f64 = 48_000.0;
const FACTOR: usize = 3;
const PASS_EDGE_HZ: f64 = 6_500.0;
// Everything above 8 kHz folds into the voice band after decimation to 16 kHz.
const STOP_EDGE_HZ: f64 = 8_000.0;
const STOPBAND_DB: f64 = 70.0;

/// 48 kHz -> 16 kHz. State carries over between `process` calls: chunk boundaries do not change
/// the output.
pub struct Decimator {
    taps: Box<[f64]>,
    history: Vec<i16>,
}

impl Decimator {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let taps = design_lowpass();
        let history = vec![0; taps.len() - 1];
        Self { taps, history }
    }

    pub fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        self.history.extend_from_slice(input);
        let before = out.len();
        out.extend(
            self.history
                .windows(self.taps.len())
                .skip(FACTOR - 1)
                .step_by(FACTOR)
                .map(|window| filter(&self.taps, window)),
        );
        self.history.drain(..(out.len() - before) * FACTOR);
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.history.resize(self.taps.len() - 1, 0);
    }
}

fn filter(taps: &[f64], window: &[i16]) -> i16 {
    let acc: f64 = taps
        .iter()
        .zip(window)
        .map(|(&c, &x)| c * f64::from(x))
        .sum();
    saturate_round(acc)
}

fn design_lowpass() -> Box<[f64]> {
    let transition = 2.0 * PI * (STOP_EDGE_HZ - PASS_EDGE_HZ) / IN_RATE_HZ;
    let cutoff = PI * (PASS_EDGE_HZ + STOP_EDGE_HZ) / IN_RATE_HZ;
    // Kaiser's empirical formulas for the window shape and the filter order.
    let beta = 0.1102 * (STOPBAND_DB - 8.7);
    let half = ((STOPBAND_DB - 8.0) / (2.285 * transition) / 2.0).ceil() as i32;
    let taps: Vec<f64> = (-half..=half)
        .map(|n| {
            let ideal = if n == 0 {
                cutoff / PI
            } else {
                (cutoff * f64::from(n)).sin() / (PI * f64::from(n))
            };
            let r = f64::from(n) / f64::from(half);
            ideal * bessel_i0(beta * (1.0 - r * r).sqrt())
        })
        .collect();
    let dc_gain: f64 = taps.iter().sum();
    taps.into_iter().map(|c| c / dc_gain).collect()
}

fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    while term > sum * f64::EPSILON {
        term *= q / (k * k);
        sum += term;
        k += 1.0;
    }
    sum
}

fn saturate_round(v: f64) -> i16 {
    v.round_ties_even()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

pub fn gain_from_db(db: u32) -> f32 {
    10f32.powf(db as f32 / 20.0)
}

/// Matches pulsar's `to_s16(v * gain)`: saturating, ties-to-even.
pub fn apply_gain(samples: &mut [i16], gain: f32) {
    for s in samples {
        *s = saturate_round(f64::from(f32::from(*s) * gain));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decimate(input: &[i16]) -> Vec<i16> {
        let mut out = Vec::new();
        Decimator::new().process(input, &mut out);
        out
    }

    fn tone(freq_hz: f64, amplitude: f64, len: usize) -> Vec<i16> {
        (0..len)
            .map(|n| {
                (amplitude * (2.0 * PI * freq_hz * n as f64 / IN_RATE_HZ).sin()).round() as i16
            })
            .collect()
    }

    fn noise(len: usize, seed: u32) -> Vec<i16> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 16) as i16
            })
            .collect()
    }

    fn rms(samples: &[i16]) -> f64 {
        let energy: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
        (energy / samples.len() as f64).sqrt()
    }

    fn settle_outputs() -> usize {
        Decimator::new().taps.len() / FACTOR + 1
    }

    fn tone_gain_db(freq_hz: f64, input_len: usize) -> f64 {
        let input = tone(freq_hz, 30_000.0, input_len);
        let out = decimate(&input);
        20.0 * (rms(&out[settle_outputs()..]) / rms(&input)).log10()
    }

    fn unrounded_output(taps: &[f64], input: &[i16], m: usize) -> f64 {
        let end = FACTOR * (m + 1);
        taps.iter()
            .enumerate()
            .filter_map(|(j, &c)| {
                let i = (end + j).checked_sub(taps.len())?;
                Some(c * f64::from(input[i]))
            })
            .sum()
    }

    #[test]
    fn dc_passes_unchanged() {
        for level in [i16::MIN, -12_345, -1, 0, 1, 7, 12_345, i16::MAX] {
            let out = decimate(&vec![level; 3_000]);
            assert!(
                out[settle_outputs()..].iter().all(|&y| y == level),
                "level {level}"
            );
        }
    }

    #[test]
    fn passes_1_khz_within_0_2_db() {
        let gain = tone_gain_db(1_000.0, 48_000);
        assert!(gain.abs() <= 0.2, "{gain} dB");
    }

    #[test]
    fn passes_6_5_khz_within_0_5_db() {
        let gain = tone_gain_db(6_500.0, 48_000);
        assert!(gain.abs() <= 0.5, "{gain} dB");
    }

    #[test]
    fn rejects_9_12_20_khz_by_60_db() {
        for freq in [9_000.0, 12_000.0, 20_000.0] {
            let gain = tone_gain_db(freq, 48_000);
            assert!(gain <= -60.0, "{freq} Hz: {gain} dB");
        }
    }

    #[test]
    fn passband_is_flat_up_to_6_5_khz() {
        for freq in (1..=26).map(|k| f64::from(k) * 250.0) {
            let gain = tone_gain_db(freq, 9_600);
            assert!(gain.abs() <= 0.5, "{freq} Hz: {gain} dB");
        }
    }

    #[test]
    fn stopband_holds_from_8_khz_up() {
        for freq in (32..96).map(|k| f64::from(k) * 250.0) {
            let gain = tone_gain_db(freq, 9_600);
            assert!(gain <= -60.0, "{freq} Hz: {gain} dB");
        }
    }

    #[test]
    fn chunking_does_not_change_output() {
        let input = noise(10_007, 1);
        let whole = decimate(&input);
        let chunkings: [&[usize]; 6] = [
            &[1],
            &[2],
            &[0, 1, 0, 0, 2, 3, 0, 5],
            &[7, 0, 160],
            &[481, 1, 1],
            &[4_000, 0],
        ];
        for sizes in chunkings {
            let mut decimator = Decimator::new();
            let mut out = Vec::new();
            let mut rest = input.as_slice();
            for &size in sizes.iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (chunk, tail) = rest.split_at(size.min(rest.len()));
                decimator.process(chunk, &mut out);
                rest = tail;
            }
            decimator.process(&[], &mut out);
            assert_eq!(out, whole, "chunk sizes {sizes:?}");
        }
    }

    #[test]
    fn emits_one_output_per_three_inputs() {
        let input = noise(1_000, 2);
        let mut decimator = Decimator::new();
        let mut out = Vec::new();
        for (fed, &sample) in input.iter().enumerate() {
            decimator.process(&[sample], &mut out);
            assert_eq!(out.len(), (fed + 1) / FACTOR);
        }
        for len in [0, 1, 2, 3, 4, 5, 6, 299, 300, 301] {
            assert_eq!(decimate(&input[..len]).len(), len / FACTOR, "{len} inputs");
        }
    }

    #[test]
    fn appends_to_existing_output() {
        let mut out = vec![42];
        Decimator::new().process(&[1, 2, 3], &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], 42);
    }

    #[test]
    fn reset_restores_fresh_state() {
        let input = noise(3_001, 3);
        let mut decimator = Decimator::new();
        let mut scratch = Vec::new();
        decimator.process(&noise(1_000, 4), &mut scratch);
        decimator.reset();
        let mut out = Vec::new();
        decimator.process(&input, &mut out);
        assert_eq!(out, decimate(&input));
    }

    #[test]
    fn output_is_every_third_full_rate_sample() {
        let taps = Decimator::new().taps;
        for position in 0..FACTOR {
            let mut input = vec![0; 600];
            input[position] = 10_000;
            let out = decimate(&input);
            for (m, &y) in out.iter().enumerate() {
                let tap = FACTOR * m + FACTOR - 1;
                let expected = tap
                    .checked_sub(position)
                    .and_then(|k| taps.get(k))
                    .map_or(0, |&c| saturate_round(10_000.0 * c));
                assert_eq!(y, expected, "impulse at {position}, output {m}");
            }
        }
    }

    #[test]
    fn overshoot_saturates_instead_of_wrapping() {
        let taps = Decimator::new().taps;
        let mut input = vec![i16::MIN; 600];
        input.extend(vec![i16::MAX; 600]);
        let out = decimate(&input);
        let mut above = 0;
        let mut below = 0;
        for (m, &y) in out.iter().enumerate() {
            let exact = unrounded_output(&taps, &input, m);
            if exact > f64::from(i16::MAX) + 0.5 {
                above += 1;
                assert_eq!(y, i16::MAX, "output {m}: {exact}");
            } else if exact < f64::from(i16::MIN) - 0.5 {
                below += 1;
                assert_eq!(y, i16::MIN, "output {m}: {exact}");
            } else {
                assert_eq!(y, saturate_round(exact), "output {m}");
            }
        }
        assert!(above > 0 && below > 0, "step must ring past full scale");
    }

    #[test]
    fn rounding_is_ties_to_even_and_saturating() {
        let cases = [
            (2.5, 2),
            (3.5, 4),
            (-2.5, -2),
            (-3.5, -4),
            (0.5, 0),
            (2.4, 2),
            (2.6, 3),
            (32_767.5, i16::MAX),
            (1e9, i16::MAX),
            (-32_768.5, i16::MIN),
            (-1e9, i16::MIN),
        ];
        for (v, expected) in cases {
            assert_eq!(saturate_round(v), expected, "{v}");
        }
    }

    #[test]
    fn gain_from_db_is_amplitude_ratio() {
        assert_eq!(gain_from_db(0), 1.0);
        assert!((gain_from_db(20) - 10.0).abs() <= 10.0 * 1e-6);
        assert!((gain_from_db(40) - 100.0).abs() <= 100.0 * 1e-6);
    }

    #[test]
    fn apply_gain_rounds_ties_to_even() {
        let mut samples = [5, 7, -5, -7, 1, -1, 3];
        apply_gain(&mut samples, 0.5);
        assert_eq!(samples, [2, 4, -2, -4, 0, 0, 2]);
    }

    #[test]
    fn apply_gain_saturates() {
        let mut samples = [20_000, -20_000, 16_384, -16_384, 0];
        apply_gain(&mut samples, 2.0);
        assert_eq!(samples, [i16::MAX, i16::MIN, i16::MAX, i16::MIN, 0]);

        let mut loud = [3_277, -3_277, 3_276];
        apply_gain(&mut loud, gain_from_db(20));
        assert_eq!(loud, [i16::MAX, i16::MIN, 32_760]);
    }

    #[test]
    fn unity_gain_keeps_samples() {
        let mut samples = [i16::MIN, -1, 0, 1, i16::MAX];
        apply_gain(&mut samples, gain_from_db(0));
        assert_eq!(samples, [i16::MIN, -1, 0, 1, i16::MAX]);
    }
}
