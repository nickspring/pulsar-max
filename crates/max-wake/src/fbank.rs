use crate::spectrum::Spectrum;

pub const RATE: u32 = 16_000;
pub const FRAME_LEN: usize = 400;
pub const FRAME_SHIFT: usize = 160;
pub const MEL_BINS: usize = 40;
pub const FEATURES: usize = 2 * MEL_BINS;

const PREEMPH: f32 = 0.97;
const DITHER: f32 = 0.1;
const MEL_LOW_HZ: f64 = 20.0;
const MEL_HIGH_HZ: f64 = 8000.0;

struct MelBank {
    first_bin: usize,
    weights: Box<[f32]>,
}

pub struct Fbank {
    // One spectrum for both channels: the device's dither index runs omni frame, then bnr frame.
    spectrum: Spectrum,
    banks: Box<[MelBank]>,
    power: Box<[f32]>,
    omni: Vec<i16>,
    bnr: Vec<i16>,
}

impl Fbank {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let spectrum = Spectrum::new(FRAME_LEN, PREEMPH, DITHER, true);
        let bins = spectrum.bins();
        Self {
            banks: mel_banks(bins),
            power: vec![0.0; bins].into_boxed_slice(),
            spectrum,
            omni: Vec::new(),
            bnr: Vec::new(),
        }
    }

    /// Two time-aligned channels; appends FEATURES values per completed frame.
    pub fn push(&mut self, omni: &[i16], bnr: &[i16], out: &mut Vec<f32>) {
        self.omni.extend_from_slice(omni);
        self.bnr.extend_from_slice(bnr);
        let ready = self.omni.len().min(self.bnr.len());
        let mut start = 0;
        while start + FRAME_LEN <= ready {
            for channel in [&self.omni, &self.bnr] {
                self.spectrum
                    .power(&channel[start..start + FRAME_LEN], &mut self.power);
                out.extend(self.banks.iter().map(|bank| {
                    let power = &self.power[bank.first_bin..bank.first_bin + bank.weights.len()];
                    let energy = bank
                        .weights
                        .iter()
                        .zip(power)
                        .fold(0.0f32, |acc, (&w, &p)| acc + w * p);
                    energy.ln()
                }));
            }
            start += FRAME_SHIFT;
        }
        self.omni.drain(..start);
        self.bnr.drain(..start);
    }

    pub fn reset(&mut self) {
        self.omni.clear();
        self.bnr.clear();
        self.spectrum.rewind_dither();
    }
}

// f64 like proto.py; the binary's f32 weights differ only in the last bits.
fn mel_banks(bins: usize) -> Box<[MelBank]> {
    let mel = |hz: f64| 1127.0 * (1.0 + hz / 700.0).ln();
    let fft_len = (bins - 1) * 2;
    let low = mel(MEL_LOW_HZ);
    let delta = (mel(MEL_HIGH_HZ) - low) / (MEL_BINS + 1) as f64;
    let bin_mel: Vec<f64> = (0..bins)
        .map(|i| mel((i * RATE as usize) as f64 / fft_len as f64))
        .collect();
    (0..MEL_BINS)
        .map(|b| {
            let left = low + b as f64 * delta;
            let center = low + (b + 1) as f64 * delta;
            let right = low + (b + 2) as f64 * delta;
            let inside = |m: f64| m > left && m < right;
            let first_bin = bin_mel.iter().position(|&m| inside(m)).unwrap_or(0);
            let weights = bin_mel[first_bin..]
                .iter()
                .take_while(|&&m| inside(m))
                .map(|&m| {
                    let w = if m <= center {
                        (m - left) / (center - left)
                    } else {
                        (right - m) / (right - center)
                    };
                    w as f32
                })
                .collect();
            MelBank { first_bin, weights }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: u32, n: usize) -> Vec<i32> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (x >> 16) as i32 - 32768
            })
            .collect()
    }

    const SAMPLES: usize = 2400;

    // probes/spotter/golden_signal.py, fbank_input.
    fn golden_input() -> (Vec<i16>, Vec<i16>) {
        let (a, b) = (lcg(1, SAMPLES), lcg(2, SAMPLES));
        let mut omni: Vec<i16> = (0..SAMPLES)
            .map(|i| ((a[i] >> 3) + ((i as i32 * 7) % 160) * 50 - 4000) as i16)
            .collect();
        let mut bnr: Vec<i16> = (0..SAMPLES)
            .map(|i| ((b[i] >> 4) + if (i / 40) % 2 == 1 { 2000 } else { -2000 }) as i16)
            .collect();
        omni[1600..2000].fill(0);
        bnr[1600..2000].fill(0);
        (omni, bnr)
    }

    fn golden_features() -> Vec<f32> {
        include_bytes!("../tests/fixtures/fbank.f32le")
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect()
    }

    #[test]
    fn matches_proto_frontend() {
        let (omni, bnr) = golden_input();
        let mut fbank = Fbank::new();
        let mut out = Vec::new();
        fbank.push(&omni, &bnr, &mut out);
        let want = golden_features();
        assert_eq!(out.len(), want.len());
        assert_eq!(
            out.len(),
            ((SAMPLES - FRAME_LEN) / FRAME_SHIFT + 1) * FEATURES
        );
        let worst = out
            .iter()
            .zip(&want)
            .map(|(g, w)| (g - w).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-5, "max abs difference {worst}");
    }

    #[test]
    fn chunking_does_not_change_features() {
        let (omni, bnr) = golden_input();
        let mut whole = Vec::new();
        Fbank::new().push(&omni, &bnr, &mut whole);

        let mut fbank = Fbank::new();
        let mut out = Vec::new();
        let (mut o, mut b) = (0, 0);
        for (step, size) in [1usize, 37, 160, 399, 401, 513, 7, 1000]
            .iter()
            .cycle()
            .enumerate()
        {
            if o == SAMPLES && b == SAMPLES {
                break;
            }
            let o_end = (o + size).min(SAMPLES);
            let b_end = (b + size * (step % 3)).min(SAMPLES);
            let before = out.len();
            fbank.push(&omni[o..o_end], &bnr[b..b_end], &mut out);
            assert_eq!((out.len() - before) % FEATURES, 0);
            (o, b) = (o_end, b_end);
        }
        assert_eq!(out, whole);
    }

    #[test]
    fn reset_restarts_framing_and_dither() {
        let (omni, bnr) = golden_input();
        let mut fbank = Fbank::new();
        let mut first = Vec::new();
        fbank.push(&omni[..1000], &bnr[..900], &mut first);
        fbank.reset();
        let mut again = Vec::new();
        fbank.push(&omni, &bnr, &mut again);
        let mut fresh = Vec::new();
        Fbank::new().push(&omni, &bnr, &mut fresh);
        assert_eq!(again, fresh);
    }

    #[test]
    fn short_input_emits_nothing() {
        let mut fbank = Fbank::new();
        let mut out = Vec::new();
        fbank.push(&[0; FRAME_LEN - 1], &[0; FRAME_LEN], &mut out);
        assert!(out.is_empty());
        fbank.push(&[0; 1], &[], &mut out);
        assert_eq!(out.len(), FEATURES);
        assert!(out.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn mel_banks_span_20_to_8000_hz() {
        let banks = mel_banks(257);
        assert_eq!(banks.len(), MEL_BINS);
        let span = |b: &MelBank| (b.first_bin, b.first_bin + b.weights.len() - 1);
        assert_eq!(span(&banks[0]), (1, 3));
        assert_eq!(span(&banks[39]), (225, 256));
        assert!(banks[39].weights[31] < 1e-12);
        for bank in banks.iter() {
            assert!(bank.weights.iter().all(|&w| w > 0.0 && w <= 1.0));
        }
    }
}
