"""Reference of io.sdk's Spotter::Chen14 decoder for one command of one unit, hit-at-peak subhit
(libquasar_daemons.so 3.367.25), and the generator of the golden values in
crates/max-wake/src/chen14.rs.

usage: chen14_ref.py             print the golden cases as Rust
       chen14_ref.py probs.npy   decisions for posteriors saved by a local run of the model

Smooth 0x3948530, Confidence 0x3948130, Evaluate 0x3947d40, peak state machine 0x3948390,
Process 0x3948f90, Reset 0x3949694. Arithmetic is float32 throughout, as in the binary.
"""
import sys
import numpy as np

F32 = np.float32


class Chen14:
    def __init__(self, w_smooth, w_max, threshold, subthreshold):
        self.w_smooth, self.w_max = w_smooth, w_max
        self.threshold, self.subthreshold = F32(threshold), F32(subthreshold)
        self.reset()

    def reset(self):
        self.n = 0
        self.ring = np.zeros(self.w_smooth, dtype=F32)
        self.sum = F32(0)
        self.smoothed = np.zeros(self.w_max, dtype=F32)
        self.prev_id, self.prev_level, self.prev_ratio = 0, 0, F32(0)
        self.flag_a = self.flag_b = False

    def confidence(self, p):
        self.n += 1
        p = F32(p)
        self.sum = F32(self.sum + p)
        if self.w_smooth <= self.n:
            self.sum = F32(self.sum - self.ring[(self.n - self.w_smooth) % self.w_smooth])
        self.ring[self.n % self.w_smooth] = p
        s = F32(self.sum / F32(min(self.w_smooth, self.n + 1)))
        self.smoothed[self.n % self.w_max] = s if s > 0 else F32(0)
        start = (self.n + 1) % self.w_max
        best = self.smoothed[start]
        for k in range(1, self.w_max):
            v = self.smoothed[(start + k) % self.w_max]
            if v > best:
                best = v
        return F32(best)

    def push(self, p):
        """Returns (confidence, decision) with decision None, ("hit", frame) or ("subhit", frame)."""
        c = self.confidence(p)
        if c > self.threshold:
            level, cur_id = 2, 1
        elif c > self.subthreshold:
            level, cur_id = 1, -1
        else:
            level, cur_id = 0, 0
        ratio = F32(c / self.threshold)
        better = ratio > self.prev_ratio if level == self.prev_level else level > self.prev_level
        out = 0
        if self.prev_id > 0:
            if cur_id == 0:
                self.flag_a = self.flag_b = False
            elif cur_id < 0:
                self.flag_a = False
        elif cur_id > 0:
            out = cur_id
            self.flag_a = self.flag_b = True
        elif self.prev_id < 0:
            if not self.flag_b and not better:
                out = self.prev_id
                self.flag_b = True
            elif cur_id == 0:
                self.flag_a = self.flag_b = False
            else:
                self.flag_a = False
        self.prev_id, self.prev_level, self.prev_ratio = cur_id, level, ratio
        frame = self.n - 1
        if out > 0:
            return c, ("hit", frame)
        if out < 0:
            return c, ("subhit", frame - 1)
        return c, None


ONE = 1 << 24


def posteriors(n, seed, noise_shift, bumps):
    """Integer-only synthesis, so Rust rebuilds the same f32 values bit for bit."""
    x = seed
    out = []
    for t in range(n):
        x = (x * 6364136223846793005 + 1442695040888963407) % (1 << 64)
        env = max([h * max(0, w - abs(t - c)) // w for c, w, h in bumps] + [0])
        out.append(F32(min(ONE, env + (x >> noise_shift))) / F32(ONE))
    return out


CASES = [
    ("STATION", (5, 20, 0.861, 0.435), 400, 1, 43,
     [(50, 14, ONE // 2), (130, 16, 15 * ONE // 16), (180, 16, 15 * ONE // 16),
      (155, 70, 5 * ONE // 8), (260, 8, 7 * ONE // 8), (330, 12, 11 * ONE // 16)]),
    ("FAST", (3, 1, 0.75, 0.375), 200, 3, 41,
     [(40, 12, ONE // 2), (100, 30, 5 * ONE // 8), (160, 8, ONE // 4)]),
]


def print_golden():
    for name, cfg, n, seed, noise_shift, bumps in CASES:
        ref = Chen14(*cfg)
        results = [ref.push(p) for p in posteriors(n, seed, noise_shift, bumps)]
        print(f"// {name}: {cfg}, {n} frames, seed {seed}, noise >> {noise_shift}, bumps {bumps}")
        print(f"const {name}_DECISIONS: &[(bool, u64, u32)] = &[")
        for c, d in results:
            if d:
                print(f"    ({str(d[0] == 'hit').lower()}, {d[1]}, 0x{np.float32(c).view(np.uint32):08x}),")
        print("];")
        print(f"const {name}_CONFIDENCE: &[(usize, u32)] = &[")
        for i in range(0, n, 20):
            print(f"    ({i}, 0x{results[i][0].view(np.uint32):08x}),")
        print("];")


def main():
    if len(sys.argv) > 1:
        ref = Chen14(5, 20, 0.861, 0.435)
        for p in np.load(sys.argv[1]):
            c, d = ref.push(p)
            if d:
                print(d[0], d[1] * 10, "ms", f"confidence {c:.3f}")
        return
    print_golden()


if __name__ == "__main__":
    main()
