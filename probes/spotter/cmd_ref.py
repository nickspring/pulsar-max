"""Reference of io.sdk's command spotter on the Station Max (libquasar_daemons.so 3.367.25): Kaldi
nnet3 acoustic model with Yandex Q16 components, log-mel fbank of the vqe channel and the
multi-phrase chen14 decoder. crates/max-wake/src/{nnet,chen14_multi,command}.rs follow it.

usage: cmd_ref.py <model_dir> <vqe.raw 16 kHz s16 mono>
       cmd_ref.py <model_dir> --capture <capture.raw 48 kHz s16 16ch> [--save16 <out.raw>]

<model_dir> is a local copy of the station's command spotter_model dir (acoustic_model.nnet,
flags.txt, words.txt), never the repo. Prints hits as "phrase frame*10". --capture takes channel 1
(vqe), decimates it to 16 kHz and drops the first 0.5 s.
"""
import pathlib
import struct
import sys

import numpy as np

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from proto import BANKS, DITHER, FRAME, RATE, SHIFT, decimate3  # noqa: E402

F32 = np.float32
NFFT = 512
WINDOW = np.power(0.5 - 0.5 * np.cos(2 * np.pi * np.arange(FRAME) / (FRAME - 1)), 0.85).astype(F32)


class Reader:
    def __init__(self, data):
        self.d, self.p = data, 0

    def token(self):
        while self.d[self.p:self.p + 1] == b" ":
            self.p += 1
        end = self.d.index(b" ", self.p)
        t = self.d[self.p:end].decode()
        self.p = end + 1
        return t

    def basic(self, size, fmt):
        assert self.d[self.p] == size, self.d[self.p:self.p + 8]
        v = struct.unpack(fmt, self.d[self.p + 1:self.p + 1 + size])[0]
        self.p += 1 + size
        return v

    def obj(self):
        t = self.token()
        if t == "FV":
            n = self.basic(4, "<i")
            v = np.frombuffer(self.d, "<f4", n, self.p)
            self.p += 4 * n
            return v
        if t == "SIM":
            r, c = self.basic(4, "<i"), self.basic(4, "<i")
            v = np.frombuffer(self.d, "<i2", r * c, self.p).reshape(r, c)
            self.p += 2 * r * c
            return v
        raise ValueError(t)


def parse(data):
    """Returns (config lines, {component name: (type, fields)})."""
    head = b"\0B<Nnet3> \n"
    assert data.startswith(head)
    end = data.index(b"\n\n", len(head))
    config = data[len(head):end].decode().split("\n")
    r = Reader(data)
    r.p = end + 2
    assert r.token() == "<NumComponents>"
    comps = {}
    for _ in range(r.basic(4, "<i")):
        assert r.token() == "<ComponentName>"
        name, typ = r.token(), r.token()
        close = "</" + typ[1:]
        f = {}
        while True:
            key = r.token()
            if key == close:
                break
            if key in ("<S>",):
                f[key] = r.basic(4, "<f")
            elif key in ("<ContextParam>", "<Dim>"):
                f[key] = r.basic(4, "<i")
            elif key == "<Count>":
                f[key] = r.basic(8, "<d")
            else:
                f[key] = r.obj()
        comps[name] = (typ, f)
    assert r.token() == "</Nnet3>"
    return config, comps


def topology(config):
    """Trunk component names from the input node, and the head affine names in output order."""
    nodes, outputs, input_name = {}, None, None
    for line in config:
        kind, rest = line.split(" ", 1)
        kv = dict(p.split("=", 1) for p in rest.replace(", ", ",").split(" "))
        if kind == "input-node":
            input_name = kv["name"]
        elif kind == "component-node":
            nodes[kv["name"]] = (kv["component"], kv["input"])
        elif kind == "output-node":
            src = kv["input"]
            outputs = src[len("Append("):-1].split(",") if src.startswith("Append(") else [src]
    heads = [nodes[sm][1] for sm in outputs]
    branch = nodes[heads[0]][1]
    trunk, cur = [], branch
    while cur != input_name:
        trunk.append(nodes[cur][0])
        cur = nodes[cur][1]
    return trunk[::-1], [nodes[h][0] for h in heads]


def fbank(x):
    """cmdnet.py's fbank: one channel, so the dither index advances one frame per frame."""
    idx = 0
    out = []
    for s in range(0, len(x) - FRAME + 1, SHIFT):
        f = x[s:s + FRAME].astype(F32).copy()
        f += F32(0.1) * DITHER[(idx + np.arange(FRAME)) % 2048]
        idx = (idx + FRAME) % 2048
        f -= f.mean()
        y = f.copy()
        y[1:] = f[1:] - 0.97 * f[:-1]
        y[0] = f[0] - 0.97 * f[0]
        y *= WINDOW
        sp = np.fft.rfft(y, NFFT)
        out.append(np.log(BANKS @ (sp.real ** 2 + sp.imag ** 2).astype(F32)))
    return np.array(out, dtype=F32).reshape(-1, len(BANKS))


def hswish(x):
    return x * np.clip(x + 3, 0, 6) / 6


class Net:
    """Whole-sequence float64 forward pass; the conv is causal, taps oldest first, no dilation."""

    def __init__(self, data):
        config, self.comps = parse(data)
        self.trunk, self.heads = topology(config)

    def forward(self, feats):
        x = feats.astype(np.float64)
        for name in self.trunk:
            typ, f = self.comps[name]
            if typ == "<FixedBiasComponent>":
                x = x + f["<Bias>"]
            elif typ == "<FixedScaleComponent>":
                x = x * f["<Scales>"]
            elif typ == "<Q16FixedAffineComponent>":
                w = f["<LinearParams>"].astype(np.float64) * f["<S>"]
                x = x @ w.T + f["<BiasParams>"]
            elif typ == "<Q16GroupConvCorouComponent>":
                w = f["<LinearParams>"].astype(np.float64) * f["<S>"]
                k = w.shape[1]
                xp = np.concatenate([np.zeros((k - 1, x.shape[1])), x])
                y = np.zeros_like(x)
                for i in range(k):
                    y += w[:, i] * xp[i:i + len(x)]
                x = y + f["<BiasParams>"]
            elif typ == "<hardswish>":
                x = hswish(x)
            else:
                raise ValueError(typ)
        out = []
        for name in self.heads:
            f = self.comps[name][1]
            z = x @ (f["<LinearParams>"].astype(np.float64) * f["<S>"]).T + f["<BiasParams>"]
            e = np.exp(z - z.max(1, keepdims=True))
            out.append(e / e.sum(1, keepdims=True))
        return np.concatenate(out, 1)


class Chen14Multi:
    """io.sdk Spotter::Chen14 for several multi-word commands (Smooth 0x3948530, Confidence
    0x3948130, Evaluate 0x3947d40, peak 0x3948390, Process 0x3948f90, OnHit 0x3949a90, Reset
    0x3949694). geometric metric, hit-at-peak subhit; f32 throughout."""

    def __init__(self, commands, thresholds, subthresholds, w_smooth, w_max, interval, zeroing):
        self.commands = commands
        self.thr = np.array(thresholds, F32)
        self.sub = np.array(subthresholds, F32)
        self.ws, self.wm, self.interval, self.zeroing = w_smooth, w_max, interval, zeroing
        self.slots = [(c, j, u) for c, units in enumerate(commands) for j, u in enumerate(units)]
        self.frame = 0
        self.block_until = 0
        self.conf = None
        self.reset(0)

    def reset(self, reset_frame):
        n = len(self.slots)
        self.sum = np.zeros(n, F32)
        self.ring = {}
        self.smoothed = np.zeros((self.wm, n), F32)
        self.reset_frame = reset_frame
        self.prev_id, self.prev_level, self.prev_ratio = 0, 0, F32(0)
        self.flag_a = self.flag_b = False

    def smooth(self, post, n):
        for k, (c, j, u) in enumerate(self.slots):
            self.sum[k] = F32(self.sum[k] + F32(post[u]))
            if self.ws <= n:
                old = self.ring.get((n - self.ws) % self.ws)
                if old is not None:
                    self.sum[k] = F32(self.sum[k] - F32(old[u]))
        self.ring[n % self.ws] = post.astype(F32).copy()
        div = F32(min(self.ws, n - self.reset_frame + 1))
        for k in range(len(self.slots)):
            s = F32(self.sum[k] / div)
            self.smoothed[n % self.wm, k] = s if s > 0 else F32(0)

    def confidence(self, n):
        start = (n + 1) % self.wm
        order = [(start + i) % self.wm for i in range(self.wm)]
        confs = []
        k = 0
        for units in self.commands:
            pos = 0
            acc = F32(1)
            for _ in units:
                col = self.smoothed[order[pos:], k]
                best_i = int(np.argmax(col))
                acc = F32(acc * col[best_i])
                pos += best_i
                k += 1
            if len(units) == 2:
                acc = F32(np.sqrt(acc))
            elif len(units) > 2:
                acc = F32(acc ** F32(1.0 / len(units)))
            confs.append(acc)
        return np.array(confs, F32)

    def evaluate(self, conf):
        best = (0, 0, F32(0))
        for i, c in enumerate(conf):
            level = 2 if self.thr[i] < c else (1 if self.sub[i] < c else 0)
            ratio = F32(c / self.thr[i])
            better = ratio > best[2] if level == best[0] else level > best[0]
            if better:
                best = (level, i, ratio)
        level, i, ratio = best
        cid = i + 1 if level == 2 else (-(i + 1) if level == 1 else 0)
        return cid, level, ratio

    def push(self, post):
        """One frame of posteriors; returns (command index, frame) on a hit. self.conf keeps the
        frame's confidences, None while the interval after a hit skips frames."""
        out = None
        self.conf = None
        if self.frame >= self.block_until:
            n = self.frame + 1
            self.smooth(post, n)
            self.conf = self.confidence(n)
            cid, level, ratio = self.evaluate(self.conf)
            better = ratio > self.prev_ratio if level == self.prev_level else level > self.prev_level
            res = 0
            if self.prev_id > 0:
                if cid > 0 and cid != self.prev_id:
                    res = cid
                    self.flag_a = self.flag_b = True
                elif cid == 0:
                    self.flag_a = self.flag_b = False
                elif cid < 0:
                    self.flag_a = False
            elif cid > 0:
                res = cid
                self.flag_a = self.flag_b = True
            elif self.prev_id < 0:
                if not self.flag_b and not better:
                    res = self.prev_id
                    self.flag_b = True
                elif cid == 0:
                    self.flag_a = self.flag_b = False
                else:
                    self.flag_a = False
            self.prev_id, self.prev_level, self.prev_ratio = cid, level, ratio
            if res > 0:
                out = (res - 1, self.frame)
        self.frame += 1
        if out is not None:
            self.block_until = self.frame + self.interval
            if self.zeroing:
                self.reset(self.block_until)
        return out


def load_config(flags_text, words_text):
    flags = dict(line.rstrip("\n").split(" ", 1) for line in flags_text.splitlines() if " " in line)
    words = [line.split()[0] for line in words_text.splitlines() if line.strip()]
    commands = [[int(u) for u in c.split(",")] for c in flags["commands"].split(":")]
    decoder = dict(
        commands=commands,
        thresholds=[float(x) for x in flags["thresholds"].split(":")],
        subthresholds=[float(x) for x in flags["subthresholds"].split(":")],
        w_smooth=int(flags["w-smooth"]),
        w_max=int(flags.get("w-max", 100)),
        interval=int(flags.get("interval-after-last-activation", 0)),
        zeroing=flags.get("zeroing-confidence-buffer-after-hit") == "true",
    )
    names = [" ".join(words[u] for u in c) for c in commands]
    return decoder, names


def run(net, decoder_cfg, names, pcm):
    post = net.forward(fbank(pcm)).astype(F32)
    ch = Chen14Multi(**decoder_cfg)
    hits = []
    for p in post:
        h = ch.push(p)
        if h:
            hits.append((names[h[0]], h[1]))
    return hits


def main():
    model = pathlib.Path(sys.argv[1])
    args = sys.argv[2:]
    if args[0] == "--capture":
        raw = np.fromfile(args[1], "<i2").reshape(-1, 16)
        pcm = decimate3(raw[:, 1])[RATE // 2:]
        if "--save16" in args:
            pcm.astype("<i2").tofile(args[args.index("--save16") + 1])
    else:
        pcm = np.fromfile(args[0], "<i2")
    net = Net((model / "acoustic_model.nnet").read_bytes())
    decoder, names = load_config((model / "flags.txt").read_text(), (model / "words.txt").read_text())
    for name, frame in run(net, decoder, names, pcm):
        print(name, frame * 10)


if __name__ == "__main__":
    main()
