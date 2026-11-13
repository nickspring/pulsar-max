//! io.sdk's music spectrogram for the 25x16 screen. Each `mul_add` mirrors an `fmadd` in io.sdk.

use std::f64::consts::PI;

const WIDTH: usize = 25;
const HEIGHT: usize = 16;
const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;

const NFFT: usize = 1024;
const HALF: usize = NFFT / 2;
const BINS: usize = HALF + 1;
const INTERVAL_NS: u64 = 40_000_000;
const SECOND_NS: u64 = 1_000_000_000;
const THRESHOLD_DB: f64 = -80.0;

const NORM_FRAMES: usize = 10;
const NORM_FLOOR: f32 = 1e-5;

const MAX_BANDS: usize = 26;
const MAX_SPLINE: usize = 10;
const PAD_ROWS: usize = 4;
const PAD_COLS: usize = 6;
const PAD_GAMMA: f32 = 0.5;
const BRICK_LEVELS: f32 = 7.0;
const BRICK_FADING: f32 = 0.2;

type Grid = [f32; WIDTH * HEIGHT];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Flame,
    WaveCenter,
    Pads,
    BarsBottom,
    BarsCenter,
    BricksSmall,
}

pub const STYLES: [Style; 6] = [
    Style::Flame,
    Style::WaveCenter,
    Style::Pads,
    Style::BarsBottom,
    Style::BarsCenter,
    Style::BricksSmall,
];

impl Style {
    pub fn name(self) -> &'static str {
        match self {
            Style::Flame => "flame",
            Style::WaveCenter => "waveCenter",
            Style::Pads => "pads",
            Style::BarsBottom => "barsBottom",
            Style::BarsCenter => "barsCenter",
            Style::BricksSmall => "bricksSmall",
        }
    }

    pub fn from_name(name: &str) -> Option<Style> {
        STYLES.into_iter().find(|s| s.name() == name)
    }

    fn preset(self) -> Preset {
        let wave = |spline, bar_width, fading, bars, bottom| {
            Draw::Wave(Wave {
                spline,
                bar_width,
                fading,
                bars,
                bottom,
            })
        };
        let (bands, fmin, fmax, smoothing, draw) = match self {
            Style::Flame => (18, 40.0, 3000.0, 0.8, wave(10, 0.0, 0.25, false, true)),
            Style::WaveCenter => (12, 30.0, 3200.0, 0.8, wave(5, 0.0, 0.25, false, false)),
            Style::Pads => (PAD_ROWS * PAD_COLS, 100.0, 3000.0, 0.9, Draw::Pads),
            Style::BarsBottom => (18, 40.0, 3000.0, 0.8, wave(10, 1.0, 0.2, true, true)),
            Style::BarsCenter => (26, 40.0, 2000.0, 0.8, wave(5, 1.0, 0.2, true, false)),
            Style::BricksSmall => (8, 40.0, 3000.0, 0.8, Draw::Bricks),
        };
        Preset {
            bands,
            fmin,
            fmax,
            smoothing,
            draw,
        }
    }
}

/// io.sdk's "random" preset: never the current style.
pub fn pick_random(current: Option<Style>, random: u64) -> Style {
    match current {
        None => STYLES[(random % STYLES.len() as u64) as usize],
        Some(current) => {
            let i = (random % (STYLES.len() as u64 - 1)) as usize;
            STYLES[if i >= current as usize { i + 1 } else { i }]
        }
    }
}

#[derive(Clone, Copy)]
struct Preset {
    bands: usize,
    fmin: f32,
    fmax: f32,
    smoothing: f32,
    draw: Draw,
}

#[derive(Clone, Copy)]
enum Draw {
    Wave(Wave),
    Pads,
    Bricks,
}

#[derive(Clone, Copy)]
struct Wave {
    spline: usize,
    /// The minimum bar height in pixels, despite io.sdk's name.
    bar_width: f32,
    fading: f32,
    bars: bool,
    bottom: bool,
}

pub type Frame = [u8; FRAME_BYTES];

/// One message of gst's `spectrum` element: per-bin dB, averaged over the interval's FFTs.
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    rate: u32,
    db: Box<[f32]>,
}

pub struct Spectrogram {
    style: Style,
    filters: Filters,
    bands: Vec<f32>,
}

impl Spectrogram {
    pub fn new(style: Style) -> Self {
        Spectrogram {
            style,
            filters: Filters::new(style.preset().bands),
            bands: Vec::with_capacity(MAX_BANDS),
        }
    }

    pub fn style(&self) -> Style {
        self.style
    }

    pub fn set_style(&mut self, style: Style) {
        if style != self.style {
            self.style = style;
            self.filters = Filters::new(style.preset().bands);
        }
    }

    pub fn reset(&mut self) {
        self.filters = Filters::new(self.style.preset().bands);
    }

    /// None: no spectrum is due, so io.sdk feeds its filters a zero vector and the picture decays.
    pub fn render(&mut self, spectrum: Option<&Spectrum>) -> Frame {
        let preset = self.style.preset();
        match spectrum {
            Some(s) => to_bands(&s.db, s.rate, &preset, &mut self.bands),
            None => {
                self.bands.clear();
                self.bands.resize(preset.bands, 0.0);
            }
        }
        self.filters.apply(preset.smoothing, &mut self.bands);
        to_frame(&draw(&preset.draw, &self.bands))
    }
}

/// gst's `spectrum interval=40000000 bands=513 threshold=-80` over a stream at `rate`.
pub struct Analyzer {
    rate: u32,
    fft: RealFft,
    window: Vec<f64>,
    ring: Vec<f32>,
    ring_pos: usize,
    input: Vec<f32>,
    spectrum: Vec<Complex>,
    acc: Vec<f32>,
    ffts: u32,
    frames: u64,
    todo: u64,
    per_interval: u64,
    error_per_interval: u64,
    accumulated_error: u64,
    start: u64,
    position: u64,
}

impl Analyzer {
    pub fn new(rate: u32) -> Self {
        let scaled = INTERVAL_NS * u64::from(rate);
        let per_interval = (scaled / SECOND_NS).max(1);
        Analyzer {
            rate,
            fft: RealFft::new(),
            window: (0..NFFT)
                .map(|i| 0.53836 - 0.46164 * (2.0 * PI * i as f64 / NFFT as f64).cos())
                .collect(),
            ring: vec![0.0; NFFT],
            ring_pos: 0,
            input: vec![0.0; NFFT],
            spectrum: vec![Complex::default(); BINS],
            acc: vec![0.0; BINS],
            ffts: 0,
            frames: 0,
            todo: per_interval,
            per_interval,
            error_per_interval: scaled % SECOND_NS,
            accumulated_error: 0,
            start: 0,
            position: 0,
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn reset(&mut self) {
        *self = Analyzer::new(self.rate);
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    /// Interleaved L,R. A spectrum comes with its interval's first position once its last FFT
    /// has run: at 44.1 and 48 kHz, after the interval's first 1024 frames.
    pub fn push(&mut self, stereo: &[i16], out: &mut Vec<(u64, Spectrum)>) {
        for &[left, right] in stereo.as_chunks::<2>().0 {
            self.ring[self.ring_pos] =
                (f32::from(left) / 32767.0 + f32::from(right) / 32767.0) / 2.0;
            self.ring_pos = (self.ring_pos + 1) % NFFT;
            self.position += 1;
            self.frames += 1;
            let full = self.frames == self.todo;
            if self.frames.is_multiple_of(NFFT as u64) || (full && self.ffts == 0) {
                self.analyse();
                if full || self.frames + NFFT as u64 > self.todo {
                    out.push((self.start, self.average()));
                }
            }
            if full {
                self.next_interval();
            }
        }
    }

    fn analyse(&mut self) {
        let ring = self.ring[self.ring_pos..]
            .iter()
            .chain(&self.ring[..self.ring_pos]);
        for ((x, &r), w) in self.input.iter_mut().zip(ring).zip(&self.window) {
            *x = (f64::from(r) * w) as f32;
        }
        self.fft.run(&self.input, &mut self.spectrum);
        for (acc, c) in self.acc.iter_mut().zip(&self.spectrum) {
            let power = f64::from(c.re * c.re) + f64::from(c.im * c.im);
            let db = (10.0 * (power / (NFFT * NFFT) as f64).log10()).max(THRESHOLD_DB);
            *acc = (f64::from(*acc) + db) as f32;
        }
        self.ffts += 1;
    }

    fn average(&self) -> Spectrum {
        let n = self.ffts as f32;
        Spectrum {
            rate: self.rate,
            db: self.acc.iter().map(|&a| a / n).collect(),
        }
    }

    // gst carries the rounding error of frames per interval and adds a frame once it reaches one.
    fn next_interval(&mut self) {
        self.start += self.frames;
        self.frames = 0;
        self.ffts = 0;
        self.acc.fill(0.0);
        self.todo = self.per_interval;
        if self.accumulated_error >= SECOND_NS {
            self.accumulated_error -= SECOND_NS;
            self.todo += 1;
        }
        self.accumulated_error += self.error_per_interval;
    }
}

fn to_bands(db: &[f32], rate: u32, preset: &Preset, out: &mut Vec<f32>) {
    let mut power = [0f32; BINS];
    for (p, &d) in power.iter_mut().zip(db) {
        *p = 10f32.powf(d / 10.0);
    }
    let at = |bin: usize| power[bin.min(BINS - 1)];
    let bin_hz = rate as f32 / NFFT as f32;
    let lo_log = (preset.fmin / bin_hz).log10();
    let span = (preset.fmax / bin_hz).log10() - lo_log;
    let n = preset.bands;
    let edge = |i: usize| 10f32.powf(span.mul_add(i as f32 / n as f32, lo_log));
    out.clear();
    for i in 0..n {
        let (lo, hi) = (edge(i), edge(i + 1));
        let (first, last) = (lo.ceil() as usize, hi.floor() as usize);
        if last < first {
            out.push(at(last) * (hi - lo));
            continue;
        }
        let mut sum = if first > 0 {
            at(first - 1) * (first as f32 - lo)
        } else {
            0.0
        };
        for bin in first..last {
            sum += at(bin);
        }
        // io.sdk compares the last bin with the band count, not the bin count.
        if last < n {
            sum = at(last).mul_add(hi - last as f32, sum);
        }
        out.push(sum);
    }
}

struct Filters {
    smoothed: Vec<f32>,
    maxima: [f32; NORM_FRAMES],
    oldest: usize,
    sum: f32,
}

impl Filters {
    fn new(bands: usize) -> Self {
        Filters {
            smoothed: vec![0.0; bands],
            maxima: [0.0; NORM_FRAMES],
            oldest: 0,
            sum: 0.0,
        }
    }

    fn apply(&mut self, smoothing: f32, bands: &mut [f32]) {
        for (s, x) in self.smoothed.iter_mut().zip(bands.iter_mut()) {
            *s = s.mul_add(smoothing, *x * (1.0 - smoothing));
            *x = *s;
        }
        let max = bands
            .iter()
            .fold(bands[0], |m, &x| if m < x { x } else { m });
        // io.sdk's MovingAverage keeps a running sum, float drift included.
        self.sum = (self.sum - self.maxima[self.oldest]) + max;
        self.maxima[self.oldest] = max;
        self.oldest = (self.oldest + 1) % NORM_FRAMES;
        let mean = self.sum / NORM_FRAMES as f32;
        let norm = if NORM_FLOOR < mean { mean } else { NORM_FLOOR };
        for x in bands {
            *x /= norm;
        }
    }
}

fn draw(draw: &Draw, bands: &[f32]) -> Grid {
    let mut grid = [0.0; WIDTH * HEIGHT];
    match draw {
        Draw::Wave(wave) => draw_wave(wave, bands, &mut grid),
        Draw::Pads => draw_pads(bands, &mut grid),
        Draw::Bricks => draw_bricks(bands, &mut grid),
    }
    grid
}

fn draw_wave(wave: &Wave, bands: &[f32], grid: &mut Grid) {
    let sub = if wave.bars { 1 } else { 8 };
    let full = if wave.bottom { HEIGHT } else { HEIGHT / 2 };
    let n = bands.len();
    let mut heights = [0f32; MAX_BANDS];
    for (h, &v) in heights.iter_mut().zip(bands) {
        *h = (v * full as f32).clamp(wave.bar_width, full as f32) * sub as f32;
    }
    let p = wave.spline;
    let columns = WIDTH * sub;
    let divisor = if wave.bars { 1.0 } else { 64.0 };
    for col in (0..columns).step_by(if wave.bars { 2 } else { 1 }) {
        let pos = (col * (n - 1)) as f32 / columns as f32;
        let base = pos.floor() as isize;
        let first = base - (p / 2) as isize;
        let mut d = [0f32; MAX_SPLINE + 1];
        for (j, dj) in d[..=p].iter_mut().enumerate() {
            let i = first + j as isize;
            if (0..n as isize).contains(&i) {
                *dj = heights[i as usize];
            }
        }
        // io.sdk centres the control points on `base` but keeps de Boor's uncentred knots.
        for k in 1..=p {
            let span = (p + 1 - k) as f32;
            for m in (k..=p).rev() {
                let t = (pos - (base - p as isize + m as isize) as f32) / span;
                d[m] = (1.0 - t).mul_add(d[m - 1], d[m] * t);
            }
        }
        let h = d[p].round() as usize;
        let x = col / sub;
        for r in 0..h {
            let v = (1.0 - r as f32 / h as f32).powf(wave.fading) / divisor;
            let dy = r / sub;
            if wave.bottom {
                grid[dy * WIDTH + x] += v;
            } else {
                grid[(full + dy) * WIDTH + x] += v;
                if dy > 0 {
                    grid[(full - dy) * WIDTH + x] += v;
                }
            }
        }
    }
}

fn draw_pads(bands: &[f32], grid: &mut Grid) {
    for col in 0..PAD_COLS {
        for row in 0..PAD_ROWS {
            let v = bands[col * PAD_ROWS + row].clamp(0.0, 1.0).powf(PAD_GAMMA);
            let (cx, cy) = (2 + 4 * col, 2 + 4 * row);
            // The live preset's fading 0 turns io.sdk's distance falloff into a constant 1.
            for y in cy - 1..=cy + 1 {
                for x in cx - 1..=cx + 1 {
                    grid[y * WIDTH + x] += v;
                }
            }
        }
    }
}

fn draw_bricks(bands: &[f32], grid: &mut Grid) {
    for (i, &v) in bands.iter().enumerate() {
        let n = ((v.clamp(0.0, 1.0) * BRICK_LEVELS).round() as usize).max(1);
        for k in 0..n {
            let color = (1.0 - k as f32 / n as f32).powf(BRICK_FADING);
            let row = 2 * k * WIDTH;
            grid[row + 3 * i..row + 3 * i + 2].fill(color);
        }
    }
}

fn to_frame(grid: &Grid) -> Frame {
    let mut frame = [0; FRAME_BYTES];
    for (y, line) in grid.as_chunks::<WIDTH>().0.iter().enumerate() {
        let row = (HEIGHT - 1 - y) * WIDTH * 3;
        for (px, &v) in frame[row..row + WIDTH * 3]
            .as_chunks_mut::<3>()
            .0
            .iter_mut()
            .zip(line)
        {
            px.fill(gray(v));
        }
    }
    frame
}

fn gray(v: f32) -> u8 {
    ((v.clamp(0.0, 1.0) * 8.0).exp2().floor() - 1.0) as u8
}

#[derive(Clone, Copy, Debug, Default)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    fn add(self, o: Complex) -> Complex {
        Complex {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }

    fn sub(self, o: Complex) -> Complex {
        Complex {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }

    fn mul(self, o: Complex) -> Complex {
        Complex {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }

    fn unit(k: usize, n: usize) -> Complex {
        let a = -2.0 * PI * k as f64 / n as f64;
        Complex {
            re: a.cos() as f32,
            im: a.sin() as f32,
        }
    }
}

struct RealFft {
    bitrev: Vec<usize>,
    twiddle: Vec<Complex>,
    split: Vec<Complex>,
    z: Vec<Complex>,
}

impl RealFft {
    fn new() -> Self {
        let bits = HALF.trailing_zeros();
        RealFft {
            bitrev: (0..HALF)
                .map(|i| i.reverse_bits() >> (usize::BITS - bits))
                .collect(),
            twiddle: (0..HALF / 2).map(|k| Complex::unit(k, HALF)).collect(),
            split: (0..=HALF).map(|k| Complex::unit(k, NFFT)).collect(),
            z: vec![Complex::default(); HALF],
        }
    }

    fn run(&mut self, input: &[f32], out: &mut [Complex]) {
        for (i, &r) in self.bitrev.iter().enumerate() {
            self.z[r] = Complex {
                re: input[2 * i],
                im: input[2 * i + 1],
            };
        }
        let mut len = 2;
        while len <= HALF {
            let step = HALF / len;
            for block in self.z.chunks_exact_mut(len) {
                let (lo, hi) = block.split_at_mut(len / 2);
                for (k, (a, b)) in lo.iter_mut().zip(hi.iter_mut()).enumerate() {
                    let t = b.mul(self.twiddle[k * step]);
                    (*a, *b) = (a.add(t), a.sub(t));
                }
            }
            len *= 2;
        }
        for (k, x) in out.iter_mut().enumerate() {
            let zk = self.z[k % HALF];
            let zn = self.z[(HALF - k) % HALF];
            let zn = Complex {
                re: zn.re,
                im: -zn.im,
            };
            let even = zk.add(zn);
            let diff = zk.sub(zn);
            let odd = Complex {
                re: diff.im,
                im: -diff.re,
            };
            let x2 = even.add(self.split[k].mul(odd));
            *x = Complex {
                re: x2.re / 2.0,
                im: x2.im / 2.0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const CD_RATE: u32 = 44_100;

    fn tone_at(rate: u32, hz: f64, amplitude: f64, frames: usize) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let s = (amplitude * 32767.0 * (2.0 * PI * hz * i as f64 / f64::from(rate)).sin())
                    .round() as i16;
                [s, s]
            })
            .collect()
    }

    fn tone(hz: f64, amplitude: f64, frames: usize) -> Vec<i16> {
        tone_at(RATE, hz, amplitude, frames)
    }

    fn spectra(a: &mut Analyzer, stereo: &[i16]) -> Vec<(u64, Spectrum)> {
        let mut out = Vec::new();
        a.push(stereo, &mut out);
        out
    }

    fn last_db(rate: u32, stereo: &[i16]) -> Box<[f32]> {
        spectra(&mut Analyzer::new(rate), stereo)
            .pop()
            .unwrap()
            .1
            .db
    }

    struct Live {
        analyzer: Analyzer,
        spectrogram: Spectrogram,
    }

    impl Live {
        fn new(style: Style, rate: u32) -> Self {
            Live {
                analyzer: Analyzer::new(rate),
                spectrogram: Spectrogram::new(style),
            }
        }

        fn push(&mut self, stereo: &[i16], frames: &mut Vec<Frame>) {
            for (_, s) in spectra(&mut self.analyzer, stereo) {
                frames.push(self.spectrogram.render(Some(&s)));
            }
        }
    }

    fn px(frame: &Frame, x: usize, row: usize) -> u8 {
        let i = (row * WIDTH + x) * 3;
        assert!(frame[i] == frame[i + 1] && frame[i] == frame[i + 2]);
        frame[i]
    }

    fn lit(frame: &Frame) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for row in 0..HEIGHT {
            for x in 0..WIDTH {
                if px(frame, x, row) > 0 {
                    out.push((x, row));
                }
            }
        }
        out
    }

    fn render(style: Style, value: f32) -> Frame {
        let preset = style.preset();
        to_frame(&draw(&preset.draw, &vec![value; preset.bands]))
    }

    fn band_edges_hz(style: Style) -> Vec<(f64, f64)> {
        let p = style.preset();
        let ratio = f64::from(p.fmax / p.fmin).powf(1.0 / p.bands as f64);
        (0..p.bands)
            .map(|i| {
                let lo = f64::from(p.fmin) * ratio.powi(i as i32);
                (lo, lo * ratio)
            })
            .collect()
    }

    fn band_edges_bins(style: Style, rate: u32) -> Vec<(f64, f64)> {
        let bin_hz = f64::from(rate) / NFFT as f64;
        band_edges_hz(style)
            .into_iter()
            .map(|(lo, hi)| (lo / bin_hz, hi / bin_hz))
            .collect()
    }

    #[test]
    fn fft_matches_naive_dft() {
        let mut seed = 12345u32;
        let input: Vec<f32> = (0..NFFT)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 8) as f32 / (1 << 23) as f32 - 1.0
            })
            .collect();
        let mut out = vec![Complex::default(); BINS];
        RealFft::new().run(&input, &mut out);
        for (k, got) in out.iter().enumerate() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (n, &x) in input.iter().enumerate() {
                let a = -2.0 * PI * (k * n % NFFT) as f64 / NFFT as f64;
                re += f64::from(x) * a.cos();
                im += f64::from(x) * a.sin();
            }
            assert!(
                (f64::from(got.re) - re).abs() < 2e-3,
                "bin {k}: {} vs {re}",
                got.re
            );
            assert!(
                (f64::from(got.im) - im).abs() < 2e-3,
                "bin {k}: {} vs {im}",
                got.im
            );
        }
    }

    #[test]
    fn spectrum_scale_and_floor() {
        let db = last_db(RATE, &tone(3000.0, 0.5, NFFT));
        let expected = 20.0 * (0.5 * 0.53836f32 / 2.0).log10();
        assert!((db[64] - expected).abs() < 0.01, "{} vs {expected}", db[64]);
        assert!(db[300] < -70.0);
        assert!(
            last_db(RATE, &vec![0; 2 * NFFT])
                .iter()
                .all(|&d| d == -80.0)
        );
    }

    #[test]
    fn mono_is_the_channel_average() {
        let stereo = tone(1000.0, 0.5, NFFT);
        let left: Vec<i16> = stereo.chunks(2).flat_map(|f| [f[0], 0]).collect();
        let diff = last_db(RATE, &stereo)[21] - last_db(RATE, &left)[21];
        assert!((diff - 20.0 * 2f32.log10()).abs() < 0.01, "{diff}");
    }

    #[test]
    fn bins_follow_the_stream_rate() {
        for (rate, bin) in [(RATE, 64), (CD_RATE, 70)] {
            let db = last_db(rate, &tone_at(rate, 3000.0, 0.5, NFFT));
            let peak = (0..BINS).max_by(|&i, &j| db[i].total_cmp(&db[j])).unwrap();
            assert_eq!(peak, bin, "{rate}");
        }
    }

    #[test]
    fn a_spectrum_per_40_ms_of_the_stream_from_its_first_1024_frames() {
        for (rate, interval) in [(RATE, 1920), (CD_RATE, 1764)] {
            let mut a = Analyzer::new(rate);
            assert!(spectra(&mut a, &vec![0; 2 * (NFFT - 1)]).is_empty());
            assert_eq!(spectra(&mut a, &[0, 0]).len(), 1);
            assert!(spectra(&mut a, &vec![0; 2 * (interval - 1)]).is_empty());
            assert_eq!(spectra(&mut a, &[0, 0])[0].0, interval as u64);
            assert_eq!(a.position(), (interval + NFFT) as u64);

            let mut a = Analyzer::new(rate);
            let mut audio = tone_at(rate, 1000.0, 0.5, NFFT);
            audio.extend(vec![0; 2 * (interval - NFFT)]);
            audio.extend(tone_at(rate, 1000.0, 0.1, NFFT));
            let got = spectra(&mut a, &audio);
            assert_eq!(got.len(), 2);
            let loud = 20.0 * (0.5 * 0.53836f32 / 2.0).log10();
            let peak = |s: &Spectrum| s.db.iter().copied().fold(f32::MIN, f32::max);
            assert!((peak(&got[0].1) - loud).abs() < 1.0, "{rate}");
            assert!((peak(&got[1].1) - (loud - 20.0 * 5f32.log10())).abs() < 1.0);
        }
    }

    #[test]
    fn several_ffts_in_an_interval_are_averaged_in_db() {
        let rate = 96_000;
        let mut audio = Vec::new();
        for amplitude in [0.5, 0.25, 0.05] {
            audio.extend(tone_at(rate, 3000.0, amplitude, NFFT));
        }
        let mut a = Analyzer::new(rate);
        assert!(spectra(&mut a, &audio[..2 * (3 * NFFT - 1)]).is_empty());
        let got = spectra(&mut a, &audio[2 * (3 * NFFT - 1)..]);
        assert_eq!(got.len(), 1);
        let bin = 32;
        let mean = [0.5f32, 0.25, 0.05]
            .iter()
            .map(|a| 20.0 * (a * 0.53836 / 2.0).log10())
            .sum::<f32>()
            / 3.0;
        assert!(
            (got[0].1.db[bin] - mean).abs() < 0.05,
            "{}",
            got[0].1.db[bin]
        );
        assert!(spectra(&mut a, &vec![0; 2 * (3840 - 3 * NFFT)]).is_empty());
    }

    #[test]
    fn an_interval_shorter_than_the_fft_reuses_the_frames_before_it() {
        let rate = 22_050;
        let mut a = Analyzer::new(rate);
        let got = spectra(&mut a, &tone_at(rate, 1000.0, 0.5, 3 * 882));
        let starts: Vec<_> = got.iter().map(|&(p, _)| p).collect();
        assert_eq!(starts, [0, 882, 1764]);
        let audio = tone_at(rate, 1000.0, 0.5, 3 * 882);
        let mut padded = vec![0; 2 * (NFFT - 882)];
        padded.extend(&audio[..2 * 882]);
        assert_eq!(got[0].1.db, last_db(RATE, &padded));
        assert_eq!(
            got[1].1.db,
            last_db(RATE, &audio[2 * (2 * 882 - NFFT)..2 * 2 * 882])
        );
    }

    #[test]
    fn the_rounding_error_of_an_interval_adds_a_frame_as_gst_does() {
        let mut a = Analyzer::new(44_110);
        let got = spectra(&mut a, &vec![0; 2 * (12_350 + NFFT)]);
        let starts: Vec<_> = got.iter().map(|&(p, _)| p).collect();
        assert_eq!(starts, [0, 1764, 3528, 5292, 7056, 8821, 10_585, 12_350]);
    }

    #[test]
    fn tone_lands_in_its_band() {
        for rate in [CD_RATE, RATE] {
            let bin_hz = f64::from(rate) / NFFT as f64;
            for style in STYLES {
                let wide: Vec<_> = band_edges_hz(style)
                    .into_iter()
                    .enumerate()
                    .filter(|(_, (lo, hi))| hi - lo >= 5.0 * bin_hz)
                    .collect();
                assert!(!wide.is_empty(), "{style:?}");
                for (band, (lo, hi)) in wide {
                    let hz = (lo * hi).sqrt();
                    let db = last_db(rate, &tone_at(rate, hz, 0.3, NFFT));
                    let mut bands = Vec::new();
                    to_bands(&db, rate, &style.preset(), &mut bands);
                    let loudest = (0..bands.len())
                        .max_by(|&i, &j| bands[i].total_cmp(&bands[j]))
                        .unwrap();
                    assert_eq!(loudest, band, "{rate} {style:?} {hz} Hz");
                }
            }
        }
    }

    /// io.sdk's band value is the integral of the bin powers as a step function over the band,
    /// except that the part past the last whole bin is dropped once that bin is >= the band count.
    fn integral(power: &[f64], lo: f64, hi: f64, bands: usize) -> f64 {
        let (first, last) = (lo.ceil(), hi.floor());
        let hi = if last >= first && last as usize >= bands {
            last
        } else {
            hi
        };
        (lo.floor() as usize..=hi.floor() as usize)
            .map(|b| {
                let overlap = (hi.min(b as f64 + 1.0) - lo.max(b as f64)).max(0.0);
                power[b] * overlap
            })
            .sum()
    }

    #[test]
    fn bands_integrate_the_bin_powers_with_fractional_edges() {
        let mut seed = 99u32;
        let db: Vec<f32> = (0..BINS)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                -80.0 + 80.0 * (seed >> 8) as f32 / (1 << 24) as f32
            })
            .collect();
        let power: Vec<f64> = db
            .iter()
            .map(|&d| f64::from(10f32.powf(d / 10.0)))
            .collect();
        for rate in [CD_RATE, RATE] {
            for style in STYLES {
                let preset = style.preset();
                let mut bands = Vec::new();
                to_bands(&db, rate, &preset, &mut bands);
                for (i, (lo, hi)) in band_edges_bins(style, rate).into_iter().enumerate() {
                    // f32 and f64 edges may round to different bins exactly at a bin boundary.
                    if (hi - hi.round()).abs() < 1e-3 {
                        continue;
                    }
                    let expected = integral(&power, lo, hi, preset.bands);
                    let got = f64::from(bands[i]);
                    assert!(
                        (got - expected).abs() <= 1e-4 * expected,
                        "{rate} {style:?} band {i} [{lo:.3}, {hi:.3}): {got} vs {expected}"
                    );
                }
            }
        }
    }

    fn single_bin(bin: usize) -> Vec<f32> {
        let mut db = vec![-80.0; BINS];
        db[bin] = 0.0;
        db
    }

    #[test]
    fn a_band_inside_one_bin_takes_its_share_of_that_bin() {
        let (band, (lo, hi)) = band_edges_bins(Style::BarsCenter, RATE)
            .into_iter()
            .enumerate()
            .find(|(_, (lo, hi))| lo.floor() == hi.floor())
            .unwrap();
        let mut bands = Vec::new();
        to_bands(
            &single_bin(hi as usize),
            RATE,
            &Style::BarsCenter.preset(),
            &mut bands,
        );
        let share = (hi - lo) as f32;
        assert!(
            (bands[band] - share).abs() < 1e-4 * share,
            "{}",
            bands[band]
        );
    }

    #[test]
    fn the_tail_bin_counts_only_below_the_band_count() {
        let style = Style::Flame;
        let n = style.preset().bands;
        let edges = band_edges_bins(style, RATE);
        let mut bands = Vec::new();
        let (band, (_, hi)) = edges
            .iter()
            .copied()
            .enumerate()
            .find(|(_, (lo, hi))| hi.floor() as usize >= n && lo.ceil() <= hi.floor())
            .unwrap();
        to_bands(&single_bin(hi as usize), RATE, &style.preset(), &mut bands);
        assert!(bands[band] < 1e-6, "tail dropped: {}", bands[band]);
        let (band, (_, hi)) = edges
            .iter()
            .copied()
            .enumerate()
            .find(|(_, (lo, hi))| (hi.floor() as usize) < n && lo.ceil() < hi.floor())
            .unwrap();
        to_bands(&single_bin(hi as usize), RATE, &style.preset(), &mut bands);
        let tail = (hi - hi.floor()) as f32;
        assert!(
            (bands[band] - tail).abs() < 1e-4,
            "{} vs {tail}",
            bands[band]
        );
    }

    #[test]
    fn the_head_bin_counts_from_the_band_edge() {
        let style = Style::Flame;
        let (band, (lo, _)) = band_edges_bins(style, RATE)
            .into_iter()
            .enumerate()
            .find(|(_, (lo, hi))| lo.ceil() < hi.floor())
            .unwrap();
        let mut bands = Vec::new();
        to_bands(&single_bin(lo as usize), RATE, &style.preset(), &mut bands);
        let head = (lo.ceil() - lo) as f32;
        assert!(
            (bands[band] - head).abs() < 1e-4,
            "{} vs {head}",
            bands[band]
        );
    }

    fn cardinal_bspline(p: usize, x: f64) -> f64 {
        if x <= 0.0 || x >= (p + 1) as f64 {
            return 0.0;
        }
        let factorial: f64 = (1..=p).map(|k| k as f64).product();
        let mut binomial = 1.0;
        let mut sum = 0.0;
        for k in 0..=p + 1 {
            let t = x - k as f64;
            if t > 0.0 {
                let sign = if k % 2 == 0 { 1.0 } else { -1.0 };
                sum += sign * binomial * t.powi(p as i32);
            }
            binomial = binomial * (p + 1 - k) as f64 / (k + 1) as f64;
        }
        sum / factorial
    }

    // io.sdk's live presets, stated apart from `Style::preset` so a changed order fails.
    fn stock_spline(style: Style) -> usize {
        match style {
            Style::Flame | Style::BarsBottom => 10,
            Style::WaveCenter | Style::BarsCenter => 5,
            _ => unreachable!(),
        }
    }

    #[test]
    fn wave_columns_follow_the_spline_through_the_bands_left_to_right() {
        for style in [
            Style::Flame,
            Style::WaveCenter,
            Style::BarsBottom,
            Style::BarsCenter,
        ] {
            let preset = style.preset();
            let Draw::Wave(wave) = preset.draw else {
                unreachable!()
            };
            let n = preset.bands;
            let bands: Vec<f32> = (0..n).map(|i| ((i * 7 + 3) % 11) as f32 / 10.0).collect();
            let (sub, full) = (
                if wave.bars { 1 } else { 8 },
                if wave.bottom { HEIGHT } else { HEIGHT / 2 },
            );
            let heights: Vec<f64> = bands
                .iter()
                .map(|&v| {
                    f64::from((v * full as f32).clamp(wave.bar_width, full as f32)) * sub as f64
                })
                .collect();
            let p = stock_spline(style);
            let columns = WIDTH * sub;
            let mut tallest = [(0usize, 0usize); WIDTH];
            for col in (0..columns).step_by(if wave.bars { 2 } else { 1 }) {
                let pos = (col * (n - 1)) as f64 / columns as f64;
                let base = pos.floor() as isize;
                let value: f64 = (0..=p)
                    .map(|j| {
                        let i = base - (p / 2) as isize + j as isize;
                        let h = usize::try_from(i).ok().and_then(|i| heights.get(i));
                        h.copied().unwrap_or(0.0)
                            * cardinal_bspline(p, pos - (base - p as isize + j as isize) as f64)
                    })
                    .sum();
                let t = &mut tallest[col / sub];
                t.0 = t.0.max((value - 0.01).round() as usize);
                t.1 = t.1.max((value + 0.01).round() as usize);
            }
            let grid = draw(&preset.draw, &bands);
            let offset = if wave.bottom { 0 } else { full };
            for (x, &(lo, hi)) in tallest.iter().enumerate() {
                let top = (0..HEIGHT).rev().find(|&y| grid[y * WIDTH + x] > 0.0);
                let row = |h: usize| (h > 0).then(|| offset + (h - 1) / sub);
                assert!(
                    top == row(lo) || top == row(hi),
                    "{style:?} x {x}: top {top:?}, spline {lo}..={hi}"
                );
            }
        }
    }

    #[test]
    fn bricks_follow_the_bands_left_to_right() {
        let bands: Vec<f32> = (0..8).map(|i| i as f32 / 7.0).collect();
        let frame = to_frame(&draw(&Draw::Bricks, &bands));
        for i in 0..8 {
            let bricks = i.max(1);
            for x in [3 * i, 3 * i + 1] {
                let rows: Vec<_> = (0..HEIGHT).filter(|&row| px(&frame, x, row) > 0).collect();
                let expected: Vec<_> = (0..bricks).rev().map(|k| HEIGHT - 1 - 2 * k).collect();
                assert_eq!(rows, expected, "band {i} x {x}");
            }
        }
    }

    #[test]
    fn tone_lights_its_pad() {
        let mut live = Live::new(Style::Pads, RATE);
        let mut frames = Vec::new();
        live.push(&tone(1000.0, 0.3, RATE as usize), &mut frames);
        let frame = frames.last().unwrap();
        let band = band_edges_hz(Style::Pads)
            .iter()
            .position(|&(lo, hi)| lo <= 1000.0 && 1000.0 < hi)
            .unwrap();
        let (x, y) = (2 + 4 * (band / PAD_ROWS), 2 + 4 * (band % PAD_ROWS));
        assert_eq!(px(frame, x, HEIGHT - 1 - y), 255);
        let brightest = lit(frame)
            .into_iter()
            .filter(|&(x, row)| px(frame, x, row) == 255)
            .collect::<Vec<_>>();
        let pad: Vec<_> = (HEIGHT - 2 - y..=HEIGHT - y)
            .flat_map(|row| (x - 1..=x + 1).map(move |x| (x, row)))
            .collect();
        assert_eq!(brightest, pad);
    }

    #[test]
    fn silence_decays_to_the_floor() {
        for style in STYLES {
            let mut live = Live::new(style, RATE);
            let mut frames = Vec::new();
            live.push(&tone(800.0, 0.5, RATE as usize), &mut frames);
            assert!(lit(frames.last().unwrap()).len() > 13, "{style:?}");
            live.push(&vec![0; 2 * 8 * RATE as usize], &mut frames);
            let frame = frames.last().unwrap();
            let baseline = render(style, 0.0);
            // -80 dB per bin is 1e-8, which the top bands of the wave styles still show as gray 1.
            let floor = matches!(style, Style::Flame | Style::WaveCenter);
            for (i, (&got, &base)) in frame.iter().zip(&baseline).enumerate() {
                let ok = got == base || (floor && base == 0 && got <= 1);
                assert!(ok, "{style:?} byte {i}: {got} vs {base}");
            }
        }
    }

    #[test]
    fn without_a_spectrum_the_picture_decays_to_the_baseline() {
        for style in STYLES {
            let mut live = Live::new(style, RATE);
            let mut frames = Vec::new();
            live.push(&tone(800.0, 0.5, RATE as usize / 2), &mut frames);
            let brightness = |f: &Frame| f.iter().map(|&b| u32::from(b)).sum::<u32>();
            let loud = brightness(frames.last().unwrap());
            let decayed: Vec<_> = (0..150).map(|_| live.spectrogram.render(None)).collect();
            assert!(brightness(&decayed[0]) <= loud, "{style:?}");
            assert_eq!(decayed.last().unwrap(), &render(style, 0.0), "{style:?}");
            assert_eq!(Spectrogram::new(style).render(None), render(style, 0.0));
        }
    }

    #[test]
    fn baselines() {
        for style in [Style::Flame, Style::WaveCenter, Style::Pads] {
            assert!(lit(&render(style, 0.0)).is_empty(), "{style:?}");
        }
        let bars_bottom = render(Style::BarsBottom, 0.0);
        for x in (4..=20).step_by(2) {
            assert_eq!(px(&bars_bottom, x, 15), 255);
        }
        assert!(
            lit(&bars_bottom)
                .iter()
                .all(|&(x, row)| row == 15 && x % 2 == 0)
        );
        let bars_center = render(Style::BarsCenter, 0.0);
        for x in (4..=20).step_by(2) {
            assert_eq!(px(&bars_center, x, 7), 255);
        }
        assert!(
            lit(&bars_center)
                .iter()
                .all(|&(x, row)| row == 7 && x % 2 == 0)
        );
        let bricks: Vec<_> = (0..8)
            .flat_map(|i| [(3 * i, 15), (3 * i + 1, 15)])
            .collect();
        let mut got = lit(&render(Style::BricksSmall, 0.0));
        got.sort_by_key(|&(x, _)| x);
        assert_eq!(got, bricks);
        assert!(
            bricks
                .iter()
                .all(|&(x, row)| px(&render(Style::BricksSmall, 0.0), x, row) == 255)
        );
    }

    #[test]
    fn wave_bottom_full_scale() {
        let frame = render(Style::Flame, 1.0);
        for x in 3..22 {
            let column: Vec<u8> = (0..HEIGHT).rev().map(|row| px(&frame, x, row)).collect();
            assert!(column.iter().all(|&g| g > 0), "x {x}: {column:?}");
            assert!(column.windows(2).all(|w| w[0] >= w[1]), "x {x}: {column:?}");
            assert!(column[0] >= 240 && column[15] < 16, "x {x}: {column:?}");
        }
    }

    #[test]
    fn wave_center_mirrors_and_skips_row_15() {
        for style in [Style::WaveCenter, Style::BarsCenter] {
            for value in [0.3, 0.7, 1.0] {
                let frame = render(style, value);
                for x in 0..WIDTH {
                    assert_eq!(px(&frame, x, 15), 0, "{style:?}");
                    for d in 1..8 {
                        assert_eq!(
                            px(&frame, x, 7 - d),
                            px(&frame, x, 7 + d),
                            "{style:?} x {x}"
                        );
                    }
                }
            }
            let full = render(style, 1.0);
            assert!((0..15).all(|row| px(&full, 12, row) > 0), "{style:?}");
        }
    }

    #[test]
    fn bars_sit_on_even_columns() {
        for style in [Style::BarsBottom, Style::BarsCenter] {
            let frame = render(style, 1.0);
            assert!(lit(&frame).iter().all(|&(x, _)| x % 2 == 0), "{style:?}");
            for x in (0..WIDTH).step_by(2) {
                assert!(px(&frame, x, if style == Style::BarsBottom { 15 } else { 7 }) > 0);
            }
        }
        let frame = render(Style::BarsBottom, 1.0);
        for x in (4..=20).step_by(2) {
            assert!((0..HEIGHT).all(|row| px(&frame, x, row) > 0), "x {x}");
        }
        let half = render(Style::BarsBottom, 0.5);
        for x in (6..=18).step_by(2) {
            let rows: Vec<_> = (0..HEIGHT).filter(|&row| px(&half, x, row) > 0).collect();
            assert_eq!(rows, (8..16).collect::<Vec<_>>(), "x {x}");
        }
    }

    #[test]
    fn pads_cover_the_documented_squares() {
        let frame = render(Style::Pads, 1.0);
        let pad_coord = |c: usize| !c.is_multiple_of(4);
        for row in 0..HEIGHT {
            for x in 0..WIDTH {
                let y = HEIGHT - 1 - row;
                let expected = if x < 24 && pad_coord(x) && pad_coord(y) {
                    255
                } else {
                    0
                };
                assert_eq!(px(&frame, x, row), expected, "x {x} y {y}");
            }
        }

        let preset = Style::Pads.preset();
        let mut bands = vec![0.0; preset.bands];
        bands[2 * PAD_ROWS + 1] = 0.25;
        let frame = to_frame(&draw(&preset.draw, &bands));
        let mut expected = Vec::new();
        for row in 8..=10 {
            for x in 9..=11 {
                expected.push((x, row));
            }
        }
        assert_eq!(lit(&frame), expected);
        assert_eq!(px(&frame, 10, 9), gray(0.5));
    }

    #[test]
    fn bricks_stack_every_other_row() {
        let frame = render(Style::BricksSmall, 1.0);
        let mut expected = Vec::new();
        for k in (0..7).rev() {
            for i in 0..8 {
                expected.push((3 * i, 15 - 2 * k));
                expected.push((3 * i + 1, 15 - 2 * k));
            }
        }
        expected.sort_by_key(|&(x, row)| (row, x));
        assert_eq!(lit(&frame), expected);
        for k in 0..7 {
            let g = gray((1.0 - k as f32 / 7.0).powf(BRICK_FADING));
            assert_eq!(px(&frame, 9, 15 - 2 * k), g);
        }
        assert!(px(&frame, 0, 15) > px(&frame, 0, 3));
    }

    #[test]
    fn gray_mapping() {
        assert_eq!(gray(-1.0), 0);
        assert_eq!(gray(0.0), 0);
        assert_eq!(gray(0.124), 0);
        assert_eq!(gray(0.125), 1);
        assert_eq!(gray(0.5), 15);
        assert_eq!(gray(1.0), 255);
        assert_eq!(gray(3.0), 255);
    }

    #[test]
    fn normalization_boosts_after_reset() {
        let mut f = Filters::new(3);
        let max = |f: &mut Filters| {
            let mut b = [1.0, 4.0, 2.0];
            f.apply(0.8, &mut b);
            b[1]
        };
        assert!((max(&mut f) - 10.0).abs() < 1e-4);
        assert!((max(&mut f) - 0.36 / 0.056).abs() < 1e-3);
        for _ in 0..200 {
            max(&mut f);
        }
        assert!((max(&mut f) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn reset_restarts_interval_and_filters() {
        let audio = tone(600.0, 0.4, RATE as usize);
        let mut fresh = Vec::new();
        Live::new(Style::Pads, RATE).push(&audio, &mut fresh);

        let mut live = Live::new(Style::Pads, RATE);
        let mut again = Vec::new();
        live.push(&tone(2000.0, 0.4, 3000), &mut again);
        live.analyzer.reset();
        live.spectrogram.reset();
        assert_eq!(live.analyzer.position(), 0);
        again.clear();
        live.push(&audio, &mut again);
        assert_eq!(fresh, again);
        let brightness = |f: &Frame| f.iter().map(|&b| u32::from(b)).sum::<u32>();
        assert!(brightness(&fresh[0]) > brightness(fresh.last().unwrap()));
    }

    #[test]
    fn chunking_does_not_change_the_frames() {
        for rate in [CD_RATE, RATE] {
            let half = rate as usize / 2;
            let mut audio = tone_at(rate, 440.0, 0.3, half);
            audio.extend(tone_at(rate, 1500.0, 0.6, half));
            let mut reference = Vec::new();
            Live::new(Style::BarsCenter, rate).push(&audio, &mut reference);
            assert_eq!(reference.len(), 25);
            for chunk in [2, 14, 960, 3842] {
                let mut live = Live::new(Style::BarsCenter, rate);
                let mut frames = Vec::new();
                for part in audio.chunks(chunk) {
                    live.push(part, &mut frames);
                }
                assert!(frames == reference, "{rate} chunk {chunk}");
            }
        }
    }

    #[test]
    fn styles_follow_the_random_order() {
        let names: Vec<_> = STYLES.iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            [
                "flame",
                "waveCenter",
                "pads",
                "barsBottom",
                "barsCenter",
                "bricksSmall"
            ]
        );
        for (i, style) in STYLES.into_iter().enumerate() {
            assert_eq!(style as usize, i);
            assert_eq!(Style::from_name(style.name()), Some(style));
        }
        assert_eq!(Style::from_name("showClock"), None);
    }

    #[test]
    fn pick_random_never_repeats_and_covers_all() {
        let all: std::collections::HashSet<_> = (0..6).map(|r| pick_random(None, r)).collect();
        assert_eq!(all.len(), 6);
        for current in STYLES {
            let picks: Vec<_> = (0..1000u64)
                .map(|r| pick_random(Some(current), r.wrapping_mul(0x9e37_79b9_7f4a_7c15)))
                .collect();
            assert!(picks.iter().all(|&s| s != current));
            let distinct: std::collections::HashSet<_> = picks.iter().collect();
            assert_eq!(distinct.len(), 5, "{current:?}");
            for other in STYLES.into_iter().filter(|&s| s != current) {
                let n = picks.iter().filter(|&&s| s == other).count();
                assert!((150..250).contains(&n), "{current:?} -> {other:?}: {n}");
            }
        }
    }

    #[test]
    fn set_style_switches_layout() {
        let mut live = Live::new(Style::Flame, RATE);
        live.spectrogram.set_style(Style::BricksSmall);
        assert_eq!(live.spectrogram.style(), Style::BricksSmall);
        let mut frames = Vec::new();
        live.push(&tone(1000.0, 0.5, RATE as usize), &mut frames);
        let frame = frames.last().unwrap();
        assert!(
            lit(frame)
                .iter()
                .all(|&(x, row)| x % 3 != 2 && x < 23 && row % 2 == 1)
        );
    }
}
