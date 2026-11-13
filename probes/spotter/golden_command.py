"""Golden fixtures for the command spotter tests of crates/max-wake (nnet.rs, chen14_multi.rs,
command.rs), all synthetic.

usage: /tmp/litert-venv/bin/python -B -I golden_command.py [out_dir]   (default crates/max-wake/tests/fixtures)

Writes a tiny nnet3 model in the station model's binary format (same component types, random
weights), its flags.txt and words.txt, an integer test signal's fbank features and posteriors,
the decoder hits on them, and decoder cases on synthetic posteriors. Expected values come from
cmd_ref.py.
"""
import pathlib
import struct
import sys

import numpy as np

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import cmd_ref  # noqa: E402
from golden_signal import lcg  # noqa: E402

F32 = np.float32
SIGNAL_SAMPLES = 32000
SEGMENT = 3200
PERIODS = (40, 0, 22, 64, 0, 30, 90, 0, 26, 50)

WORDS = ["SIL", "один", "два", "три", "четыре", "SIL", "пять", "шесть"]
COMMANDS = [[1], [2], [3, 4], [6, 4], [7], [1, 2, 7]]
W_SMOOTH, W_MAX, INTERVAL, BATCH = 6, 30, 12, 4


def signal():
    """Rebuilt in command.rs tests: integer arithmetic only."""
    a = lcg(7, SIGNAL_SAMPLES)
    out = []
    for i in range(SIGNAL_SAMPLES):
        seg = i // SEGMENT
        period = PERIODS[seg]
        tone = 0 if period == 0 else (3000 if (i // (period // 2)) % 2 else -3000)
        out.append((a[i] >> (4 + seg % 3)) + tone)
    return np.array(out, dtype=np.int16)


def tok(s):
    return s.encode() + b" "


def i32(v):
    return b"\x04" + struct.pack("<i", v)


def f32(v):
    return b"\x04" + struct.pack("<f", v)


def fv(a):
    a = np.asarray(a, "<f4")
    return tok("FV") + i32(len(a)) + a.tobytes()


def sim(m):
    return tok("SIM") + i32(m.shape[0]) + i32(m.shape[1]) + m.astype("<i2").tobytes()


def component(name, typ, body):
    return tok("<ComponentName>") + tok(name) + tok(typ) + body + tok("</" + typ[1:])


def q16(rng, rows, cols):
    w = rng.randint(-32767, 32768, size=(rows, cols))
    w.flat[rng.randint(w.size)] = 32767
    return w.astype(np.int16)


def scale_for(raw, std):
    return F32(std / raw.std())


def build_model(feats, rng):
    """Calibrates each scale on the features so hardswish and softmax see their whole range."""
    comps, x = [], feats.astype(np.float64)
    mean, sd = feats.mean(0).astype(F32), feats.std(0).astype(F32)
    comps.append(("cmvn_add_0", "<FixedBiasComponent>", tok("<Bias>") + fv(-mean)))
    comps.append(("cmvn_mul_1", "<FixedScaleComponent>", tok("<Scales>") + fv(F32(1) / sd)))
    x = (x - mean) * (F32(1) / sd)

    def affine(name, rows, bias_sd, std):
        nonlocal x
        w = q16(rng, rows, x.shape[1])
        s = scale_for(x @ w.T.astype(np.float64), std)
        b = (rng.randn(rows) * bias_sd).astype(F32)
        comps.append((name, "<Q16FixedAffineComponent>", tok("<S>") + f32(s) + tok("<LinearParams>") + sim(w) + tok("<BiasParams>") + fv(b)))
        x = x @ (w.astype(np.float64) * s).T + b

    def conv(name, taps, ctx, std):
        nonlocal x
        w = q16(rng, x.shape[1], taps)
        xp = np.concatenate([np.zeros((taps - 1, x.shape[1])), x])
        raw = sum(w[:, i].astype(np.float64) * xp[i:i + len(x)] for i in range(taps))
        s = scale_for(raw, std)
        b = (rng.randn(x.shape[1]) * 0.5).astype(F32)
        comps.append((name, "<Q16GroupConvCorouComponent>", tok("<S>") + f32(s) + tok("<LinearParams>") + sim(w) + tok("<BiasParams>") + fv(b) + tok("<ContextParam>") + i32(ctx)))
        x = raw * s + b

    def hswish(name):
        nonlocal x
        comps.append((name, "<hardswish>", b""))
        x = cmd_ref.hswish(x)

    affine("svdf_features_2", 12, 0.0, 2.0)
    conv("svdf_times_3", 5, 1, 2.5)
    hswish("hardswish_4")
    affine("tdnn_5", 10, 0.5, 2.5)
    hswish("hardswish_6")
    affine("svdf_features_7", 14, 0.0, 2.0)
    conv("svdf_times_8", 7, 2, 2.5)
    hswish("hardswish_9")
    trunk = x
    for name, rows in (("tdnn_10", 5), ("tdnn_11", 3)):
        x = trunk
        affine(name, rows, 0.3, 2.0)
    for name, rows in (("softmax_12", 5), ("softmax_13", 3)):
        body = tok("<Dim>") + i32(rows) + tok("<ValueAvg>") + fv([]) + tok("<DerivAvg>") + fv([]) + tok("<Count>") + b"\x08" + struct.pack("<d", 0.0)
        comps.append((name, "<SoftmaxComponent>", body))

    inputs = {"tdnn_10": "hardswish_9", "tdnn_11": "hardswish_9", "softmax_12": "tdnn_10", "softmax_13": "tdnn_11"}
    config = ["input-node name=input dim=40"]
    prev = "input"
    for name, _, _ in comps:
        src = inputs.get(name, prev)
        config.append(f"component-node name={name} component={name} input={src}")
        prev = name
    config.append("output-node name=output input=Append(softmax_12, softmax_13) objective=linear")
    data = b"\0B" + tok("<Nnet3>") + b"\n" + "\n".join(config).encode() + b"\n\n"
    data += tok("<NumComponents>") + i32(len(comps))
    data += b"".join(component(*c) for c in comps)
    return data + tok("</Nnet3>") + b"\n"


def f32s(v):
    return str(F32(v))


def decode(post, decoder):
    ch = cmd_ref.Chen14Multi(**decoder)
    hits, confs = [], []
    for p in post:
        h = ch.push(p)
        confs.append(ch.conf)
        if h:
            hits.append(h)
    return hits, confs


def thresholds_for(post, commands):
    """Below each command's peak confidence, and away from every confidence and ratio tie the
    decoder sees, so f32 rounding in the Rust forward pass cannot move a hit."""
    peak = np.array(decode(post, decoder_cfg(commands, [1e9] * len(commands)))[1]).max(0)
    for factor in (0.8, 0.75, 0.85, 0.7, 0.9, 0.65):
        thr = [F32(round(float(p) * factor, 3)) for p in peak]
        _, confs = decode(post, decoder_cfg(commands, thr))
        ratios = np.array([c for c in confs if c is not None]) / np.array(thr)
        if np.abs(ratios - 1).min() < 1e-3:
            continue
        top = [np.sort(r[r > 1])[::-1][:2] for r in ratios]
        if all(len(t) < 2 or t[0] - t[1] > 1e-3 * t[0] for t in top):
            return thr
    raise SystemExit("no threshold set with margins")


def decoder_cfg(commands, thr, sub=None, ws=W_SMOOTH, wm=W_MAX, interval=INTERVAL, zeroing=True):
    return dict(commands=commands, thresholds=thr, subthresholds=sub or thr, w_smooth=ws, w_max=wm, interval=interval, zeroing=zeroing)


def flags_text(thr):
    t = ":".join(f32s(v) for v in thr)
    cmds = ":".join(",".join(str(u) for u in c) for c in COMMANDS)
    return f"""am-batch-size {BATCH}
am-type nnet
commands {cmds}
dither 0.1
dynamically-quantized-matmul true
feat-frontend log-mel-fbank
feat-pipeline
frame-shift 10
frame-size 25
hit-at-peak subhit
interval-after-last-activation {INTERVAL}
mel-num-bins 40
metric geometric
need-priors false
nnet-representation float
sample-rate 16000
skip-rate 1
spotter-type chen14
subthresholds {t}
thresholds {t}
use-vad false
version synthetic-commands
w-max {W_MAX}
w-smooth {W_SMOOTH}
zeroing-confidence-buffer-after-hit true
"""


def synthetic_posteriors(rng, frames, units, bursts):
    """Small noise plus triangular bursts (unit, centre, half width, height), 3 decimals."""
    p = rng.rand(frames, units) * 0.03
    t = np.arange(frames)
    for u, c, w, h in bursts:
        p[:, u] = np.maximum(p[:, u], h * np.clip(1 - np.abs(t - c) / w, 0, None))
    return np.round(np.clip(p, 0, 1), 3).astype(F32)


DECODER_CASES = [
    ("station", 320, 7, [[1], [2], [3, 4], [5], [3, 6, 5]], [0.5, 0.3, 0.4, 0.6, 0.35], None, 8, 60, 35, True,
     [(1, 30, 10, 0.9), (2, 60, 6, 0.5), (1, 64, 6, 0.8), (3, 120, 6, 0.7), (4, 132, 6, 0.6),
      (4, 180, 6, 0.8), (3, 192, 6, 0.8), (3, 240, 5, 0.6), (6, 250, 5, 0.7), (5, 262, 6, 0.9),
      (2, 300, 4, 0.45)]),
    ("takeover", 200, 6, [[1], [2], [1, 2], [4, 3]], [0.3, 0.35, 0.3, 0.25], [0.1, 0.2, 0.15, 0.1], 3, 7, 0, False,
     [(1, 20, 8, 0.6), (2, 26, 8, 0.9), (4, 70, 5, 0.5), (3, 76, 5, 0.6), (3, 120, 5, 0.6),
      (4, 126, 5, 0.6), (1, 160, 12, 0.5), (2, 166, 3, 0.5)]),
    ("interval", 200, 6, [[1, 2, 3], [4], [5, 4]], [0.2, 0.5, 0.3], [0.2, 0.5, 0.3], 5, 12, 6, False,
     [(1, 20, 6, 0.7), (2, 28, 6, 0.6), (3, 36, 6, 0.8), (4, 40, 20, 0.9), (5, 100, 6, 0.8),
      (4, 106, 6, 0.8), (1, 150, 4, 0.4), (2, 154, 4, 0.4), (3, 158, 4, 0.4)]),
]


def decoder_cases(rng):
    lines = ["# generated by probes/spotter/golden_command.py from cmd_ref.Chen14Multi",
             "# case <name> <w_smooth> <w_max> <interval> <zeroing> <commands> <thresholds> <subthresholds>",
             "# p <posteriors>, c <confidences or ->, h <frame> <command> after the frame that hit"]
    for name, frames, units, commands, thr, sub, ws, wm, interval, zeroing, bursts in DECODER_CASES:
        thr = [F32(v) for v in thr]
        sub = [F32(v) for v in (sub or thr)]
        post = synthetic_posteriors(rng, frames, units, bursts)
        ch = cmd_ref.Chen14Multi(**decoder_cfg(commands, thr, sub, ws, wm, interval, zeroing))
        spec = ":".join(",".join(str(u) for u in c) for c in commands)
        lines.append(f"case {name} {ws} {wm} {interval} {int(zeroing)} {spec} "
                     + ":".join(f32s(v) for v in thr) + " " + ":".join(f32s(v) for v in sub))
        hits = []
        for p in post:
            h = ch.push(p)
            lines.append("p " + " ".join(f32s(v) for v in p))
            lines.append("c -" if ch.conf is None else "c " + " ".join(f32s(v) for v in ch.conf))
            if h:
                lines.append(f"h {h[1]} {h[0]}")
                hits.append(h)
        print(f"decoder case {name}: hits {hits}")
    return "\n".join(lines) + "\n"


def main():
    out = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else HERE.parent.parent / "crates/max-wake/tests/fixtures"
    out.mkdir(parents=True, exist_ok=True)
    rng = np.random.RandomState(2026)

    pcm = signal()
    feats = cmd_ref.fbank(pcm)
    model = build_model(feats, rng)
    net = cmd_ref.Net(model)
    post = net.forward(feats).astype(F32)
    thr = thresholds_for(post, COMMANDS)
    hits, _ = decode(post, decoder_cfg(COMMANDS, thr))

    (out / "command_tiny.nnet").write_bytes(model)
    (out / "command_flags.txt").write_text(flags_text(thr))
    (out / "command_words.txt").write_text("".join(f"{w} {i}\n" for i, w in enumerate(WORDS)))
    feats.astype("<f4").tofile(out / "command_fbank.f32le")
    post.astype("<f4").tofile(out / "command_post.f32le")
    (out / "command_hits.txt").write_text(
        "# generated by probes/spotter/golden_command.py: <frame> <command> per decoder hit, before the debounce\n"
        + "".join(f"{frame} {cmd}\n" for cmd, frame in hits))
    (out / "chen14_multi.txt").write_text(decoder_cases(rng))
    print(f"model {len(model)} B, frames {len(feats)}, thresholds {[f32s(v) for v in thr]}, hits {hits}")


if __name__ == "__main__":
    main()
