"""Reference model of io.sdk's .led pattern parser and player (libquasar_daemons.so, io.sdk 3.367.25).

usage: python3 -I ledref.py [pattern.led ...]   self-checks, then a summary of each given file

Addresses in comments refer to that binary. Colours are kept as 4-byte tuples in the order the
binary stores them in memory: (first hex pair, second, third, fourth) = (R, G, B, X).
parse(port=True) adds the limits of crates/max-core/src/ledpattern.rs; port=False is stock.
"""

import math
import re
import sys
from dataclasses import dataclass, field
from fractions import Fraction
from pathlib import Path

LED_COUNT = 24
WS = b" \t\n\v\f\r"
U64_MAX = 2**64 - 1

MAX_SMOOTH = 1000
MAX_FRAMES = 1 << 16
LOOP_AT_END = "loop after the last frame with smooth != 0"


class ParseError(Exception):
    pass


@dataclass
class Frame:
    colors: list  # list of (b0, b1, b2, b3) tuples, len == led_count
    duration: int  # ms, uint32
    pos: float = 0.0  # Frame+0x20; interpolated but never read by the ring drawer


@dataclass
class Pattern:
    frames: list = field(default_factory=list)
    loop: int = -1
    led_count: int = LED_COUNT
    name: bytes = b""
    background: bool = False
    smooth: int = 0
    logsmooth: bool = False
    gamma: bool = False
    size_optimization: bool = True
    loop_duration: int = None  # LedPattern+0xa8
    port: bool = True


def fma(a, b, c):
    return float(Fraction(a) * Fraction(b) + Fraction(c))


def round_half_away(x):
    f = math.floor(x)
    return f + 1 if x - f >= 0.5 else f


def gamma_lut():
    return [math.ceil(255.0 * math.pow(i / 255.0, 2.8)) for i in range(256)]


GAMMA = gamma_lut()


def parse_color(token):
    """0x45905f4: up to four 2-char hex pairs, each the longest hex prefix of the pair, else 0."""
    out = []
    for k in range(4):
        pair = token[2 * k:2 * k + 2]
        v, ok = 0, False
        for ch in pair:
            d = bytes([ch])
            if d in b"0123456789abcdefABCDEF":
                v = v * 16 + int(d, 16)
                ok = True
            else:
                break
        out.append(v if ok else 0)
    return tuple(out)


INT_RE = re.compile(rb"-?[0-9]+")
UINT_RE = re.compile(rb"[0-9]+")


class Tokens:
    """String-view tokenizer of the frame-line parser (+0x18 'done' also means 'last read failed')."""

    def __init__(self, line, skip_first=False):
        self.toks = line.split()
        self.i = 1 if skip_first else 0
        self.done = False

    def word(self):
        if self.done or self.i >= len(self.toks):
            self.done = True
            return None
        t = self.toks[self.i]
        self.i += 1
        return t

    def _num(self, regex, lo, hi):
        t = self.word()
        if t is None:
            return None
        if not regex.fullmatch(t) or not lo <= int(t) <= hi:
            self.done = True
            return None
        return int(t)

    def uint(self):
        return self._num(UINT_RE, 0, 2**32 - 1)

    def int(self):
        return self._num(INT_RE, -(2**31), 2**31 - 1)


def gradient(p1, c1, p2, c2, duration, n):
    """0x44fc968: c1 at led p1, c2 at led p2, linear both ways round the ring, round half away."""
    if p1 == p2:
        raise ParseError(f"Cannot create gradient between two same points: led1 = {p1} led2 = {p2}")
    if p2 >= n or p1 < 0 or p2 < 0 or p1 >= n:
        raise ParseError(
            f"Cannot create gradient between two points: led1 = {p1} led2 = {p2} ledCount = {n}")
    lo, hi, clo, chi = (p1, p2, c1, c2) if p1 <= p2 else (p2, p1, c2, c1)
    diff = [chi[b] - clo[b] for b in range(4)]

    def mix(t):
        return tuple(int(round_half_away(fma(float(diff[b]), t, float(clo[b])))) & 0xFF
                     for b in range(4))

    out = [(0, 0, 0, 0)] * n
    for i in range(lo, hi + 1):
        out[i] = mix(float(i - lo) / float(hi - lo))
    j = (hi + 1) % n
    arc = float(n - hi + lo)
    while j != lo:
        out[j] = mix(float((lo + n - j) % n) / arc)
        j = (j + 1) % n
    return Frame(out, duration, 0.0)


def parse_frame_line(tok, n):
    """0x44fbabc. On a malformed gradient stock reads uninitialised stack; here it is an error."""
    first = tok.word()
    if first == b"gradient":
        p1 = tok.int()
        c1 = tok.word() or b""
        p2 = tok.int()
        c2 = tok.word() or b""
        ms = tok.uint()
        if None in (p1, p2, ms):
            raise ParseError("Bad gradient format")
        return gradient(p1, parse_color(c1), p2, parse_color(c2), ms, n)
    colors = [parse_color(first or b"")]
    while len(colors) < n:
        t = tok.word()
        if t is None:
            break
        colors.append(parse_color(t))
    duration = tok.uint()
    if duration is None:
        raise ParseError("Bad LED pattern format")
    if tok.uint() is not None:
        raise ParseError("Too many tokens in line")
    return Frame(colors, duration, 0.0)


def _push(p, f):
    """0x45018b0: with size optimisation an equal frame extends the previous one."""
    if p.size_optimization and p.frames and p.frames[-1].colors == f.colors:
        p.frames[-1].duration = (p.frames[-1].duration + f.duration) & 0xFFFFFFFF
    else:
        if p.port and len(p.frames) >= MAX_FRAMES:
            raise ParseError(f"LED pattern has more than {MAX_FRAMES} frames")
        p.frames.append(f)


def _add_linear(p, f):
    """0x450084c."""
    if not p.frames:
        _push(p, f)
        return
    prev = p.frames[-1]
    if prev.colors == f.colors:
        _push(p, f)
        return
    n = p.smooth
    if n < 0:
        return
    if p.port and n > MAX_SMOOTH:
        raise ParseError(f"smooth {n} exceeds {MAX_SMOOTH}")
    steps = n + 1
    base, rem = f.duration // steps, f.duration % steps
    p0 = float(math.trunc(prev.pos))
    for k in range(steps):
        t = float(k + 1) / float(steps)
        colors = [tuple(math.trunc(fma(t, float(c[b] - a[b]), float(a[b]))) & 0xFF for b in range(4))
                  for a, c in zip(prev.colors, f.colors)]
        wrap = float(p.led_count) if f.pos < p0 else -0.0
        pos = fma(f.pos + wrap - p0, t, p0)
        _push(p, Frame(colors, base + (1 if k < rem else 0), pos))


def _add_log(p, f):
    """0x44fd934 ('logsmooth'); no shipped pattern uses it, byte-exactness not verified."""
    if not p.frames or p.frames[-1].colors == f.colors:
        _push(p, f)
        return
    n = p.smooth
    if n < 0:
        return
    prev = p.frames[-1]
    steps = n + 1
    for k in range(steps):
        t = float(k + 1) / float(steps)
        colors = []
        for a, c in zip(prev.colors, f.colors):
            px = []
            for b in range(4):
                la, lc = math.log10(a[b] + 1), math.log10(c[b] + 1)
                v = math.pow(10.0, fma(lc - la, t, la)) - 1.0
                px.append(math.trunc(max(v, 0.0)) & 0xFF)
            colors.append(tuple(px))
        _push(p, Frame(colors, f.duration // steps, 0.0))


def add_frame(p, f):
    """0x44fc4f4."""
    old_count, old_loop = len(p.frames), p.loop
    (_add_log if p.logsmooth else _add_linear)(p, f)
    if old_loop == old_count:
        p.loop = len(p.frames) - 1


def _rotated(colors, k):
    n = len(colors)
    return [colors[(j - k) % n] for j in range(n)]


def _expand(p, kind, f):
    """Multi-frame line keywords, 0x44fa154..0x44fac90."""
    n = p.led_count
    black = (0, 0, 0, 0)
    if kind == b"rotate":
        for k in range(n):
            add_frame(p, Frame(_rotated(f.colors, k), f.duration, float(k)))
    elif kind == b"step1":
        head = [f.colors[0]] + [black] * (n - 1)
        add_frame(p, Frame(_rotated(head, n - 1), f.duration, float(n - 1)))
        for k in range(n - 1):
            v = f.colors[:k + 2] + [black] * (n - k - 2)
            add_frame(p, Frame(_rotated(v, k), f.duration, float(k)))
    elif kind in (b"step2", b"fill"):
        for k in range(n):
            add_frame(p, Frame(f.colors[:k + 1] + [black] * (n - k - 1), f.duration, f.pos))
        if kind == b"step2":
            for k in range(1, n):
                add_frame(p, Frame([black] * k + f.colors[k:], f.duration, f.pos))
    elif kind == b"unfill":
        for k in range(n):
            add_frame(p, Frame([black] * k + f.colors[k:], f.duration, f.pos))
    else:
        add_frame(p, f)


def parse(text, led_count=LED_COUNT, port=True):
    """0x44f9a5c. Returns Pattern or raises ParseError (stock then rejects the whole file).
    LOOP_AT_END: stock abort()s when +0xa8 is unset and reads past the frames when loop == n."""
    if isinstance(text, str):
        text = text.encode()
    p = Pattern(led_count=led_count, port=port)
    for raw in text.split(b"\n"):
        line = raw.strip(WS)
        if not line or line[:1] == b"#":
            continue
        if line.startswith(b"no_size_optimization"):
            p.size_optimization = False
        elif line.startswith(b"loop"):
            p.loop = len(p.frames)
        elif line.startswith(b"smooth") or line.startswith(b"logsmooth"):
            if port and line.startswith(b"logsmooth"):
                raise ParseError("logsmooth unsupported")
            v = Tokens(line, skip_first=True).int()
            if v is not None:
                p.smooth = v
            p.logsmooth = line.startswith(b"logsmooth")
        elif line.startswith(b"gammacorrection"):
            p.gamma = True
        elif line.startswith(b"background"):
            p.background = True
        elif line.startswith(b"name"):
            t = Tokens(line, skip_first=True).word()
            if t is not None:
                p.name = t
        else:
            kind = next((k for k in (b"rotate", b"step1", b"step2", b"fill", b"unfill")
                         if line.startswith(k)), None)
            f = parse_frame_line(Tokens(line, skip_first=kind is not None), led_count)
            if p.loop == len(p.frames):
                p.loop_duration = f.duration
            _expand(p, kind, f)
    n = len(p.frames)
    if p.loop >= 0 and n >= 2 and p.loop != n - 1 and p.smooth != 0:
        if p.loop_duration is None or p.loop >= n:
            raise ParseError(LOOP_AT_END)
        lf = p.frames[p.loop]
        add_frame(p, Frame(list(lf.colors), p.loop_duration, lf.pos))
        p.loop += 1
    if port and not p.frames:
        raise ParseError("Empty LED pattern")
    if p.gamma:
        for f in p.frames:
            f.colors = [tuple(GAMMA[b] for b in c) for c in f.colors]
    return p


def valid_loop(p):
    return p.loop if 0 <= p.loop < len(p.frames) else None


def _locate(durs, t, start):
    acc = 0
    for i in range(start, len(durs)):
        if t < acc + durs[i]:
            return i, acc + durs[i]
        acc += durs[i]
    return None, acc


def frame_index(p, t_ms):
    """Ideal-scheduling player (LedPattern::update 0x45031b4). Returns (index, next_change_ms).
    A looped part of 0 ms holds the loop frame: every update wraps back to it."""
    durs = [f.duration for f in p.frames]
    if not durs:
        return None, None
    i, end = _locate(durs, t_ms, 0)
    if i is not None:
        return i, end
    total = end
    if valid_loop(p) is None:
        held = max((k for k, d in enumerate(durs) if d > 0), default=0)
        return held, None
    period = sum(durs[p.loop:])
    if period == 0:
        return p.loop, None
    cycles, rem = divmod(t_ms - total, period)
    i, end = _locate(durs, rem, p.loop)
    return i, total + cycles * period + end


class Player:
    """LedPattern::start 0x450338c and update 0x45031b4 as LedAnimator::drawLoop 0x4508134 drives
    them. Once an unlooped pattern has finished it holds its last frame with a nonzero duration:
    what stock leaves on the ring when every frame is drawn on time, whatever the caller's update
    schedule (a pattern hidden under an overlay is not drawn on time).

    One deliberate difference: on wrap stock stops on the loop frame even when it lasts 0 ms and
    drawLoop draws it once before the next update; here 0 ms frames after the wrap are skipped at
    once, which leaves the same picture and matches frame_index()."""

    def __init__(self, now):
        self.index, self.frame_start = 0, now

    def _advance(self, p, now):
        elapsed = now - self.frame_start
        while self.index < len(p.frames) and elapsed >= p.frames[self.index].duration:
            d = p.frames[self.index].duration
            self.frame_start += d
            elapsed -= d
            self.index += 1

    def update(self, p, now):
        n = len(p.frames)
        if self.index >= n:
            return
        self._advance(p, now)
        loop = valid_loop(p)
        if self.index < n or loop is None:
            return
        self.index, self.frame_start = loop, now
        self._advance(p, now)
        if self.index >= n:
            self.index = loop

    def finished(self, p):
        return self.index >= len(p.frames)

    def shown(self, p):
        if not self.finished(p):
            return self.index
        return max((k for k, f in enumerate(p.frames) if f.duration > 0), default=len(p.frames) - 1)

    def next_change(self, p):
        if self.finished(p):
            return None
        d = p.frames[self.index].duration
        loop = valid_loop(p)
        if d == 0 and loop is not None and loop <= self.index \
                and not any(f.duration for f in p.frames[loop:]):
            return None
        return min(self.frame_start + d, U64_MAX)


def render(p, t_ms):
    """fb5 pixels in pixel order (pixel q shows column n-1-q) as logical (R, G, B), plus the time the
    player next advances (for the last frame of an unlooped pattern: when it finishes and holds)."""
    i, nxt = frame_index(p, t_ms)
    n = p.led_count
    if i is None:
        return [(0, 0, 0)] * n, None
    cols = p.frames[i].colors
    return [cols[n - 1 - q][:3] for q in range(n)], nxt


def fb_bytes(p, t_ms):
    """Exact bytes io.sdk writes into the fb5 mapping (0x458f924): per pixel (b2, b1, b0) = (B, G, R)."""
    px, _ = render(p, t_ms)
    return bytes(v for r, g, b in px for v in (b, g, r))


def hexrow(px):
    return " ".join("%02x%02x%02x" % c for c in px)


def _line(cols, ms):
    return " ".join(cols) + " %d" % ms


def _solid(hexc, ms, n=LED_COUNT):
    return _line([hexc] * n, ms)


def _expect_error(text, needle, port=True):
    try:
        parse(text, port=port)
    except ParseError as e:
        assert needle in str(e), (needle, str(e))
        return
    raise AssertionError("no error for: " + needle)


def golden():
    n = LED_COUNT

    cols = ["%02x%02x%02x" % (k, 2 * k, 3 * k) for k in range(n)]
    p = parse(_line(cols, 100))
    px, nxt = render(p, 0)
    assert px[0] == (23, 46, 69) and px[23] == (0, 0, 0) and nxt == 100
    assert render(p, 100) == (px, None)
    assert fb_bytes(p, 0)[:3] == bytes([69, 46, 23])

    text = ("# comment\r\n\r\n  \t\n"
            + "\t" + _line(["FF00Aa"] + ["000000"] * (n - 1), 10) + " # trailing\r\n"
            + _line(["12345678", "#ff0000", "1g2h3i"] + ["0"] * (n - 3), 10) + "\n"
            + "#" + _solid("ffffff", 10) + "\n")
    p = parse(text)
    assert p.frames[0].colors[0] == (0xFF, 0x00, 0xAA, 0)
    assert len(p.frames) == 2
    assert p.frames[1].colors[:3] == [(0x12, 0x34, 0x56, 0x78), (0, 0xF0, 0, 0), (1, 2, 3, 0)]
    assert render(p, 10)[0][22:] == [(0, 0xF0, 0), (0x12, 0x34, 0x56)]

    _expect_error(_line(["000000"] * (n - 1), 10), "Bad LED pattern format")
    _expect_error(_line(["000000"] * n, 10) + " 5", "Too many tokens")
    _expect_error(_line(["000000"] * n, 10) + "ms", "Bad LED pattern format")
    _expect_error("gradient 3 ff0000 3 00ff00 10", "same points")
    _expect_error("gradient 0 ff0000 24 00ff00 10", "ledCount = 24")
    _expect_error("Loop\n" + _solid("000000", 10), "Bad LED pattern format")

    p = parse("smooth 3\n" + _solid("000000", 100) + "\n" + _solid("ffffff", 100))
    assert [f.duration for f in p.frames] == [100, 25, 25, 25, 25]
    assert [f.colors[0][0] for f in p.frames] == [0, 63, 127, 191, 255]
    assert render(p, 99)[0][0] == (0, 0, 0)
    assert render(p, 100) == ([(63, 63, 63)] * n, 125)
    assert render(p, 175)[0][0] == (255, 255, 255) and render(p, 10_000) == ([(255,) * 3] * n, None)

    p = parse("smooth 2\n" + _solid("000000", 5) + "\n" + _solid("090909", 10))
    assert [f.duration for f in p.frames] == [5, 4, 3, 3]
    assert [f.colors[0][0] for f in p.frames] == [0, 3, 6, 9]

    p = parse("smooth 4\n" + _solid("ffffff", 5) + "\n" + _solid("000000", 5))
    assert [f.colors[0][0] for f in p.frames] == [255, 204, 153, 102, 50, 0]

    p = parse("smooth 7\n" + _solid("101010", 100) + "\n" + _solid("101010", 50))
    assert len(p.frames) == 1 and p.frames[0].duration == 150
    p = parse("no_size_optimization\n" + _solid("101010", 100) + "\n" + _solid("101010", 50))
    assert [f.duration for f in p.frames] == [100, 50]

    p = parse(_solid("010101", 10) + "\nloop\n" + _solid("020202", 20) + "\n" + _solid("030303", 30))
    assert p.loop == 1
    seq = [(t, render(p, t)[0][0][0], render(p, t)[1]) for t in (0, 9, 10, 29, 30, 59, 60, 80, 110)]
    assert seq == [(0, 1, 10), (9, 1, 10), (10, 2, 30), (29, 2, 30), (30, 3, 60), (59, 3, 60),
                   (60, 2, 80), (80, 3, 110), (110, 2, 130)], seq

    pl = Player(0)
    pl.update(p, 45)
    assert (pl.shown(p), pl.next_change(p)) == (2, 60)
    pl.update(p, 75)
    assert (pl.shown(p), pl.frame_start, pl.next_change(p)) == (1, 75, 95)

    p = parse("smooth 1\n" + _solid("000000", 10) + "\nloop\n" + _solid("ffffff", 20) + "\n"
              + _solid("404040", 20))
    assert [f.colors[0][0] for f in p.frames] == [0, 127, 255, 159, 64, 159, 255] and p.loop == 3
    seq = [render(p, t)[0][0][0] for t in range(0, 130, 10)]
    assert seq == [0, 127, 255, 159, 64, 159, 255, 159, 64, 159, 255, 159, 64], seq

    p = parse(_solid("010101", 10) + "\n" + _solid("020202", 10) + "\n" + _solid("030303", 10))
    pl = Player(0)
    pl.update(p, 15)
    pl.update(p, 500)
    assert pl.finished(p) and pl.shown(p) == 2 and pl.next_change(p) is None
    late = Player(0)
    late.update(p, 500)
    assert late.shown(p) == 2, "the held frame does not depend on the update schedule"
    p = parse(_solid("010101", 10) + "\nno_size_optimization\n" + _solid("020202", 10) + "\n"
              + _solid("030303", 0))
    pl = Player(0)
    pl.update(p, 500)
    assert pl.finished(p) and pl.shown(p) == 1, "a trailing 0 ms frame is never shown"

    p = parse(_solid("010101", 10) + "\nloop\n" + _solid("020202", 0) + "\n" + _solid("030303", 0))
    pl = Player(0)
    pl.update(p, 10)
    assert (pl.shown(p), pl.next_change(p), pl.finished(p)) == (1, None, False)
    assert frame_index(p, 10) == (1, None)

    p = parse("gradient 0 000000 4 ff0000 50")
    r = [c[0] for c in p.frames[0].colors]
    assert (r[0], r[1], r[2], r[3], r[4], r[5], r[14], r[23]) == (0, 64, 128, 191, 255, 242, 128, 13), r
    assert parse("gradient 4 ff0000 0 000000 50").frames[0].colors == p.frames[0].colors
    assert parse("gradient 0 123456 1 123456 7").frames[0].colors == [(0x12, 0x34, 0x56, 0)] * n

    p = parse("loop\nrotate " + _line(["ff0000"] + ["000000"] * (n - 1), 10))
    assert len(p.frames) == n and p.loop == 0
    for t, col in ((0, 0), (15, 1), (239, 23), (240, 0), (255, 1)):
        px = render(p, t)[0]
        assert px[n - 1 - col] == (255, 0, 0) and px.count((0, 0, 0)) == n - 1, (t, col)

    p = parse("gammacorrection\n" + _line(["010101", "808080", "7f7f7f", "ffffff", "232323", "242424"]
                                         + ["000000"] * (n - 6), 10))
    assert [c[0] for c in p.frames[0].colors[:7]] == [1, 38, 37, 255, 1, 2, 0]
    p = parse("smooth 1\n" + _solid("000000", 10) + "\n" + _solid("ffffff", 10) + "\ngammacorrection")
    assert [f.colors[0][0] for f in p.frames] == [0, 37, 255]

    red, blk = (255, 0, 0, 0), (0, 0, 0, 0)
    p = parse("fill " + _line(["ff0000"] * n, 1))
    assert len(p.frames) == n and p.frames[2].colors[:4] == [red] * 3 + [blk]
    p = parse("unfill " + _line(["ff0000"] * n, 1))
    assert len(p.frames) == n and p.frames[0].colors == [red] * n
    assert p.frames[2].colors[:3] == [blk, blk, red]
    p = parse("step2 " + _line(["ff0000"] * n, 1))
    assert len(p.frames) == 2 * n - 1 and p.frames[n - 1].colors == [red] * n
    assert p.frames[n].colors[:2] == [blk, red]
    p = parse("step1 " + _line(["%02x0000" % (k + 1) for k in range(n)], 1))
    assert len(p.frames) == n
    assert p.frames[0].colors == [blk] * (n - 1) + [(1, 0, 0, 0)]
    assert p.frames[3].colors[:6] == [blk, blk, (1, 0, 0, 0), (2, 0, 0, 0), (3, 0, 0, 0), (4, 0, 0, 0)]

    p = parse("name  test-name extra\nbackground\nlogsmooth 4\n" + _solid("000000", 10), port=False)
    assert (p.name, p.background, p.logsmooth, p.smooth) == (b"test-name", True, True, 4)
    _expect_error("logsmooth 4\n" + _solid("000000", 10), "logsmooth unsupported")
    p = parse("smooth 9\nsmooth\nsmooth x\nsmooth20\n" + _solid("000000", 10))
    assert p.smooth == 9 and not p.logsmooth

    _expect_error("", "Empty LED pattern")
    assert not parse("", port=False).frames
    two = "\nno_size_optimization\n" + _solid("000000", 10) + "\n" + _solid("ffffff", 10)
    assert len(parse("smooth %d" % MAX_SMOOTH + two).frames) == MAX_SMOOTH + 2
    _expect_error("smooth %d" % (MAX_SMOOTH + 1) + two, "exceeds")
    assert len(parse("smooth %d" % (MAX_SMOOTH + 1) + two, port=False).frames) == MAX_SMOOTH + 3
    rot = "rotate " + _solid("000000", 1) + "\n"
    per = MAX_FRAMES // n
    assert len(parse("no_size_optimization\n" + rot * per).frames) == per * n
    _expect_error("no_size_optimization\n" + rot * (per + 1), "more than")
    _expect_error(_solid("000000", 1) + "\n" + _solid("ffffff", 1) + "\nloop\nsmooth 1", LOOP_AT_END)
    print("golden: OK")


def summary(name, p):
    durs = [f.duration for f in p.frames]
    intro = sum(durs[:p.loop]) if 0 <= p.loop < len(durs) else sum(durs)
    period = sum(durs[p.loop:]) if 0 <= p.loop < len(durs) else 0
    print(f"== {name}: frames={len(durs)} loop={p.loop} smooth={p.smooth} gamma={p.gamma} "
          f"intro_ms={intro} period_ms={period}")


def main(argv):
    golden()
    for name in argv[1:]:
        path = Path(name)
        p = parse(path.read_bytes())
        summary(path.name, p)
        t, shown = 0, 0
        while shown < 4:
            px, nxt = render(p, t)
            print(f"  t={t:5d} next={nxt}  {hexrow(px)}")
            shown += 1
            if nxt is None:
                break
            t = nxt
        if 0 <= p.loop < len(p.frames):
            end = sum(f.duration for f in p.frames)
            px, nxt = render(p, end)
            print(f"  t={end:5d} next={nxt}  {hexrow(px)}  (first frame after wrap)")


if __name__ == "__main__":
    main(sys.argv)
