//! Byte-exact with the reference model probes/ledpattern/ledref.py.

pub const LEDS: usize = 24;
/// A parsed colour as io.sdk keeps it: 0xXXBBGGRR (R in the low byte).
pub type Color = u32;

// io.sdk has no limits. Stock files use smooth <= 80 and <= 1783 frames; a broken one could ask for
// 2^31 sub-frames per fade. Caps: 1001 sub-frames per fade, 64 Ki frames (~6.5 MB) per pattern.
const MAX_SMOOTH: i32 = 1000;
const MAX_FRAMES: usize = 1 << 16;

const BAD_FORMAT: &str = "Bad LED pattern format";
const LOOP_AT_END: &str = "loop after the last frame with smooth != 0";
const BLACK: [Color; LEDS] = [0; LEDS];
const GAMMA: [u8; 256] = gamma_lut();

#[derive(Clone, Debug, PartialEq, Eq)]
struct Frame {
    colors: [Color; LEDS],
    ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    frames: Vec<Frame>,
    loop_start: Option<usize>,
}

impl Pattern {
    pub fn parse(text: &str) -> Result<Pattern, String> {
        let mut b = Builder {
            frames: Vec::new(),
            loop_at: None,
            loop_ms: None,
            smooth: 0,
            merge: true,
            gamma: false,
        };
        for line in text.as_bytes().split(|&c| c == b'\n').map(trim) {
            if line.is_empty() || line[0] == b'#' {
                continue;
            }
            if line.starts_with(b"no_size_optimization") {
                b.merge = false;
            } else if line.starts_with(b"loop") {
                b.loop_at = Some(b.frames.len());
            } else if line.starts_with(b"smooth") {
                let mut tokens = Tokens::new(line);
                tokens.word();
                if let Some(n) = tokens.int() {
                    b.smooth = n;
                }
            } else if line.starts_with(b"logsmooth") {
                return Err("logsmooth unsupported".into());
            } else if line.starts_with(b"gammacorrection") {
                b.gamma = true;
            } else if !line.starts_with(b"background") && !line.starts_with(b"name") {
                b.frame_line(line)?;
            }
        }
        b.finish()
    }

    #[cfg(test)]
    fn duration_ms(&self) -> u64 {
        self.frames.iter().map(|f| u64::from(f.ms)).sum()
    }
}

/// Playback state; time is monotonic milliseconds supplied by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Player {
    index: usize,
    frame_start: u64,
}

impl Player {
    pub fn start(now_ms: u64) -> Self {
        Player {
            index: 0,
            frame_start: now_ms,
        }
    }

    /// io.sdk's update(): exact frame boundaries, catch-up over skipped and 0 ms frames, a looped
    /// pattern re-anchors its frame start to `now_ms` on wrap.
    pub fn update(&mut self, pattern: &Pattern, now_ms: u64) {
        let frames = &pattern.frames;
        if self.index >= frames.len() {
            return;
        }
        self.advance(frames, now_ms);
        let Some(start) = pattern.loop_start.filter(|_| self.index >= frames.len()) else {
            return;
        };
        self.index = start;
        self.frame_start = now_ms;
        // io.sdk stops on the loop frame even when it lasts 0 ms and draws it once before the next
        // update; skipping it leaves the same picture without a zero-length wait.
        self.advance(frames, now_ms);
        if self.index >= frames.len() {
            self.index = start;
        }
    }

    fn advance(&mut self, frames: &[Frame], now_ms: u64) {
        let Some(mut elapsed) = now_ms.checked_sub(self.frame_start) else {
            return;
        };
        while let Some(f) = frames.get(self.index)
            && elapsed >= u64::from(f.ms)
        {
            elapsed -= u64::from(f.ms);
            self.frame_start += u64::from(f.ms);
            self.index += 1;
        }
    }

    /// Once finished, the last frame with a nonzero duration: what io.sdk leaves on the ring when it
    /// draws every frame on time, however often this player was updated.
    pub fn colors<'a>(&self, pattern: &'a Pattern) -> &'a [Color; LEDS] {
        let frames = &pattern.frames;
        let shown = if self.finished(pattern) {
            frames
                .iter()
                .rposition(|f| f.ms > 0)
                .unwrap_or(frames.len().saturating_sub(1))
        } else {
            self.index
        };
        frames.get(shown).map_or(&BLACK, |f| &f.colors)
    }

    /// When the shown frame changes next; None once an unlooped pattern has finished (it holds).
    pub fn next_change_ms(&self, pattern: &Pattern) -> Option<u64> {
        let frames = &pattern.frames;
        let frame = frames.get(self.index)?;
        let zero_loop = frame.ms == 0
            && pattern
                .loop_start
                .is_some_and(|l| l <= self.index && frames[l..].iter().all(|f| f.ms == 0));
        (!zero_loop).then(|| self.frame_start.saturating_add(u64::from(frame.ms)))
    }

    pub fn finished(&self, pattern: &Pattern) -> bool {
        self.index >= pattern.frames.len()
    }
}

/// The 72 bytes io.sdk writes to fb5 for one frame: pixel q = column 23 - q, bytes (B, G, R).
pub fn fb_bytes(colors: &[Color; LEDS]) -> [u8; 3 * LEDS] {
    let mut out = [0; 3 * LEDS];
    for (px, c) in out
        .as_chunks_mut::<3>()
        .0
        .iter_mut()
        .zip(colors.iter().rev())
    {
        let [r, g, b, _] = c.to_le_bytes();
        *px = [b, g, r];
    }
    out
}

#[derive(Clone, Copy)]
enum Expand {
    Rotate,
    Step1,
    Step2,
    Fill,
    Unfill,
}

const EXPANSIONS: [(&[u8], Expand); 5] = [
    (b"rotate", Expand::Rotate),
    (b"step1", Expand::Step1),
    (b"step2", Expand::Step2),
    (b"fill", Expand::Fill),
    (b"unfill", Expand::Unfill),
];

struct Builder {
    frames: Vec<Frame>,
    loop_at: Option<usize>,
    loop_ms: Option<u32>,
    smooth: i32,
    merge: bool,
    gamma: bool,
}

impl Builder {
    fn frame_line(&mut self, line: &[u8]) -> Result<(), String> {
        let expand = EXPANSIONS
            .iter()
            .find(|(keyword, _)| line.starts_with(keyword))
            .map(|&(_, e)| e);
        let mut tokens = Tokens::new(line);
        if expand.is_some() {
            tokens.word();
        }
        let frame = parse_frame(&mut tokens)?;
        if self.loop_at == Some(self.frames.len()) {
            self.loop_ms = Some(frame.ms);
        }
        let Some(expand) = expand else {
            return self.add(frame);
        };
        let src = &frame.colors;
        let mut add = |colors| {
            self.add(Frame {
                colors,
                ms: frame.ms,
            })
        };
        match expand {
            Expand::Rotate => (0..LEDS).try_for_each(|k| add(rotated(src, k))),
            Expand::Step1 => {
                add(rotated(&head(src, 1), LEDS - 1))?;
                (0..LEDS - 1).try_for_each(|k| add(rotated(&head(src, k + 2), k)))
            }
            Expand::Fill => (0..LEDS).try_for_each(|k| add(head(src, k + 1))),
            Expand::Step2 => {
                (0..LEDS).try_for_each(|k| add(head(src, k + 1)))?;
                (1..LEDS).try_for_each(|k| add(tail(src, k)))
            }
            Expand::Unfill => (0..LEDS).try_for_each(|k| add(tail(src, k))),
        }
    }

    fn add(&mut self, frame: Frame) -> Result<(), String> {
        let count = self.frames.len();
        self.fade_to(frame)?;
        // `loop` points at the target of the first fade after it, not at its sub-frames.
        if self.loop_at == Some(count) {
            self.loop_at = Some(self.frames.len().saturating_sub(1));
        }
        Ok(())
    }

    fn fade_to(&mut self, frame: Frame) -> Result<(), String> {
        let Some(from) = self.frames.last().map(|f| f.colors) else {
            return self.push(frame);
        };
        if from == frame.colors {
            return self.push(frame);
        }
        if self.smooth < 0 {
            return Ok(());
        }
        if self.smooth > MAX_SMOOTH {
            return Err(format!("smooth {} exceeds {MAX_SMOOTH}", self.smooth));
        }
        let steps = self.smooth.unsigned_abs() + 1;
        let (base, rem) = (frame.ms / steps, frame.ms % steps);
        // Fused as in io.sdk: unfused, a 255 -> 0 fade in 5 steps gives 51 instead of 50 at t = 4/5.
        for k in 0..steps {
            let t = f64::from(k + 1) / f64::from(steps);
            self.push(Frame {
                colors: std::array::from_fn(|i| {
                    blend(from[i], frame.colors[i], |a, d| t.mul_add(d, a).trunc())
                }),
                ms: base + u32::from(k < rem),
            })?;
        }
        Ok(())
    }

    fn push(&mut self, frame: Frame) -> Result<(), String> {
        if self.merge
            && let Some(last) = self.frames.last_mut()
            && last.colors == frame.colors
        {
            last.ms = last.ms.wrapping_add(frame.ms);
            return Ok(());
        }
        if self.frames.len() >= MAX_FRAMES {
            return Err(format!("LED pattern has more than {MAX_FRAMES} frames"));
        }
        self.frames.push(frame);
        Ok(())
    }

    fn finish(mut self) -> Result<Pattern, String> {
        let n = self.frames.len();
        if let Some(l) = self.loop_at
            && n >= 2
            && l != n - 1
            && self.smooth != 0
        {
            // io.sdk abort()s here, or reads past the frames when l == n.
            let (Some(ms), Some(target)) = (self.loop_ms, self.frames.get(l)) else {
                return Err(LOOP_AT_END.into());
            };
            let colors = target.colors;
            self.add(Frame { colors, ms })?;
            self.loop_at = Some(l + 1);
        }
        if self.frames.is_empty() {
            return Err("Empty LED pattern".into());
        }
        if self.gamma {
            for c in self.frames.iter_mut().flat_map(|f| &mut f.colors) {
                *c = Color::from_le_bytes(c.to_le_bytes().map(|v| GAMMA[usize::from(v)]));
            }
        }
        let loop_start = self.loop_at.filter(|&l| l < self.frames.len());
        Ok(Pattern {
            frames: self.frames,
            loop_start,
        })
    }
}

fn parse_frame(tokens: &mut Tokens) -> Result<Frame, String> {
    let first = tokens.word();
    if first == Some(b"gradient") {
        return parse_gradient(tokens);
    }
    let mut colors = [0; LEDS];
    colors[0] = parse_color(first.unwrap_or_default());
    for c in &mut colors[1..] {
        let Some(word) = tokens.word() else { break };
        *c = parse_color(word);
    }
    let ms = tokens.uint().ok_or(BAD_FORMAT)?;
    if tokens.uint().is_some() {
        return Err("Too many tokens in line".into());
    }
    Ok(Frame { colors, ms })
}

fn parse_gradient(tokens: &mut Tokens) -> Result<Frame, String> {
    let p1 = tokens.int();
    let c1 = parse_color(tokens.word().unwrap_or_default());
    let p2 = tokens.int();
    let c2 = parse_color(tokens.word().unwrap_or_default());
    let ms = tokens.uint();
    let (Some(p1), Some(p2), Some(ms)) = (p1, p2, ms) else {
        return Err("Bad gradient format".into());
    };
    if p1 == p2 {
        return Err(format!(
            "Cannot create gradient between two same points: led1 = {p1} led2 = {p2}"
        ));
    }
    let (Ok(i1), Ok(i2)) = (usize::try_from(p1), usize::try_from(p2)) else {
        return Err(gradient_range(p1, p2));
    };
    if i1 >= LEDS || i2 >= LEDS {
        return Err(gradient_range(p1, p2));
    }
    let ((lo, c_lo), (hi, c_hi)) = if i1 <= i2 {
        ((i1, c1), (i2, c2))
    } else {
        ((i2, c2), (i1, c1))
    };
    let (span, arc) = ((hi - lo) as f64, (LEDS - hi + lo) as f64);
    let colors = std::array::from_fn(|j| {
        let t = if (lo..=hi).contains(&j) {
            (j - lo) as f64 / span
        } else {
            ((lo + LEDS - j) % LEDS) as f64 / arc
        };
        blend(c_lo, c_hi, |a, d| d.mul_add(t, a).round())
    });
    Ok(Frame { colors, ms })
}

fn gradient_range(p1: i32, p2: i32) -> String {
    format!("Cannot create gradient between two points: led1 = {p1} led2 = {p2} ledCount = {LEDS}")
}

fn blend(from: Color, to: Color, f: impl Fn(f64, f64) -> f64) -> Color {
    let (a, b) = (from.to_le_bytes(), to.to_le_bytes());
    Color::from_le_bytes(std::array::from_fn(|i| {
        let a = f64::from(a[i]);
        f(a, f64::from(b[i]) - a) as u8
    }))
}

fn rotated(src: &[Color; LEDS], k: usize) -> [Color; LEDS] {
    std::array::from_fn(|j| src[(j + LEDS - k) % LEDS])
}

fn head(src: &[Color; LEDS], lit: usize) -> [Color; LEDS] {
    std::array::from_fn(|j| if j < lit { src[j] } else { 0 })
}

fn tail(src: &[Color; LEDS], dark: usize) -> [Color; LEDS] {
    std::array::from_fn(|j| if j < dark { 0 } else { src[j] })
}

fn parse_color(token: &[u8]) -> Color {
    let mut bytes = [0u8; 4];
    for (byte, pair) in bytes.iter_mut().zip(token.chunks(2)) {
        *byte = pair
            .iter()
            .map_while(|&c| char::from(c).to_digit(16))
            .fold(0, |v, d| v * 16 + d as u8);
    }
    Color::from_le_bytes(bytes)
}

// The set C isspace() accepts; u8::is_ascii_whitespace leaves out \v.
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t'..=b'\r')
}

fn trim(line: &[u8]) -> &[u8] {
    let start = line
        .iter()
        .position(|&c| !is_space(c))
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|&c| !is_space(c))
        .map_or(start, |e| e + 1);
    &line[start..end]
}

struct Tokens<'a> {
    rest: &'a [u8],
    failed: bool,
}

impl<'a> Tokens<'a> {
    fn new(line: &'a [u8]) -> Self {
        Tokens {
            rest: line,
            failed: false,
        }
    }

    fn word(&mut self) -> Option<&'a [u8]> {
        let start = self.rest.iter().position(|&c| !is_space(c));
        let Some(start) = start.filter(|_| !self.failed) else {
            self.failed = true;
            return None;
        };
        let rest = &self.rest[start..];
        let end = rest.iter().position(|&c| is_space(c)).unwrap_or(rest.len());
        let (word, rest) = rest.split_at(end);
        self.rest = rest;
        Some(word)
    }

    fn uint(&mut self) -> Option<u32> {
        let value = self
            .word()
            .and_then(decimal)
            .and_then(|v| u32::try_from(v).ok());
        self.failed |= value.is_none();
        value
    }

    fn int(&mut self) -> Option<i32> {
        let value = self.word().and_then(|w| match w.split_first() {
            Some((b'-', digits)) => decimal(digits).and_then(|v| i32::try_from(-v).ok()),
            _ => decimal(w).and_then(|v| i32::try_from(v).ok()),
        });
        self.failed |= value.is_none();
        value
    }
}

// std::from_chars rules; str::parse would also take a leading '+'.
fn decimal(digits: &[u8]) -> Option<i64> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0i64, |v, &c| {
        let d = char::from(c).to_digit(10)?;
        v.checked_mul(10)?.checked_add(i64::from(d))
    })
}

// ceil(255 * (v / 255)^2.8) as the least k with k^5 * 255^9 >= v^14, independent of libm's pow().
const fn gamma_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    let mut v = 0;
    let mut k: u128 = 0;
    while v < 256 {
        while k.pow(5) * 255u128.pow(9) < (v as u128).pow(14) {
            k += 1;
        }
        lut[v] = k as u8;
        v += 1;
    }
    lut
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn solid(color: &str, ms: u32) -> String {
        format!("{} {ms}\n", [color; LEDS].join(" "))
    }

    fn unescape(s: &str) -> String {
        let mut expanded = String::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            let close = open + rest[open..].find('}').unwrap();
            let (token, count) = rest[open + 1..close].rsplit_once('*').unwrap();
            expanded.push_str(&rest[..open]);
            expanded.push_str(&vec![token; count.parse().unwrap()].join(" "));
            rest = &rest[close + 1..];
        }
        expanded.push_str(rest);
        let mut out = String::new();
        let mut chars = expanded.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    out.push(char::from(u8::from_str_radix(&hex, 16).unwrap()));
                }
                other => panic!("bad escape {other:?}"),
            }
        }
        out
    }

    fn row(s: &str) -> Vec<Color> {
        s.split(' ')
            .flat_map(|run| {
                let (v, k) = run.split_once('*').unwrap_or((run, "1"));
                vec![Color::from_str_radix(v, 16).unwrap(); k.parse().unwrap()]
            })
            .collect()
    }

    fn opt(s: &str) -> Option<u64> {
        (s != "-").then(|| s.parse().unwrap())
    }

    fn check_player(p: &Pattern, player: &Player, want: &[&str], at: &str) {
        let shown: usize = want[0].parse().unwrap();
        assert!(
            std::ptr::eq(player.colors(p), &p.frames[shown].colors),
            "{at}: shown"
        );
        assert_eq!(player.next_change_ms(p), opt(want[1]), "{at}: next");
        assert_eq!(player.finished(p), want[2] == "1", "{at}: finished");
    }

    #[test]
    fn golden_fixture() {
        let mut parsed: HashMap<String, Pattern> = HashMap::new();
        let (mut case, mut result, mut player) = (String::new(), Err(String::new()), None);
        let mut cases = 0;
        for (n, line) in include_str!("../tests/fixtures/ledpattern.txt")
            .lines()
            .enumerate()
        {
            let at = format!("line {} ({case})", n + 1);
            let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
            let args: Vec<&str> = rest.split(' ').collect();
            let p = || result.as_ref().unwrap_or_else(|e| panic!("{at}: {e}"));
            match kind {
                "gamma" => {
                    let want: Vec<u8> = args.iter().map(|v| v.parse().unwrap()).collect();
                    assert_eq!(GAMMA.as_slice(), want);
                    assert_eq!(gamma_lut().as_slice(), want);
                }
                "case" => {
                    case = rest.to_string();
                    cases += 1;
                }
                "text" => {
                    result = Pattern::parse(&unescape(rest));
                    if let Ok(p) = &result {
                        parsed.insert(case.clone(), p.clone());
                    }
                }
                "error" => assert_eq!(
                    result.as_ref().err().map(String::as_str),
                    Some(rest),
                    "{at}"
                ),
                "same" => assert_eq!(p(), &parsed[rest], "{at}"),
                "loop" => assert_eq!(p().loop_start, rest.parse().ok(), "{at}"),
                "dur" => assert_eq!(p().duration_ms(), rest.parse::<u64>().unwrap(), "{at}"),
                "nframes" => assert_eq!(p().frames.len(), rest.parse::<usize>().unwrap(), "{at}"),
                "frame" => {
                    let f = &p().frames[args[0].parse::<usize>().unwrap()];
                    assert_eq!(f.ms, args[1].parse::<u32>().unwrap(), "{at}");
                    assert_eq!(f.colors.as_slice(), row(&args[2..].join(" ")), "{at}");
                }
                "fb" => {
                    let bytes = fb_bytes(&p().frames[args[0].parse::<usize>().unwrap()].colors);
                    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
                    assert_eq!(hex, args[1], "{at}");
                }
                "start" => player = Some(Player::start(rest.parse().unwrap())),
                "peek" => check_player(p(), player.as_ref().unwrap(), &args, &at),
                "upd" => {
                    let pl = player.as_mut().unwrap();
                    pl.update(p(), args[0].parse().unwrap());
                    check_player(p(), pl, &args[1..], &at);
                }
                "#" => {}
                _ => panic!("{at}: unknown line kind {kind}"),
            }
        }
        assert!(cases >= 80, "only {cases} cases");
    }

    #[test]
    fn gamma_is_monotonic_and_keeps_nonzero_lit() {
        assert_eq!((GAMMA[0], GAMMA[1], GAMMA[255]), (0, 1, 255));
        assert!(GAMMA.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn frame_cap() {
        let rotate = format!("rotate {}", solid("000000", 1));
        let at_cap = format!(
            "no_size_optimization\n{}{}",
            rotate.repeat(MAX_FRAMES / LEDS),
            solid("000000", 1).repeat(MAX_FRAMES % LEDS)
        );
        assert_eq!(Pattern::parse(&at_cap).unwrap().frames.len(), MAX_FRAMES);
        let over = at_cap + &solid("000000", 1);
        assert_eq!(
            Pattern::parse(&over).unwrap_err(),
            "LED pattern has more than 65536 frames"
        );
    }

    #[test]
    fn longest_fades_stop_at_the_frame_cap() {
        let fades = (0..67)
            .map(|k| solid(if k % 2 == 0 { "000000" } else { "ffffff" }, 1))
            .collect::<String>();
        let text = |lines: &str| format!("no_size_optimization\nsmooth {MAX_SMOOTH}\n{lines}");
        let fit = Pattern::parse(&text(&fades[..fades.len() / 67 * 66])).unwrap();
        assert_eq!(fit.frames.len(), 1 + 65 * 1001);
        assert!(Pattern::parse(&text(&fades)).is_err());
    }

    #[test]
    fn mismatched_pattern_is_harmless() {
        let short = Pattern::parse(&format!("loop\n{}", solid("010101", 10))).unwrap();
        let many = (1..=9)
            .map(|k| solid(&format!("0{k}0{k}0{k}"), 10))
            .collect::<String>();
        let many = Pattern::parse(&many).unwrap();
        let mut player = Player::start(0);
        player.update(&many, 50);
        player.update(&many, 85);
        assert_eq!(player.colors(&short), &short.frames[0].colors);
        assert_eq!(player.next_change_ms(&short), None);
        assert!(player.finished(&short));
        player.update(&short, 1_000);
        assert_eq!(player.colors(&many), &many.frames[8].colors);
    }

    fn lcg(x: &mut u32) -> u32 {
        *x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *x >> 8
    }

    #[test]
    fn random_text_never_panics_and_next_change_is_ahead() {
        const DIRECTIVES: [&str; 12] = [
            "loop",
            "smooth 1",
            "smooth 3",
            "smooth -1",
            "smooth 0",
            "no_size_optimization",
            "gammacorrection",
            "rotate",
            "step1",
            "step2",
            "fill",
            "unfill",
        ];
        const WORDS: [&str; 8] = [
            "ff00ff",
            "0",
            "12345678",
            "g",
            "4294967295",
            "1",
            "-1",
            "24",
        ];
        let mut seed = 7;
        let mut parsed = 0;
        for _ in 0..2_000 {
            let mut text = String::new();
            for _ in 0..lcg(&mut seed) % 8 {
                let r = lcg(&mut seed);
                match r % 4 {
                    0 => text.push_str(DIRECTIVES[r as usize / 4 % DIRECTIVES.len()]),
                    1 => text.push_str(&format!(
                        "gradient {} {:06x} {} {:06x} {}",
                        r / 4 % 26,
                        r % 0xff_ffff,
                        r / 128 % 26,
                        r / 7 % 0xff_ffff,
                        r / 1024 % 40
                    )),
                    _ => {
                        let colors =
                            (0..LEDS).map(|k| format!("{:06x}", (r >> (k % 20)) & 0x01_0f0f));
                        text.push_str(&format!(
                            "{} {}",
                            colors.collect::<Vec<_>>().join(" "),
                            r % 25
                        ));
                    }
                }
                if r.is_multiple_of(9) {
                    text.push_str(&format!(" {}", WORDS[r as usize / 9 % WORDS.len()]));
                }
                text.push('\n');
            }
            let Ok(p) = Pattern::parse(&text) else {
                continue;
            };
            parsed += 1;
            let mut player = Player::start(5);
            let mut now = 5;
            for _ in 0..30 {
                now += u64::from(lcg(&mut seed) % 40);
                player.update(&p, now);
                assert!(p.frames.iter().any(|f| &f.colors == player.colors(&p)));
                if let Some(next) = player.next_change_ms(&p) {
                    assert!(next > now, "{text:?} at {now}");
                }
            }
        }
        assert!(parsed > 500, "{parsed}");
    }

    #[test]
    fn same_points_are_reported_before_the_range() {
        let e = Pattern::parse("gradient 30 ff0000 30 00ff00 10\n").unwrap_err();
        assert_eq!(
            e,
            "Cannot create gradient between two same points: led1 = 30 led2 = 30"
        );
    }

    #[test]
    fn loop_is_matched_as_a_prefix_like_io_sdk() {
        for directive in ["loop", "loop 3", "loopX"] {
            let text = format!(
                "{}{directive}\n{}",
                solid("010101", 10),
                solid("020202", 10)
            );
            let p = Pattern::parse(&text).unwrap();
            let mut player = Player::start(0);
            player.update(&p, 25);
            assert!(!player.finished(&p), "{directive}");
            assert_eq!(player.colors(&p), &p.frames[1].colors, "{directive}");
        }
    }
}
