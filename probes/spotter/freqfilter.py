"""Reference model of Yandex spotter "freq filter" (libquasar_daemons.so, Station Max, arm64).

Addresses are virtual addresses in libquasar_daemons.so (.text/.rodata vaddr == file offset).

Pipeline per freq-filter frame (raw-mic int16 PCM, 16 kHz):
  frame   = samples[n*2832 : n*2832 + 4032]                      framer 0x391f54c
  x       = float(frame) + 0.1 * DITHER[k++ % 2048]               0x37b02c4 -> 0x37b4ec4
  x      -= mean(x)                                               0x37b0308..0x37b0410
  x[j]   -= 0.72 * x[j-1] (j = N-1..1), x[0] *= (1 - 0.72)        0x37b0498..0x37b0574
  x      *= povey window (0.5 - 0.5 cos(2 pi i/(N-1)))^0.85       ctor 0x37afe48..0x37aff5c
  P       = |rfft(x, 4096)|^2, 2049 bins (pffft, unscaled)        0x37b0150..0x37b023c
  for each f_i of the 8 frequencies:                              0x37ae530
    in_i  = mean(P[c-hi .. c+hi])            c  = bin(f_i)
    lo_i  = median(P[l-ho .. l+ho])          l  = bin(0.68 f_i)
    up_i  = median(P[u-ho .. u+ho])          u  = bin(1.45 f_i)
    feat_i = ln(in_i) - ln(min(lo_i, up_i))
  frame_score = sum_i feat_i                                     0x37aeca0..0x37aee00
On a spotter hit at 10 ms frame t:                                0x37ae9a4
  pick the newest freq frame whose end sample <= 160*t, plus 4 older frames,
  confidence = min over those 5 frames of frame_score; vetoed if confidence <= -3.6.
Sign: feat_i > 0 means a peak at f_i, < 0 a notch. The veto fires on notches.

Constants (flags.txt value / binary default, evidence):
  sample-rate 16000          cfg+0x00 int, parsed 0x37ad19c/0x37ad1e4, default 8000 at 0x37aceb4
  frame-size 252 ms          cfg+0x04 float, 0x37ad30c/0x37ad350, default 512 (0x37acea0)
  frame-shift 177 ms         cfg+0x08 float, 0x37ad390/0x37ad3e0, default 128 (0x37acea4)
    ms -> samples: uint(rate*ms/1000.0) at 0x37afa3c (size) / 0x37afa64 (shift): 4032 / 2832
  remove-dc-offset true      cfg+0x0c, 0x37ad21c/0x37ad25c, default 1 (0x37afca8 w2=1)
  dither 0.1                 cfg+0x10, generic "dither" key 0x37ad290..0x37ad2d4, default 0.1 (0x37acebc)
    noise = 2048-entry float table at .rodata 0x6ea494 (GOT 0x48d4de8) ==
    np.random.RandomState(42).randn(2048).astype(float32); index persists across frames (0x37b4ec4)
  freq-filter-preemph-coeff 0.72  cfg+0x14, 0x37ad420/0x37ad470, range check 0..1 at 0x37adb60
  freq-filter-outside-smooth 99   cfg+0x18 int, 0x37ad4b0/0x37ad500, default 10 (0x275198)
  freq-filter-inside-smooth 3     cfg+0x1c int, 0x37ad544/0x37ad588, default 2 (0x275198)
  freq-filter-lower-freq-ratio 0.68   cfg+0x20, 0x37ad5c8/0x37ad618, default 0.9 (0x274218)
  freq-filter-higher-freq-ratio 1.45  cfg+0x24, 0x37ad658/0x37ad6a4, default 1.1 (0x274218)
  freq-filter-frequencies-hz      cfg+0x28 vector<float>, count cfg+0x40, 0x37ad924..0x37ada80
    (each must be >= 10 Hz and < rate/2)
  freq-filter-threshold -3.6      cfg+0x44, 0x37ad8ac/0x37ad8f4 (mandatory)
  freq-filter-buffer-size 3000 ms cfg+0x48 (absent in flags, default 3000.0 at 0x37acefc)
    ring = max(1, int((3000 - 252 + 177)/177)) = 16 frames (0x37adfa4..0x37adfcc)
  freq-filter-react-as-subhits    cfg+0x4c, default true (0x37acf14); veto turns hit into
    subhit (0x37b536c, negates status) or zeroes it (0x37b539c)
  freq-filter-aggregation-func-inside mean     cfg+0x68, 0x37ad87c; "mean" -> 0x37ace08
  freq-filter-aggregation-func-outside median  cfg+0x70, 0x37ad894; "median" -> 0x37acce0
    (median: nth_element, even n -> mean of the two middle values)
  freq-filter-use-dyn-freq-smooth true  cfg+0x78 = (logf(fmax)-logf(fmin))/(fmax-fmin)
    0x37adb7c..0x37adbe8; then half-widths per frequency:
    h = int(roundf_away(float(smooth) * f * cfg+0x78)) (frinta, 0x37ae564..0x37ae588)
  freq-filter-left-context 979 ms cfg+0x7c, 0x37ad7f8/0x37ad844
    frames = int(max((979 - 252)/177, 0)) = 4 (0x37aef78..0x37aef90)
  freq-filter-reduce-features-first (absent) cfg+0x80 = !value, default 0 -> sum over
    frequencies first, then min over frames (0x37adbf4..0x37adc38, 0x37aeb20/0x37aeca0)
  FFT size = smallest power of two >= 4032 = 4096 (0x37afd00..0x37afd14), pffft setup 0x37b07e8
  bin(f) = int(float(4096) * f / float(16000)), truncation (0x37b0628)
  band index range: [max(c-h, 0), min(c+h+1, 2049)) (0x37ae5bc..0x37ae5ec)
  feature = logf(in) - logf(min(lower, upper)) (0x37ae6b4..0x37ae6d4)
  hit sample = hit_frame * 160 (spotter frame-shift samples, 0x3976204..0x3976228, 0x37ae9cc..0x37aea08)
  freq frame end sample = 4032 + n*2832 (ring index store 0x37ae4a8..0x37ae4d4)
"""

import numpy as np

F32 = np.float32

SAMPLE_RATE = 16000
FRAME_SIZE_MS = 252.0
FRAME_SHIFT_MS = 177.0
DITHER = 0.1
REMOVE_DC = True
PREEMPH = 0.72
OUTSIDE_SMOOTH = 99
INSIDE_SMOOTH = 3
LOWER_RATIO = 0.68
HIGHER_RATIO = 1.45
FREQS_HZ = (3501, 2089, 814, 1355, 638, 4510, 2635, 486)
THRESHOLD = -3.6
BUFFER_SIZE_MS = 3000.0
LEFT_CONTEXT_MS = 979.0
USE_DYN_FREQ_SMOOTH = True
SPOTTER_FRAME_SHIFT_SAMPLES = 160


def _ms_to_samples(rate, ms):
    return int(float(F32(rate) * F32(ms)) / 1000.0)


def _round_half_away(x):
    return int(np.sign(x) * np.floor(abs(float(x)) + 0.5))


class FreqFilter:
    def __init__(self, freqs_hz=FREQS_HZ, rate=SAMPLE_RATE):
        self.rate = rate
        self.freqs = np.asarray(freqs_hz, dtype=F32)
        self.frame_len = _ms_to_samples(rate, FRAME_SIZE_MS)
        self.frame_shift = _ms_to_samples(rate, FRAME_SHIFT_MS)
        self.fft_size = 2
        while self.fft_size < self.frame_len:
            self.fft_size *= 2
        self.nbins = self.fft_size // 2 + 1
        i = np.arange(self.frame_len, dtype=np.float64)
        ang = (i * (2 * np.pi) / (self.frame_len - 1)).astype(F32)
        self.window = np.power(F32(0.5) - F32(0.5) * np.cos(ang), F32(0.85)).astype(F32)
        self.dither_table = np.random.RandomState(42).randn(2048).astype(F32)
        self.dither_pos = 0
        dyn = F32(0.0)
        if USE_DYN_FREQ_SMOOTH:
            fmax, fmin = self.freqs.max(), self.freqs.min()
            dyn = F32((np.log(fmax) - np.log(fmin)) / (fmax - fmin))
        self.bands = []
        for f in self.freqs:
            if dyn != 0:
                h_out = _round_half_away(F32(F32(OUTSIDE_SMOOTH) * f) * dyn)
                h_in = _round_half_away(F32(F32(INSIDE_SMOOTH) * f) * dyn)
            else:
                h_out, h_in = OUTSIDE_SMOOTH, INSIDE_SMOOTH
            self.bands.append((
                self._range(self._bin(F32(F32(LOWER_RATIO) * f)), h_out),
                self._range(self._bin(f), h_in),
                self._range(self._bin(F32(F32(HIGHER_RATIO) * f)), h_out),
            ))
        self.ring_frames = max(1, int((F32(BUFFER_SIZE_MS) - F32(FRAME_SIZE_MS) + F32(FRAME_SHIFT_MS))
                                      / F32(FRAME_SHIFT_MS)))
        self.left_context = int(max((F32(LEFT_CONTEXT_MS) - F32(FRAME_SIZE_MS)) / F32(FRAME_SHIFT_MS), 0.0))

    def _bin(self, f):
        return int(F32(F32(self.fft_size) * F32(f)) / F32(self.rate))

    def _range(self, center, half):
        return max(center - half, 0), min(center + half + 1, self.nbins)

    def _dither(self, n):
        idx = (self.dither_pos + np.arange(n)) % len(self.dither_table)
        self.dither_pos = int((self.dither_pos + n) % len(self.dither_table))
        return self.dither_table[idx] * F32(DITHER)

    def power_spectrum(self, frame_int16):
        x = np.asarray(frame_int16, dtype=np.int16).astype(F32)
        assert x.size == self.frame_len
        if DITHER != 0:
            x = x + self._dither(x.size)
        if REMOVE_DC:
            x = x - F32(x.mean(dtype=F32))
        if PREEMPH != 0:
            y = x.copy()
            y[1:] = x[1:] - F32(PREEMPH) * x[:-1]
            y[0] = x[0] - F32(PREEMPH) * x[0]
            x = y
        x = x * self.window
        spec = np.fft.rfft(x, self.fft_size)
        return (spec.real ** 2 + spec.imag ** 2).astype(F32)

    def frame_features(self, frame_int16):
        p = self.power_spectrum(frame_int16)
        feats = np.empty(len(self.freqs), dtype=F32)
        for k, ((l0, l1), (c0, c1), (u0, u1)) in enumerate(self.bands):
            inside = F32(np.mean(p[c0:c1], dtype=F32))
            outside = min(F32(np.median(p[l0:l1])), F32(np.median(p[u0:u1])))
            feats[k] = np.log(inside) - np.log(outside)
        return feats

    def frame_score(self, frame_int16):
        return float(self.frame_features(frame_int16).sum(dtype=F32))

    def process(self, pcm_int16):
        """Returns (features[n_frames, n_freqs], end_sample[n_frames]) for a whole stream."""
        pcm = np.asarray(pcm_int16, dtype=np.int16)
        feats, ends = [], []
        start = 0
        while start + self.frame_len <= pcm.size:
            feats.append(self.frame_features(pcm[start:start + self.frame_len]))
            ends.append(start + self.frame_len)
            start += self.frame_shift
        return np.array(feats, dtype=F32).reshape(-1, len(self.freqs)), np.array(ends, dtype=np.int64)

    def hit_confidence(self, feats, ends, hit_frame_10ms):
        """Score compared with THRESHOLD for a hit at spotter frame hit_frame_10ms (None if no frame)."""
        hit_sample = hit_frame_10ms * SPOTTER_FRAME_SHIFT_SAMPLES
        idx = np.nonzero(ends <= hit_sample)[0]
        if idx.size == 0:
            return None
        last = int(idx[-1])
        first = max(0, last - self.left_context)
        return float(feats[first:last + 1].sum(axis=1, dtype=F32).min())


def _shape_noise(noise, freqs, rate, gain, rel_width=0.012, min_width_hz=12.0):
    spec = np.fft.rfft(noise)
    hz = np.fft.rfftfreq(noise.size, 1.0 / rate)
    for f in freqs:
        spec[np.abs(hz - f) <= max(rel_width * f, min_width_hz)] *= gain
    return np.fft.irfft(spec, noise.size)


def _self_test():
    rng = np.random.default_rng(0)
    seconds = 3.0
    noise = rng.standard_normal(int(SAMPLE_RATE * seconds)) * 2000.0
    cases = {
        "white noise": noise,
        "8 freqs notched (-40 dB)": _shape_noise(noise, FREQS_HZ, SAMPLE_RATE, 0.01),
        "8 freqs notched (-6 dB)": _shape_noise(noise, FREQS_HZ, SAMPLE_RATE, 0.5),
        "8 freqs boosted (+12 dB)": _shape_noise(noise, FREQS_HZ, SAMPLE_RATE, 4.0),
    }
    ff = FreqFilter()
    print("frame_len=%d shift=%d fft=%d ring=%d left_context=%d" %
          (ff.frame_len, ff.frame_shift, ff.fft_size, ff.ring_frames, ff.left_context))
    for f, (lo, c, up) in zip(ff.freqs, ff.bands):
        print("  f=%6.0f inside bins %s lower %s upper %s" % (f, c, lo, up))
    for name, sig in cases.items():
        ff = FreqFilter()
        pcm = np.clip(np.round(sig), -32768, 32767).astype(np.int16)
        feats, ends = ff.process(pcm)
        scores = feats.sum(axis=1)
        hit = int(ends[-1] // SPOTTER_FRAME_SHIFT_SAMPLES)
        conf = ff.hit_confidence(feats, ends, hit)
        print("%-26s per-frame score mean %+7.2f [%+7.2f..%+7.2f]  hit conf %+7.2f  vetoed=%s" %
              (name, scores.mean(), scores.min(), scores.max(), conf, conf <= THRESHOLD))
        print("%-26s per-freq mean %s" % ("", np.array2string(feats.mean(axis=0), precision=2)))


if __name__ == "__main__":
    _self_test()
