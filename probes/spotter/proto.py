"""Prototype of the Yandex Alisa spotter pipeline as reverse-engineered from libquasar_daemons.so.

usage: proto.py <capture.raw 48k s16 16ch> <model.tflite> [omni_ch bnr_ch] [probs.npy]
probs.npy receives the keyword posteriors after the look-ahead drop (input of chen14_ref.py).
"""
import sys
import numpy as np
from ai_edge_litert.interpreter import Interpreter

RATE = 16000
FRAME, SHIFT, NFFT, BINS = 400, 160, 512, 40


def decimate3(x):
    n = 141
    t = np.arange(n) - (n - 1) / 2
    fc = 7250 / 48000
    h = 2 * fc * np.sinc(2 * fc * t) * np.kaiser(n, 7.0)
    h /= h.sum()
    y = np.convolve(x.astype(np.float64), h)[n // 2:n // 2 + len(x)]
    return np.clip(np.round(y[::3]), -32768, 32767).astype(np.int16)


def mel_banks():
    mel = lambda f: 1127.0 * np.log(1 + f / 700.0)
    lo, hi = mel(20.0), mel(8000.0)
    delta = (hi - lo) / (BINS + 1)
    fft_mel = mel(np.arange(NFFT // 2 + 1) * RATE / NFFT)
    banks = np.zeros((BINS, NFFT // 2 + 1), dtype=np.float32)
    for b in range(BINS):
        left, center, right = lo + b * delta, lo + (b + 1) * delta, lo + (b + 2) * delta
        up = (fft_mel - left) / (center - left)
        down = (right - fft_mel) / (right - center)
        banks[b] = np.maximum(0, np.where(fft_mel <= center, up, down)) * ((fft_mel > left) & (fft_mel < right))
    return banks


DITHER = np.random.RandomState(42).randn(2048).astype(np.float32)
WINDOW = np.power(0.5 - 0.5 * np.cos(2 * np.pi * np.arange(FRAME) / (FRAME - 1)).astype(np.float32), 0.85).astype(np.float32)
BANKS = mel_banks()


class Frontend:
    def __init__(self):
        self.idx = 0

    def frame(self, x):
        x = x.astype(np.float32).copy()
        d = np.empty(FRAME, dtype=np.float32)
        for i in range(FRAME):
            d[i] = DITHER[self.idx]
            self.idx = (self.idx + 1) % 2048
        x += np.float32(0.1) * d
        x -= x.mean()
        y = x.copy()
        y[1:] = x[1:] - 0.97 * x[:-1]
        y[0] = x[0] - 0.97 * x[0]
        y *= WINDOW
        spec = np.fft.rfft(y, NFFT)
        power = (spec.real ** 2 + spec.imag ** 2).astype(np.float32)
        return np.log(BANKS @ power)


def main():
    raw = np.fromfile(sys.argv[1], dtype="<i2").reshape(-1, 16)
    extra = sys.argv[3:]
    save = next((a for a in extra if a.endswith(".npy")), None)
    chans = [int(a) for a in extra if not a.endswith(".npy")]
    omni_ch, bnr_ch = chans if len(chans) == 2 else (1, 2)
    omni, bnr = decimate3(raw[:, omni_ch]), decimate3(raw[:, bnr_ch])
    skip = RATE // 2
    omni, bnr = omni[skip:], bnr[skip:]
    fe = Frontend()
    feats = []
    for start in range(0, len(omni) - FRAME + 1, SHIFT):
        a = fe.frame(omni[start:start + FRAME])
        b = fe.frame(bnr[start:start + FRAME])
        feats.append(np.concatenate([a, b]))
    feats = np.array(feats, dtype=np.float32)
    print(f"frames {len(feats)}; omni log-mel mean {feats[:, :40].mean():.2f} [p5 {np.percentile(feats[:, :40], 5):.2f}, p95 {np.percentile(feats[:, :40], 95):.2f}]; bnr mean {feats[:, 40:].mean():.2f}")

    it = Interpreter(model_path=sys.argv[2])
    run = it.get_signature_runner("serving_default")
    det = run.get_input_details()
    state = {k: np.full(v["shape"], v["quantization"][1], dtype=v["dtype"]) for k, v in det.items() if k != "frames"}
    probs = []
    for i in range(0, len(feats) - 5, 6):
        out = run(frames=feats[i:i + 6].reshape(1, 1, 6, 80), **state)
        for k in state:
            state[k] = out[k + "_new"]
        z = out["logits"].reshape(6, 2)
        e = np.exp(z - z.max(1, keepdims=True))
        probs.extend(e[:, 1] / e.sum(1))
    probs = np.array(probs[16:])
    if save:
        np.save(save, probs.astype(np.float32))
    s = np.array([probs[max(0, n - 4):n + 1].sum() / min(5, n + 2) for n in range(len(probs))])
    c = np.array([s[max(0, n - 19):n + 1].max() for n in range(len(s))])
    hits = [n for n in range(1, len(c)) if c[n] > 0.861 and c[n - 1] <= 0.861]
    print(f"keyword posterior max {probs.max():.3f} mean {probs.mean():.3f}; confidence max {c.max():.3f} at {c.argmax() * 10} ms; hits {[(h * 10) for h in hits]} ms; subthreshold frames {(c > 0.435).sum()}")


if __name__ == "__main__":
    main()
