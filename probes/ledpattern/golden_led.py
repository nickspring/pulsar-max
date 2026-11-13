"""Golden fixture for crates/max-core/src/ledpattern.rs, from ledref.py (synthetic patterns only).

usage: python3 -I golden_led.py [out_file]   (default crates/max-core/tests/fixtures/ledpattern.txt)

Line kinds: case, text (see escape()), error, same <earlier case>, loop, dur, nframes,
frame <k> <ms> <24 hex u32, runs as value*count>, fb <k> <72 bytes>, start <t>,
peek/upd [<t>] <shown frame> <next|-> <finished>, gamma <256>.
"""
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import ledref  # noqa: E402
from ledref import LED_COUNT as N, MAX_SMOOTH, ParseError, Player, frame_index, parse  # noqa: E402

OUT = HERE.parent.parent / "crates/max-core/tests/fixtures/ledpattern.txt"
T0 = 1000
FULL_FRAMES = 64
IDEAL_STEPS = 80


def line(cols, ms):
    return " ".join(cols) + " " + str(ms)


def solid(c, ms):
    return line([c] * N, ms)


def ramp(fmt, ms):
    return line([fmt(k) for k in range(N)], ms)


A, B, C, D = (solid(c, ms) for c, ms in (("101010", 10), ("202020", 20), ("303030", 30), ("404040", 40)))
DISTINCT = ramp(lambda k: "%02x%02x%02x" % (10 * k + 3, 255 - 9 * k, (37 * k) % 256), 100)
DISTINCT2 = ramp(lambda k: "%02x%02x%02x" % ((53 * k) % 256, 7 * k, 255 - 11 * k), 70)
ONE_LIT = line(["ff0000"] + ["000000"] * (N - 1), 10)
COUNT = ramp(lambda k: "%02x0000" % (k + 1), 5)

CASES = {
    "frame_mapping": ramp(lambda k: "%02x%02x%02x" % (k, 2 * k, 3 * k), 100),
    "whitespace_comments": (
        "# comment\r\n\r\n  \t\n\x0b\x0c\n"
        + "\t" + line(["FF00Aa"] + ["000000"] * (N - 1), 10) + " # trailing\r\n"
        + line(["12345678", "#ff0000", "1g2h3i"] + ["0"] * (N - 3), 10) + " x5\n"
        + "#" + solid("ffffff", 10) + "\n"
        + "\x0b".join(["0a0b0c"] * N) + "\x0c007 4294967296\n"
        + "   " + solid("0d0e0f", 4294967295) + "\t\x0b"),
    "color_tokens": line(["f", "fff", "fffff", "123456789a", "gg", "zz12", "ABCDEF", "aBcD",
                          "0é1", "ééff", "-1", "1-2-3-", "0x12", "a\\b", "FfFfFf00",
                          "g0g0g0g0", "9", "99", "999", "9999999", "a", "b", "c", "d"], 3),
    "smooth_split": "smooth 3\n" + solid("000000", 100) + "\n" + solid("ffffff", 100),
    "smooth_remainder": "smooth 2\n" + solid("000000", 5) + "\n" + solid("090909", 10),
    "smooth_fused_rounding": ("smooth 4\n" + solid("ffffff", 5) + "\n" + solid("000000", 5) + "\n"
                              + "smooth 5\n" + solid("ffffff", 12) + "\n"
                              + "gradient 0 ffffff 6 000000 7"),
    "smooth_short_target": ("smooth 4\n" + solid("000000", 10) + "\n" + solid("ffffff", 3) + "\n"
                            + solid("808080", 2)),
    "smooth_zero_ms": "smooth 2\n" + A + "\n" + solid("ffffff", 0) + "\n" + B,
    "smooth_negative": ("smooth -1\n" + A + "\nloop\n" + B + "\n" + A + "\n" + C + "\n" + A + "\n"
                        + solid("101010", 5)),
    "smooth_args": ("smooth 9\nsmooth\nsmooth x\nsmooth20\nsmooth +3\nsmooth 2147483648\n"
                    + "smooth -2147483649\nsmooth 1-\nsmooth -\n" + A + "\n" + B + "\nsmooth 2 7 x\n" + C
                    + "\nsmooth -0\n" + D),
    "smooth_distinct": "smooth 7\n" + DISTINCT + "\n" + DISTINCT2 + "\n" + DISTINCT,
    "smooth_x_byte": ("smooth 3\n" + solid("00000000", 10) + "\n" + solid("01020304", 40) + "\n"
                      + solid("ff00ff80", 9)),
    "smooth_x_byte_no_merge": ("no_size_optimization\nsmooth 3\n" + solid("00000000", 10) + "\n"
                               + solid("01020304", 40)),
    "smooth_cap_ok": ("no_size_optimization\nsmooth %d\n" % MAX_SMOOTH + solid("000000", 3000) + "\n"
                      + solid("ffffff", 1500)),
    "merge_equal": "smooth 7\n" + solid("101010", 100) + "\n" + solid("101010", 50),
    "merge_off": "no_size_optimization\n" + solid("101010", 100) + "\n" + solid("101010", 50),
    "merge_overflow": solid("101010", 4294967295) + "\n" + solid("101010", 4294967295) + "\n" + B,
    "loop_hard_cut": A + "\nloop\n" + B + "\n" + C,
    "loop_smooth_wrap": ("smooth 1\n" + solid("000000", 10) + "\nloop\n" + solid("ffffff", 20) + "\n"
                         + solid("404040", 20)),
    "loop_first": "loop\nsmooth 2\n" + A + "\n" + B + "\n" + C,
    "loop_twice": "loop\n" + A + "\nloop\n" + B + "\n" + C + "\n" + D,
    "loop_at_end_hard": A + "\n" + B + "\nloop",
    "loop_last_frame": "smooth 3\n" + A + "\n" + B + "\nloop\n" + C,
    "loop_single_frame": "smooth 3\nloop\n" + A,
    "loop_merge": A + "\nloop\n" + A + "\n" + B,
    "loop_smooth_off_at_end": "smooth 3\n" + A + "\nloop\n" + B + "\n" + C + "\nsmooth 0",
    "loop_smooth_set_at_end": A + "\nloop\n" + B + "\n" + C + "\nsmooth 2",
    "loop_zero_target": "smooth 4\n" + A + "\nloop\n" + solid("ffffff", 3) + "\n" + C,
    "loop_zero_period": "no_size_optimization\n" + A + "\nloop\n" + solid("202020", 0) + "\n"
                        + solid("303030", 0),
    "loop_all_zero": "loop\n" + solid("202020", 0) + "\n" + solid("303030", 0),
    "zero_frames": "\n".join([solid("010101", 0), solid("020202", 0), solid("030303", 10),
                              solid("040404", 0), solid("050505", 5), solid("060606", 0)]),
    "zero_first_loop": "loop\n" + solid("010101", 0) + "\n" + B,
    "gradient_ring": "gradient 0 000000 4 ff0000 50",
    "gradient_reversed": "gradient 4 ff0000 0 000000 50",
    "gradient_solid": "gradient 0 123456 1 123456 7",
    "gradient_arc": "gradient 5 10203040 17 f0e0d0c0 30",
    "gradient_full_span": "gradient 23 ffffff -0 000000 20",
    "gradient_half": "gradient 0 000000 2 010101 10\ngradient 3 000000 1 030303 10",
    "gradient_trailing": "gradient 0 ff0000 12 0000ff 10 99 extra",
    "gradient_odd_colors": "gradient 1 zz 2 # 5\ngradient 0 1g 23 f 5",
    "gradient_smooth": "smooth 2\ngradient 0 ff0000 12 0000ff 30\ngradient 6 00ff00 18 000000 30",
    "rotate": "smooth 1\nloop\nrotate " + ONE_LIT,
    "rotate_plain": "rotate " + COUNT,
    "rotate_gradient": "rotate gradient 0 ff0000 6 000000 10",
    "rotate_loop_duration": "smooth 2\n" + A + "\nloop\nrotate " + ONE_LIT + "\n" + B,
    "rotate_prefix": "rotate5 " + COUNT,
    "fill": "fill " + COUNT,
    "fill_prefix": "fillx " + COUNT,
    "fill_merge": "fill " + solid("000000", 5),
    "unfill": "unfill " + COUNT,
    "step1": "step1 " + COUNT,
    "step2": "step2 " + COUNT,
    "step1_smooth": "smooth 2\nstep1 " + COUNT,
    "fill_no_merge": "no_size_optimization\nfill " + solid("000000", 5),
    "gamma_values": "gammacorrection\n" + ramp(lambda k: "%02x%02x%02x%02x" % (k, 11 * k, 255 - k, 7 * k), 10),
    "gamma_after_smooth": ("smooth 1\n" + solid("000000", 10) + "\n" + solid("ffffff", 10)
                           + "\ngammacorrection"),
    "gamma_args_and_wrap": "gammacorrection off 0\nsmooth 2\nloop\n" + A + "\n" + solid("ffffff", 30),
    "ignored_lines": ("name x\nnames\nbackground 1\nbackgroundx\nname\n" + A + "\nnamed " + B
                      + "\nbackground\n" + C),
    "err_few_colors": line(["000000"] * (N - 1), 10),
    "err_trailing_number": solid("000000", 10) + " 5",
    "err_ms_suffix": solid("000000", 10) + "ms",
    "err_25_hex": line(["000000"] * (N + 1), 10),
    "err_25_digits": line(["000000"] * N + ["123456"], 10),
    "err_negative_ms": solid("000000", -5),
    "err_plus_ms": line(["000000"] * N, "+5"),
    "err_ms_overflow": solid("000000", 4294967296),
    "err_loop_case": "Loop\n" + A,
    "err_keyword_only": "rotate",
    "err_first_wins": A + "\n" + solid("000000", 10) + " 5\n" + line(["000000"] * (N - 1), 10),
    "err_gradient_same": "gradient 3 ff0000 3 00ff00 10",
    "err_gradient_high": "gradient 0 ff0000 24 00ff00 10",
    "err_gradient_negative": "gradient -1 ff0000 5 00ff00 10",
    "err_gradient_p1_high": "gradient 30 ff0000 5 00ff00 10",
    "err_gradient_p2_negative": "gradient 7 ff0000 -2147483648 00ff00 10",
    "err_gradient_bad_point": "gradient 0 ff0000 x 00ff00 10",
    "err_gradient_no_ms": "gradient 0 ff0000 5 00ff00",
    "err_gradient_short": "gradient 0",
    "err_empty": "",
    "err_only_directives": "smooth 3\nloop\n# " + A + "\nname x\n\n",
    "err_logsmooth": "logsmooth 4\n" + A,
    "err_logsmooth_late": A + "\n" + B + "\nlogsmooth",
    "err_loop_at_end": A + "\n" + B + "\nloop\nsmooth 2",
    "err_loop_at_end_after_loop": "loop\n" + A + "\n" + B + "\nloop\nsmooth 1",
    "err_smooth_cap": "smooth %d\n" % (MAX_SMOOTH + 1) + A + "\n" + B,
    "err_smooth_cap_at_wrap": A + "\nloop\n" + B + "\n" + C + "\nsmooth 2000000000",
    "smooth_cap_unused": "smooth 2000000000\n" + A + "\n" + A,
}

LATE_UPDATES = {
    "loop_hard_cut": [1005, 1009, 1010, 1045, 1061, 1070, 1071, 1081, 1003, 1111, 5000, 5019, 5020],
    "loop_smooth_wrap": [1000, 1035, 1090, 1091, 1500, 1520],
    "smooth_split": [1150, 2000, 2001, 9000],
    "smooth_short_target": [1010, 1013, 1014, 1200],
    "zero_frames": [999, 1000, 1010, 1012, 1015, 1016],
    "merge_overflow": [1000 + 4294967294, 1000 + 8589934588, 1000 + 8589934608],
    "loop_zero_period": [1009, 1010, 1011, 2000],
    "loop_all_zero": [1000, 1001, 1002],
    "loop_zero_target": [1003, 1005, 1013, 1016, 1100],
    "rotate": [1000, 1240, 1250, 1255, 1490, 2000, 2004],
    "loop_single_frame": [1009, 1010, 1031, 1035],
}

FB_CASES = {"frame_mapping", "color_tokens", "gradient_arc", "smooth_distinct", "gamma_values"}

SATURATING_START = {"loop_single_frame": 2**64 - 6}


def escape(text):
    """Control characters as \\n or \\xHH, three or more equal tokens in a row as {token*count}."""
    assert "{" not in text
    lines = []
    for ln in text.split("\n"):
        toks = []
        for t in ln.split(" "):
            plain = t and t.isprintable() and not set(t) & set("\\{}*")
            if plain and toks and toks[-1][0] == t:
                toks[-1][1] += 1
            else:
                toks.append([t, 1])
        parts = []
        for t, k in toks:
            parts += [f"{{{t}*{k}}}"] if k >= 3 else [t] * k
        lines.append(" ".join(parts))
    out = []
    for ch in "\n".join(lines):
        o = ord(ch)
        if ch == "\\":
            out.append("\\\\")
        elif ch == "\n":
            out.append("\\n")
        elif o < 0x20 or o == 0x7F:
            out.append("\\x%02x" % o)
        else:
            out.append(ch)
    return "".join(out)


def unescape(s):
    s = re.sub(r"\{([^{}*]+)\*([0-9]+)\}", lambda m: " ".join([m[1]] * int(m[2])), s)
    return re.sub(r"\\(n|\\|x[0-9a-f]{2})",
                  lambda m: {"n": "\n", "\\": "\\"}.get(m[1]) or chr(int(m[1][1:], 16)), s)


def u32(c):
    return c[0] | c[1] << 8 | c[2] << 16 | c[3] << 24


def row(colors):
    runs = []
    for v in ("%x" % u32(c) for c in colors):
        if runs and runs[-1][0] == v:
            runs[-1][1] += 1
        else:
            runs.append([v, 1])
    return " ".join(v if k == 1 else f"{v}*{k}" for v, k in runs)


def fb(colors):
    return "".join("%02x%02x%02x" % (c[2], c[1], c[0]) for c in reversed(colors))


def opt(v):
    return "-" if v is None else str(v)


def player_line(kind, t, pl, p):
    head = f"{kind} {t} " if t is not None else f"{kind} "
    return head + f"{pl.shown(p)} {opt(pl.next_change(p))} {int(pl.finished(p))}"


def ideal_trace(p, out):
    """Updates exactly when next_change says, as drawLoop does; must agree with frame_index()."""
    pl = Player(T0)
    out.append(f"start {T0}")
    out.append(player_line("peek", None, pl, p))
    for _ in range(min(IDEAL_STEPS, 2 * len(p.frames) + 4)):
        t = pl.next_change(p)
        if t is None:
            break
        pl.update(p, t)
        out.append(player_line("upd", t, pl, p))
        want_i, want_next = frame_index(p, t - T0)
        got_next = pl.next_change(p)
        assert pl.shown(p) == want_i, (t, pl.shown(p), want_i)
        assert got_next == (None if want_next is None else want_next + T0), (t, got_next, want_next)


def custom_trace(p, times, out, start=T0):
    pl = Player(start)
    out.append(f"start {start}")
    out.append(player_line("peek", None, pl, p))
    for t in times:
        pl.update(p, t)
        out.append(player_line("upd", t, pl, p))


def emit_case(name, text, out, seen):
    out.append(f"case {name}")
    out.append("text " + escape(text))
    assert unescape(escape(text)) == text, name
    try:
        p = parse(text)
    except ParseError as e:
        out.append(f"error {e}")
        return
    assert not name.startswith("err_"), name
    loop, n = ledref.valid_loop(p), len(p.frames)
    key = (loop, [(f.duration, f.colors) for f in p.frames])
    twin = next((other for other, k in seen if k == key), None)
    seen.append((name, key))
    if twin is not None:
        out.append(f"same {twin}")
        return
    out.append(f"loop {opt(loop)}")
    out.append(f"dur {sum(f.duration for f in p.frames)}")
    out.append(f"nframes {n}")
    keep = range(n) if n <= FULL_FRAMES else sorted({0, 1, 2, n // 3, n // 2, n - 2, n - 1})
    for k in keep:
        f = p.frames[k]
        out.append(f"frame {k} {f.duration} {row(f.colors)}")
    if name in FB_CASES:
        out.append(f"fb 0 {fb(p.frames[0].colors)}")
        out.append(f"fb {n - 1} {fb(p.frames[-1].colors)}")
    ideal_trace(p, out)
    if name in LATE_UPDATES:
        custom_trace(p, LATE_UPDATES[name], out)
    if name in SATURATING_START:
        s = SATURATING_START[name]
        custom_trace(p, [s + 3, 2**64 - 1, s - 100], out, start=s)


def main(argv):
    ledref.golden()
    out_path = pathlib.Path(argv[1]) if len(argv) > 1 else OUT
    out = ["# generated by probes/ledpattern/golden_led.py from ledref.py",
           "gamma " + " ".join(str(v) for v in ledref.GAMMA)]
    seen = []
    for name, text in CASES.items():
        emit_case(name, text, out, seen)
    out_path.write_text("\n".join(out) + "\n")
    print(f"{out_path}: {len(CASES)} cases, {len(out)} lines")


if __name__ == "__main__":
    main(sys.argv)
