"""Freq-filter gate of the Yandex spotter (libquasar_daemons.so, Station Max), reimplemented.

Input is the freq-filter feature ring as the library keeps it: one row per freq-filter
frame (252 ms window, 177 ms hop, raw-mic channel), one column per configured frequency,
cell = log(inside) - log(min(outside_low, outside_high)) from 0x37ae530. Frame j is the
j-th frame since the TSpotter was created; its stored position is its END sample.

Constants (alisa flags.txt + defaults, 16 kHz):
  FF_WIN      = 4032  samples  sr*252/1000            0x37afa3c, init pos 0x37ae120
  FF_HOP      = 2832  samples  sr*177/1000            0x37afa64, pos += hop 0x37ae4b8
  SPOT_HOP    = 160   samples  spotter frame-shift 10 0x3976208 -> gate+0x38
  LEFT        = 4     frames   trunc(max(0,(979-252)/177))      0x37aef78..0x37aef90
  RING        = 16    frames   max(1,trunc((3000-252+177)/177)) 0x37adfa4..0x37adfcc
                               (freq-filter-buffer-size absent -> default 3000.0, 0x37acefc)
  THRESHOLD   = -3.6           config+0x44, gate+0x30
  REACT_AS_SUBHITS = True      default byte 1 at config+0x4c (0x37acf14), absent in flags
  REDUCE_FEATURES_FIRST = True config+0x80 = !value, default 0 (0x37acf34, 0x37adc34)
  EMPTY_VALUE = 1e10           0x501502f9, returned when no frame qualifies

Gate (0x37ae9a4, called once per pulled result at 0x397a4c8):
  hit_sample = SPOT_HOP * result.frame                       0x37ae9cc..0x37ae9d8
  m = newest frame in the ring with end_pos <= hit_sample    0x37ae9f4..0x37aea08 (signed <=)
  collect frames m, m-1, ..., m-LEFT; stop early at the never-written ring slot
                                                              0x37aea6c..0x37aeab4
  value = min over frames of (sum over freqs)   when REDUCE_FEATURES_FIRST  0x37aeca0..
        = sum over freqs of (min over frames)   otherwise                   0x37aeb28..
  vetoed = not (value > THRESHOLD)  (<= vetoes; a NaN value too) 0x37aee30, 0x37aee5c
    NaN frame sums are skipped by the min (fcsel mi); in the other mode a NaN in
    the hit frame survives the per-freq min and vetoes
  veto: REACT_AS_SUBHITS -> phrase_id = -phrase_id if > 0    0x37b536c
        else             -> phrase_id = 0 (result dropped)   0x37b539c
  errors (no result change, JSON freq_filter_state "failed"):
    no ring frame <= hit_sample in a full ring  -> status 1          0x37aea18
    full ring, oldest frame reached before LEFT frames collected
                                                -> status 0x100000001 0x37aeb0c
"""

import numpy as np

SAMPLE_RATE = 16000
FF_WIN = SAMPLE_RATE * 252 // 1000
FF_HOP = SAMPLE_RATE * 177 // 1000
SPOT_HOP = SAMPLE_RATE * 10 // 1000
LEFT = int(max(0.0, (979.0 - 252.0) / 177.0))
RING = max(1, int((3000.0 - 252.0 + 177.0) / 177.0))
THRESHOLD = np.float32(-3.6)
EMPTY_VALUE = np.float32(1e10)

STATUS_OK = 0
STATUS_NO_HIT_POSITION = 1
STATUS_NO_LEFT_CONTEXT = 0x100000001


def frame_end_sample(j):
    return FF_WIN + FF_HOP * j


def _value(rows, reduce_features_first):
    if len(rows) == 0:
        return EMPTY_VALUE
    rows = np.asarray(rows, dtype=np.float32)
    if reduce_features_first:
        best = EMPTY_VALUE
        for row in rows:
            s = np.float32(0.0)
            for x in row:
                s = np.float32(s + x)
            if s < best:
                best = s
        return best
    # fcmgt keeps the hit frame's value on NaN compares (0x37aebb0), unlike np.min.
    mins = rows[0].copy()
    for row in rows[1:]:
        take = mins > row
        mins[take] = row[take]
    s = np.float32(0.0)
    for x in mins:
        s = np.float32(s + x)
    return s


def gate_detailed(scores, hit_frame, threshold=THRESHOLD, left=LEFT, ring=RING,
                  reduce_features_first=True):
    """scores: (n_computed, n_freqs) or (n_computed,) per-frame sums; hit_frame: spotter
    frame index (10 ms). Returns dict(status, allowed, value, frame, count)."""
    scores = np.asarray(scores, dtype=np.float32)
    if scores.ndim == 1:
        scores = scores[:, None]
    n = scores.shape[0]
    hit_sample = SPOT_HOP * int(hit_frame)
    oldest = max(0, n - ring)
    ring_full = n >= ring

    m = None
    for j in range(n - 1, oldest - 1, -1):
        if frame_end_sample(j) <= hit_sample:
            m = j
            break
    if m is None:
        if ring_full:
            return dict(status=STATUS_NO_HIT_POSITION, allowed=True, value=None,
                        frame=None, count=0)
        # Before the ring wraps, slot RING-1 holds INT64_MIN and matches any hit (0x37ae118).
        value = EMPTY_VALUE
        return dict(status=STATUS_OK, allowed=bool(value > threshold), value=float(value),
                    frame=None, count=0)

    rows = []
    for k in range(left + 1):
        j = m - k
        if j < 0:
            break
        rows.append(scores[j])
        if ring_full and j == oldest and k < left:
            return dict(status=STATUS_NO_LEFT_CONTEXT, allowed=True, value=None,
                        frame=m, count=len(rows))

    value = _value(rows, reduce_features_first)
    allowed = bool(value > threshold)
    return dict(status=STATUS_OK, allowed=allowed, value=float(value), frame=m,
                count=len(rows))


def gate(scores, hit_frame, **kw):
    r = gate_detailed(scores, hit_frame, **kw)
    return r["allowed"], r["value"]


def apply_to_phrase_id(phrase_id, allowed, react_as_subhits=True):
    """Result.PhraseId after the gate: >0 hit, <0 subhit, 0 dropped."""
    if allowed:
        return phrase_id
    if react_as_subhits:
        return -phrase_id if phrase_id >= 1 else phrase_id
    return 0


if __name__ == "__main__":
    nf = 8
    hit = 300
    m = (hit * SPOT_HOP - FF_WIN) // FF_HOP
    clean = np.zeros((m + 2, nf), dtype=np.float32)
    assert gate(clean, hit) == (True, 0.0)
    assert gate_detailed(clean, hit)["frame"] == m

    marked = clean.copy()
    marked[m - 2] = -0.5
    allowed, value = gate(marked, hit)
    assert not allowed and abs(value + 4.0) < 1e-6
    assert apply_to_phrase_id(1, allowed) == -1
    assert apply_to_phrase_id(1, allowed, react_as_subhits=False) == 0

    marked_old = clean.copy()
    marked_old[m - LEFT - 1] = -0.5
    assert gate(marked_old, hit)[0]

    marked_future = clean.copy()
    marked_future[m + 1] = -0.5
    assert gate(marked_future, hit)[0]

    assert gate(clean[:0], 10) == (True, float(EMPTY_VALUE))
    assert gate(clean[:2], 10) == (True, float(EMPTY_VALUE))
    r = gate_detailed(clean[:3], 61)
    assert r["status"] == STATUS_OK and r["count"] == 3

    long = np.zeros((40, nf), dtype=np.float32)
    assert gate_detailed(long, 20)["status"] == STATUS_NO_HIT_POSITION
    late = frame_end_sample(40 - RING + 1) // SPOT_HOP
    assert gate_detailed(long, late)["status"] == STATUS_NO_LEFT_CONTEXT

    per_freq = clean.copy()
    per_freq[m, :4] = -0.8
    per_freq[m - 1, 4:] = -0.8
    assert gate(per_freq, hit)[0]
    assert not gate(per_freq, hit, reduce_features_first=False)[0]

    nan_scores = clean.copy()
    nan_scores[m, 0] = np.nan
    assert gate(nan_scores, hit) == (True, 0.0)
    assert not gate(nan_scores, hit, reduce_features_first=False)[0]
    nan_old = clean.copy()
    nan_old[m - 1, 0] = np.nan
    assert gate(nan_old, hit, reduce_features_first=False) == (True, 0.0)
    print("ok", dict(FF_WIN=FF_WIN, FF_HOP=FF_HOP, SPOT_HOP=SPOT_HOP, LEFT=LEFT, RING=RING))
