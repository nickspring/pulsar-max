use std::f64::consts::PI;
use std::sync::OnceLock;

pub const DITHER_TABLE_LEN: usize = 2048;

/// `numpy.random.RandomState(42).randn(2048)` as f32: io.sdk dithers with this fixed table.
pub fn dither_table() -> &'static [f32; DITHER_TABLE_LEN] {
    static TABLE: OnceLock<[f32; DITHER_TABLE_LEN]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut gauss = LegacyGauss::new(42);
        std::array::from_fn(|_| gauss.next() as f32)
    })
}

const MT_N: usize = 624;
const MT_M: usize = 397;

struct Mt19937 {
    state: [u32; MT_N],
    pos: usize,
}

impl Mt19937 {
    fn new(seed: u32) -> Self {
        let mut state = [0u32; MT_N];
        state[0] = seed;
        for i in 1..MT_N {
            let prev = state[i - 1];
            state[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        Self { state, pos: MT_N }
    }

    fn twist(&mut self) {
        for i in 0..MT_N {
            let y = (self.state[i] & 0x8000_0000) | (self.state[(i + 1) % MT_N] & 0x7fff_ffff);
            let mag = if y & 1 == 1 { 0x9908_b0df } else { 0 };
            self.state[i] = self.state[(i + MT_M) % MT_N] ^ (y >> 1) ^ mag;
        }
        self.pos = 0;
    }

    fn next_u32(&mut self) -> u32 {
        if self.pos == MT_N {
            self.twist();
        }
        let mut y = self.state[self.pos];
        self.pos += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    fn next_f64(&mut self) -> f64 {
        let a = self.next_u32() >> 5;
        let b = self.next_u32() >> 6;
        (f64::from(a) * 67_108_864.0 + f64::from(b)) / 9_007_199_254_740_992.0
    }
}

struct LegacyGauss {
    mt: Mt19937,
    cached: Option<f64>,
}

impl LegacyGauss {
    fn new(seed: u32) -> Self {
        Self {
            mt: Mt19937::new(seed),
            cached: None,
        }
    }

    fn next(&mut self) -> f64 {
        if let Some(v) = self.cached.take() {
            return v;
        }
        loop {
            let x1 = 2.0 * self.mt.next_f64() - 1.0;
            let x2 = 2.0 * self.mt.next_f64() - 1.0;
            let r2 = x1 * x1 + x2 * x2;
            if r2 < 1.0 && r2 != 0.0 {
                let f = (-2.0 * r2.ln() / r2).sqrt();
                self.cached = Some(f * x1);
                return f * x2;
            }
        }
    }
}

/// See docs/max.md, "Ключевое слово".
pub struct Spectrum {
    frame_len: usize,
    preemph: f32,
    dither: f32,
    remove_dc: bool,
    dither_pos: usize,
    window: Box<[f32]>,
    frame: Box<[f32]>,
    fft: RealFft,
}

impl Spectrum {
    pub fn new(frame_len: usize, preemph: f32, dither: f32, remove_dc: bool) -> Self {
        Self {
            frame_len,
            preemph,
            dither,
            remove_dc,
            dither_pos: 0,
            window: povey_window(frame_len),
            frame: vec![0.0; frame_len].into_boxed_slice(),
            fft: RealFft::new(frame_len.next_power_of_two().max(2)),
        }
    }

    pub fn bins(&self) -> usize {
        self.fft.n / 2 + 1
    }

    pub fn power(&mut self, frame: &[i16], out: &mut [f32]) {
        assert_eq!(frame.len(), self.frame_len, "frame length");
        assert_eq!(out.len(), self.bins(), "output bins");
        let x = &mut self.frame;
        for (x, &s) in x.iter_mut().zip(frame) {
            *x = f32::from(s);
        }
        if self.dither != 0.0 {
            let table = dither_table();
            for x in x.iter_mut() {
                *x += self.dither * table[self.dither_pos];
                self.dither_pos = (self.dither_pos + 1) % DITHER_TABLE_LEN;
            }
        }
        if self.remove_dc {
            // Sequential f32 sum as in the binary, not numpy's pairwise one.
            let mean = x.iter().fold(0.0f32, |acc, &v| acc + v) / self.frame_len as f32;
            for x in x.iter_mut() {
                *x -= mean;
            }
        }
        if self.preemph != 0.0
            && let Some(&first) = x.first()
        {
            let mut prev = first;
            for v in x.iter_mut().skip(1) {
                let cur = *v;
                *v = cur - self.preemph * prev;
                prev = cur;
            }
            x[0] = first - self.preemph * first;
        }
        for (x, &w) in x.iter_mut().zip(self.window.iter()) {
            *x *= w;
        }
        self.fft.power(x, out);
    }

    pub(crate) fn rewind_dither(&mut self) {
        self.dither_pos = 0;
    }
}

// f64 angle, f32 cosine: as in the binary.
fn povey_window(n: usize) -> Box<[f32]> {
    let denom = n.saturating_sub(1) as f64;
    (0..n)
        .map(|i| {
            let angle = (2.0 * PI * i as f64 / denom) as f32;
            (0.5 - 0.5 * angle.cos()).powf(0.85)
        })
        .collect()
}

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    fn add(self, o: Self) -> Self {
        Self {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }

    fn sub(self, o: Self) -> Self {
        Self {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }

    fn mul(self, o: Self) -> Self {
        Self {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }

    fn conj(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }

    fn scale(self, k: f64) -> Self {
        Self {
            re: self.re * k,
            im: self.im * k,
        }
    }

    fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
}

// f64 where the binary has f32 pffft: closest to both the device and the numpy references.
struct RealFft {
    n: usize,
    twiddle: Box<[Complex]>,
    bit_rev: Box<[usize]>,
    buf: Box<[Complex]>,
}

impl RealFft {
    fn new(n: usize) -> Self {
        debug_assert!(n >= 2 && n.is_power_of_two());
        let half = n / 2;
        let twiddle = (0..half)
            .map(|k| {
                let angle = -2.0 * PI * k as f64 / n as f64;
                Complex {
                    re: angle.cos(),
                    im: angle.sin(),
                }
            })
            .collect();
        let bits = half.trailing_zeros();
        let bit_rev = (0..half)
            .map(|i| {
                i.reverse_bits()
                    .checked_shr(usize::BITS - bits)
                    .unwrap_or(0)
            })
            .collect();
        Self {
            n,
            twiddle,
            bit_rev,
            buf: vec![Complex::default(); half].into_boxed_slice(),
        }
    }

    fn power(&mut self, input: &[f32], out: &mut [f32]) {
        let half = self.n / 2;
        let sample = |i: usize| input.get(i).map_or(0.0, |&v| f64::from(v));
        for (k, c) in self.buf.iter_mut().enumerate() {
            *c = Complex {
                re: sample(2 * k),
                im: sample(2 * k + 1),
            };
        }
        self.complex_fft();

        let z0 = self.buf[0];
        out[0] = ((z0.re + z0.im) * (z0.re + z0.im)) as f32;
        out[half] = ((z0.re - z0.im) * (z0.re - z0.im)) as f32;
        let minus_half_i = Complex { re: 0.0, im: -0.5 };
        for (k, out) in out.iter_mut().enumerate().take(half).skip(1) {
            let a = self.buf[k];
            let b = self.buf[half - k].conj();
            let even = a.add(b).scale(0.5);
            let odd = a.sub(b).mul(minus_half_i);
            *out = even.add(self.twiddle[k].mul(odd)).norm_sqr() as f32;
        }
    }

    fn complex_fft(&mut self) {
        let m = self.buf.len();
        for i in 0..m {
            let j = self.bit_rev[i];
            if i < j {
                self.buf.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= m {
            let stride = self.n / len;
            for chunk in self.buf.chunks_exact_mut(len) {
                let (lo, hi) = chunk.split_at_mut(len / 2);
                for (k, (a, b)) in lo.iter_mut().zip(hi.iter_mut()).enumerate() {
                    let t = b.mul(self.twiddle[k * stride]);
                    *b = a.sub(t);
                    *a = a.add(t);
                }
            }
            len *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_f32le(bytes: &[u8]) -> Vec<f32> {
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect()
    }

    fn naive_power(x: &[f64], n: usize) -> Vec<f64> {
        (0..=n / 2)
            .map(|k| {
                let (mut re, mut im) = (0.0, 0.0);
                for (i, &v) in x.iter().enumerate() {
                    let angle = -2.0 * PI * ((k * i) % n) as f64 / n as f64;
                    re += v * angle.cos();
                    im += v * angle.sin();
                }
                re * re + im * im
            })
            .collect()
    }

    fn assert_close(got: &[f32], want: &[f64], rel: f64) {
        let scale = want.iter().fold(0.0f64, |m, &v| m.max(v.abs()));
        assert_eq!(got.len(), want.len());
        for (k, (&g, &w)) in got.iter().zip(want).enumerate() {
            let f32_rounding = f64::from(f32::EPSILON) * w.abs();
            assert!(
                (f64::from(g) - w).abs() <= f32_rounding + rel * scale,
                "bin {k}: {g} vs {w} (scale {scale})"
            );
        }
    }

    #[test]
    fn dither_table_is_numpy_randn_seed_42() {
        let want = read_f32le(include_bytes!("../tests/fixtures/dither42.f32le"));
        assert_eq!(want.len(), DITHER_TABLE_LEN);
        let got = dither_table();
        for (i, (&g, &w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.to_bits(), w.to_bits(), "value {i}: {g} vs {w}");
        }
        assert!(std::ptr::eq(got, dither_table()));
    }

    #[test]
    fn mt19937_matches_reference_sequence() {
        // mt19937ar.c, init_genrand(5489).
        let mut mt = Mt19937::new(5489);
        let first: Vec<u32> = (0..3).map(|_| mt.next_u32()).collect();
        assert_eq!(first, [3_499_211_612, 581_869_302, 3_890_346_734]);
    }

    #[test]
    fn fft_matches_naive_dft() {
        for n in [2usize, 4, 8, 16, 64, 512] {
            let input: Vec<f32> = (0..n)
                .map(|i| ((i * 37 + 11) % 23) as f32 - 11.0 + (i as f32 * 0.7).sin())
                .collect();
            let mut fft = RealFft::new(n);
            let mut out = vec![0.0; n / 2 + 1];
            fft.power(&input, &mut out);
            let x: Vec<f64> = input.iter().map(|&v| f64::from(v)).collect();
            assert_close(&out, &naive_power(&x, n), 1e-12);
        }
    }

    #[test]
    fn fft_zero_pads_short_input() {
        let input: Vec<f32> = (0..300).map(|i| ((i * 13) % 17) as f32 - 8.0).collect();
        let mut fft = RealFft::new(512);
        let mut out = vec![0.0; 257];
        fft.power(&input, &mut out);
        let x: Vec<f64> = input.iter().map(|&v| f64::from(v)).collect();
        assert_close(&out, &naive_power(&x, 512), 1e-12);
    }

    #[test]
    fn bins_cover_the_padded_fft() {
        assert_eq!(Spectrum::new(400, 0.97, 0.1, true).bins(), 257);
        assert_eq!(Spectrum::new(4032, 0.72, 0.1, true).bins(), 2049);
        assert_eq!(Spectrum::new(512, 0.0, 0.0, false).bins(), 257);
        assert_eq!(Spectrum::new(1, 0.0, 0.0, false).bins(), 2);
    }

    fn windowed_power(window: &[f32], x: &[f32], n: usize) -> Vec<f64> {
        let y: Vec<f64> = x
            .iter()
            .zip(window)
            .map(|(&v, &w)| f64::from(v * w))
            .collect();
        naive_power(&y, n)
    }

    #[test]
    fn dither_index_runs_across_frames_and_wraps() {
        let frame_len = 300;
        let mut spectrum = Spectrum::new(frame_len, 0.0, 0.5, false);
        let window = povey_window(frame_len);
        let zeros = vec![0i16; frame_len];
        let mut out = vec![0.0; spectrum.bins()];
        let table = dither_table();
        for call in 0..8 {
            spectrum.power(&zeros, &mut out);
            let x: Vec<f32> = (0..frame_len)
                .map(|i| 0.5 * table[(call * frame_len + i) % DITHER_TABLE_LEN])
                .collect();
            assert_close(&out, &windowed_power(&window, &x, 512), 1e-6);
        }
        spectrum.rewind_dither();
        spectrum.power(&zeros, &mut out);
        let x: Vec<f32> = table[..frame_len].iter().map(|&t| 0.5 * t).collect();
        assert_close(&out, &windowed_power(&window, &x, 512), 1e-6);
    }

    #[test]
    fn remove_dc_cancels_a_constant_frame() {
        let mut spectrum = Spectrum::new(400, 0.97, 0.0, true);
        let mut out = vec![1.0; spectrum.bins()];
        spectrum.power(&[1000; 400], &mut out);
        assert!(out.iter().all(|&p| p == 0.0));

        let mut kept = Spectrum::new(400, 0.0, 0.0, false);
        kept.power(&[1000; 400], &mut out);
        assert!(out[0] > 1e9);
    }

    #[test]
    fn preemphasis_uses_the_unfiltered_previous_sample() {
        let n = 16;
        let c = 0.5;
        let mut spectrum = Spectrum::new(n, c, 0.0, false);
        let mut frame = vec![0i16; n];
        frame[3] = 1000;
        let mut out = vec![0.0; spectrum.bins()];
        spectrum.power(&frame, &mut out);
        let mut x = vec![0.0f32; n];
        x[3] = 1000.0;
        x[4] = -c * 1000.0;
        assert_close(&out, &windowed_power(&povey_window(n), &x, n), 1e-6);
    }

    #[test]
    fn povey_window_shape() {
        let w = povey_window(400);
        assert_eq!(w[0], 0.0);
        assert!(w[399] < 1e-6);
        assert!((w[199] - 1.0).abs() < 1e-4);
        assert_eq!(povey_window(1).len(), 1);
    }
}
