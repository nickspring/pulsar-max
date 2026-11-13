"""Frequency-filter reference on station recordings: per-frame sums and the gate value at every 0.5 s.

usage: ff_check.py <capture.raw 48k s16 16ch> <channel>...
"""
import sys
import numpy as np
sys.path.insert(0, str(__import__("pathlib").Path(__file__).resolve().parent))
import freqfilter
from proto import decimate3

raw = np.fromfile(sys.argv[1], dtype="<i2").reshape(-1, 16)
for ch in [int(c) for c in sys.argv[2:]]:
    x = decimate3(raw[:, ch])[8000:]
    ff = freqfilter.FreqFilter()
    feats, ends = ff.process(x)
    sums = np.asarray(feats).reshape(len(ends), -1).sum(axis=1)
    worst = min(ff.hit_confidence(feats, ends, f) for f in range(30, len(x) // 160, 50))
    print(f"ch{ch}: frames {len(ends)}, frame score min {sums.min():.2f} mean {sums.mean():.2f} max {sums.max():.2f}; worst gate value {worst:.2f} (veto if <= -3.6)")
