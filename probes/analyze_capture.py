"""Per-channel levels of a raw s16le capture: RMS per time window and RMS of the tone component.

usage: analyze_capture.py <file> <channels> [rate] [tone Hz] [windows "a-b,c-d" in seconds]
"""

import math
import sys
from array import array

# The HAL clicks up to 0 dBFS on every channel for about 0.5 s after the capture starts.
STARTUP_SECONDS = 0.5


def dbfs(x):
    return 20 * math.log10(x / 32768) if x > 0 else -120.0


def rms(samples):
    return math.sqrt(sum(s * s for s in samples) / len(samples)) if samples else 0.0


def tone_rms(samples, rate, freq):
    if not samples:
        return 0.0
    k = 2 * math.cos(2 * math.pi * freq / rate)
    s1 = s2 = 0.0
    for x in samples:
        s1, s2 = x + k * s1 - s2, s1
    power = s1 * s1 + s2 * s2 - k * s1 * s2
    return math.sqrt(max(power, 0.0)) * math.sqrt(2) / len(samples)


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__.strip())
    path, nch = sys.argv[1], int(sys.argv[2])
    rate = int(sys.argv[3]) if len(sys.argv) > 3 else 48000
    tone = float(sys.argv[4]) if len(sys.argv) > 4 else 440.0
    windows = sys.argv[5] if len(sys.argv) > 5 else "0.5-1.5,2.5-3.5,5-7"
    spans = [tuple(float(v) for v in w.split("-")) for w in windows.split(",")]

    data = array("h")
    with open(path, "rb") as f:
        data.frombytes(f.read())
    frames = len(data) // nch
    seconds = frames / rate
    print(f"frames={frames} seconds={seconds:.2f}")
    for a, b in spans:
        if b > seconds:
            print(f"warning: window {a:g}-{b:g}s goes past the end of the capture")
    header = "ch  " + "  ".join(f"rms {a:g}-{b:g}s  {tone:g}Hz" for a, b in spans)
    print(header + f"   peak  dc (after {STARTUP_SECONDS:g}s)")
    for ch in range(nch):
        chan = data[ch:frames * nch:nch]
        cells = []
        for a, b in spans:
            seg = chan[int(a * rate):int(b * rate)]
            cells.append(f"{dbfs(rms(seg)):7.1f}  {dbfs(tone_rms(seg, rate, tone)):7.1f}")
        steady = chan[int(STARTUP_SECONDS * rate):]
        peak = max((abs(s) for s in steady), default=0)
        dc = sum(steady) / len(steady) if steady else 0.0
        print(f"{ch:2d}  " + "     ".join(cells) + f"   {dbfs(peak):6.1f}  {dc:7.1f}")


if __name__ == "__main__":
    main()
