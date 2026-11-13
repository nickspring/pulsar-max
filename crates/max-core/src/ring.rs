use crate::dialog::Ui;
use crate::ledpattern::{Color, LEDS, Pattern, Player, fb_bytes};

pub const FILES: [(Role, &str); 6] = [
    (Role::Listening, "alice_listening.led"),
    (Role::Thinking, "alice_thinking.led"),
    (Role::Speaking, "alice_speaking.led"),
    (Role::Error, "error.led"),
    (Role::Timer, "timer.led"),
    (Role::Mute, "mute_mics.led"),
];

// Timings of io.sdk's ring: docs/max.md, "Светодиоды и экран".
pub const ERROR_MS: u64 = 3_000;
pub const MUTE_MS: u64 = 960;
pub const VOLUME_MS: u64 = 1_000;
/// io.sdk draws over us without notice (its alarms, the volume set from HA): a lit frame is
/// written again this often.
pub const REASSERT_MS: u64 = 200;

const BLACK: [Color; LEDS] = [0; LEDS];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Listening,
    Thinking,
    Speaking,
    Error,
    Timer,
    Mute,
}

#[derive(Debug, Default)]
pub struct Patterns {
    pub listening: Option<Pattern>,
    pub thinking: Option<Pattern>,
    pub speaking: Option<Pattern>,
    pub error: Option<Pattern>,
    pub timer: Option<Pattern>,
    pub mute: Option<Pattern>,
}

impl Patterns {
    pub fn set(&mut self, role: Role, pattern: Pattern) {
        let slot = match role {
            Role::Listening => &mut self.listening,
            Role::Thinking => &mut self.thinking,
            Role::Speaking => &mut self.speaking,
            Role::Error => &mut self.error,
            Role::Timer => &mut self.timer,
            Role::Mute => &mut self.mute,
        };
        *slot = Some(pattern);
    }

    fn get(&self, role: Role) -> Option<&Pattern> {
        match role {
            Role::Listening => self.listening.as_ref(),
            Role::Thinking => self.thinking.as_ref(),
            Role::Speaking => self.speaking.as_ref(),
            Role::Error => self.error.as_ref(),
            Role::Timer => self.timer.as_ref(),
            Role::Mute => self.mute.as_ref(),
        }
    }
}

#[derive(Debug)]
struct Background {
    role: Role,
    player: Player,
    start: u64,
}

#[derive(Debug)]
struct Overlay {
    mute: bool,
    player: Player,
    until: u64,
}

#[derive(Debug)]
pub struct Ring {
    patterns: Patterns,
    voice_indication: bool,
    ui: Ui,
    background: Option<Background>,
    error_until: Option<u64>,
    overlay: Option<Overlay>,
    muted: Option<bool>,
    paused_until: Option<u64>,
}

impl Ring {
    pub fn new(patterns: Patterns) -> Self {
        Self {
            patterns,
            voice_indication: true,
            ui: Ui::Idle,
            background: None,
            error_until: None,
            overlay: None,
            muted: None,
            paused_until: None,
        }
    }

    pub fn show(&mut self, ui: Ui, now: u64) {
        self.expire(now);
        if ui == Ui::Error {
            self.ui = Ui::Idle;
            self.error_until = Some(now + ERROR_MS);
            self.background = None;
            self.play(Some(Role::Error), now);
            return;
        }
        self.ui = ui;
        if self.error_until.is_some() && ui == Ui::Idle {
            return;
        }
        self.error_until = None;
        self.play(self.role(), now);
    }

    pub fn set_voice_indication(&mut self, on: bool, now: u64) {
        self.expire(now);
        self.voice_indication = on;
        if self.error_until.is_none() {
            self.play(self.role(), now);
        }
    }

    pub fn set_muted(&mut self, muted: bool, now: u64) {
        self.expire(now);
        if self.muted.replace(muted).is_none_or(|was| was == muted) {
            return;
        }
        self.overlay = Some(Overlay {
            mute: muted,
            player: Player::start(now),
            until: now + MUTE_MS,
        });
        self.defer_unstarted(now);
    }

    /// io.sdk's volume overlay ends in black and does not know our state.
    pub fn foreign_draw(&mut self, now: u64) {
        self.expire(now);
        self.paused_until = Some(now + VOLUME_MS);
        self.defer_unstarted(now);
    }

    pub fn frame(&mut self, now: u64) -> Option<[u8; 3 * LEDS]> {
        self.tick(now);
        if self.paused_until.is_some() {
            return None;
        }
        Some(fb_bytes(self.colors()))
    }

    pub fn next_change(&mut self, now: u64) -> Option<u64> {
        self.expire(now);
        let patterns = &self.patterns;
        let overlay = self.overlay.as_ref().and_then(|o| {
            patterns
                .get(Role::Mute)
                .filter(|_| o.mute)
                .and_then(|p| o.player.next_change_ms(p))
        });
        let background = self.background.as_ref().and_then(|b| {
            if now < b.start {
                return Some(b.start);
            }
            patterns
                .get(b.role)
                .and_then(|p| b.player.next_change_ms(p))
        });
        [
            self.error_until,
            self.paused_until,
            self.overlay.as_ref().map(|o| o.until),
            overlay,
            background,
        ]
        .into_iter()
        .flatten()
        .filter(|&t| t > now)
        .min()
    }

    fn role(&self) -> Option<Role> {
        match self.ui {
            Ui::TimerRing => Some(Role::Timer),
            Ui::Idle | Ui::Error => None,
            _ if !self.voice_indication => None,
            Ui::Listening => Some(Role::Listening),
            Ui::Thinking => Some(Role::Thinking),
            Ui::Speaking => Some(Role::Speaking),
        }
    }

    /// As in io.sdk, a state begun under an overlay starts when nothing hides it any more.
    fn visible_from(&self, now: u64) -> u64 {
        [self.overlay.as_ref().map(|o| o.until), self.paused_until]
            .into_iter()
            .flatten()
            .fold(now, u64::max)
    }

    fn play(&mut self, role: Option<Role>, now: u64) {
        let Some(role) = role.filter(|&r| self.patterns.get(r).is_some()) else {
            self.background = None;
            return;
        };
        if self.background.as_ref().is_some_and(|b| b.role == role) {
            return;
        }
        let start = self.visible_from(now);
        self.background = Some(Background {
            role,
            player: Player::start(start),
            start,
        });
    }

    fn defer_unstarted(&mut self, now: u64) {
        let start = self.visible_from(now);
        if let Some(b) = &mut self.background
            && b.start > now
        {
            b.start = start;
            b.player = Player::start(start);
        }
    }

    fn expire(&mut self, now: u64) {
        if self.paused_until.is_some_and(|t| now >= t) {
            self.paused_until = None;
        }
        if self.overlay.as_ref().is_some_and(|o| now >= o.until) {
            self.overlay = None;
        }
        if self.error_until.is_some_and(|t| now >= t) {
            self.error_until = None;
            self.background = None;
            self.play(self.role(), now);
        }
    }

    /// Hidden players advance too, so a state comes back where it would be, as in io.sdk.
    fn tick(&mut self, now: u64) {
        self.expire(now);
        let patterns = &self.patterns;
        if let Some(b) = &mut self.background
            && now >= b.start
            && let Some(p) = patterns.get(b.role)
        {
            b.player.update(p, now);
        }
        if let Some(o) = &mut self.overlay
            && let Some(p) = patterns.get(Role::Mute)
        {
            o.player.update(p, now);
        }
    }

    fn colors(&self) -> &[Color; LEDS] {
        let patterns = &self.patterns;
        if let Some(o) = &self.overlay {
            return match patterns.get(Role::Mute).filter(|_| o.mute) {
                Some(p) => o.player.colors(p),
                None => &BLACK,
            };
        }
        self.background
            .as_ref()
            .and_then(|b| patterns.get(b.role).map(|p| b.player.colors(p)))
            .unwrap_or(&BLACK)
    }
}

type Frame = [u8; 3 * LEDS];

/// What to write to fb5 and when to look again.
#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    pub write: Option<Frame>,
    pub wake: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Output {
    written: Option<Frame>,
    written_at: u64,
}

impl Output {
    pub fn step(&mut self, ring: &mut Ring, now: u64) -> Step {
        let write = match ring.frame(now) {
            Some(frame) => {
                let due = lit(&frame) && now >= self.written_at + REASSERT_MS;
                (self.written != Some(frame) || due).then(|| {
                    self.written = Some(frame);
                    self.written_at = now;
                    frame
                })
            }
            None => {
                self.written = None;
                None
            }
        };
        let reassert = self
            .written
            .filter(lit)
            .map(|_| self.written_at + REASSERT_MS);
        let wake = [ring.next_change(now), reassert]
            .into_iter()
            .flatten()
            .min();
        Step { write, wake }
    }
}

fn lit(frame: &Frame) -> bool {
    frame.iter().any(|&b| b != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(rgb: &str, ms: u32) -> String {
        format!("{} {ms}\n", vec![rgb; LEDS].join(" "))
    }

    fn pattern(text: &str) -> Pattern {
        Pattern::parse(text).unwrap()
    }

    fn bytes(rgb: [u8; 3]) -> [u8; 3 * LEDS] {
        let [r, g, b] = rgb;
        let mut out = [0; 3 * LEDS];
        for px in out.as_chunks_mut::<3>().0 {
            *px = [b, g, r];
        }
        out
    }

    const LISTEN_A: [u8; 3] = [0x10, 0, 0];
    const LISTEN_B: [u8; 3] = [0x20, 0, 0];
    const THINK_A: [u8; 3] = [0, 0x10, 0];
    const THINK_B: [u8; 3] = [0, 0x20, 0];
    const SPEAK: [u8; 3] = [0, 0, 0x10];
    const ERROR: [u8; 3] = [0x30, 0, 0];
    const TIMER: [u8; 3] = [0, 0x30, 0];
    const MUTE: [u8; 3] = [0x40, 0, 0x40];
    const BLACK_FRAME: [u8; 3 * LEDS] = [0; 3 * LEDS];

    fn patterns() -> Patterns {
        let mut p = Patterns::default();
        p.set(
            Role::Listening,
            pattern(&(solid("100000", 100) + &solid("200000", 100))),
        );
        p.set(
            Role::Thinking,
            pattern(&format!(
                "loop\n{}{}",
                solid("001000", 100),
                solid("002000", 100)
            )),
        );
        p.set(
            Role::Speaking,
            pattern(&format!("loop\n{}", solid("000010", 50))),
        );
        p.set(
            Role::Error,
            pattern(&format!(
                "loop\n{}{}",
                solid("300000", 500),
                solid("310000", 500)
            )),
        );
        p.set(
            Role::Timer,
            pattern(&format!("loop\n{}", solid("003000", 500))),
        );
        p.set(Role::Mute, pattern(&solid("400040", 2_000)));
        p
    }

    fn ring() -> Ring {
        Ring::new(patterns())
    }

    /// The daemon's schedule: a frame at every change up to `to`.
    fn run_to(r: &mut Ring, from: u64, to: u64) -> Option<[u8; 3 * LEDS]> {
        let mut t = from;
        while let Some(next) = r.next_change(t).filter(|&n| n <= to) {
            r.frame(next);
            t = next;
        }
        r.frame(to)
    }

    #[test]
    fn idle_is_black_and_holds() {
        let mut r = ring();
        assert_eq!(r.frame(0), Some(BLACK_FRAME));
        assert_eq!(r.next_change(0), None);
    }

    #[test]
    fn entering_a_state_starts_its_pattern_and_staying_does_not_restart_it() {
        let mut r = ring();
        r.show(Ui::Listening, 1_000);
        assert_eq!(r.frame(1_000), Some(bytes(LISTEN_A)));
        assert_eq!(r.next_change(1_000), Some(1_100));
        assert_eq!(r.frame(1_150), Some(bytes(LISTEN_B)));
        assert_eq!(r.next_change(1_150), Some(1_200));
        assert_eq!(
            r.frame(1_200),
            Some(bytes(LISTEN_B)),
            "an unlooped pattern holds"
        );
        assert_eq!(r.next_change(1_200), None);
        r.show(Ui::Listening, 1_300);
        assert_eq!(r.frame(1_300), Some(bytes(LISTEN_B)));
        r.show(Ui::Thinking, 1_400);
        assert_eq!(r.frame(1_400), Some(bytes(THINK_A)));
        assert_eq!(r.frame(1_550), Some(bytes(THINK_B)));
        assert_eq!(r.frame(1_650), Some(bytes(THINK_A)), "the loop wraps");
        r.show(Ui::Listening, 1_700);
        assert_eq!(r.frame(1_700), Some(bytes(LISTEN_A)));
    }

    #[test]
    fn each_state_has_its_pattern() {
        let mut r = ring();
        for (ui, want) in [
            (Ui::Speaking, SPEAK),
            (Ui::TimerRing, TIMER),
            (Ui::Thinking, THINK_A),
        ] {
            r.show(ui, 0);
            assert_eq!(r.frame(0), Some(bytes(want)), "{ui:?}");
        }
        r.show(Ui::Idle, 0);
        assert_eq!(r.frame(0), Some(BLACK_FRAME));
    }

    #[test]
    fn an_error_outlasts_a_following_idle_then_goes_dark() {
        let mut r = ring();
        r.show(Ui::Listening, 0);
        r.show(Ui::Error, 100);
        r.show(Ui::Idle, 200);
        assert_eq!(run_to(&mut r, 200, 500), Some(bytes(ERROR)));
        assert_eq!(
            run_to(&mut r, 500, 99 + ERROR_MS),
            Some(bytes([0x31, 0, 0]))
        );
        assert_eq!(r.next_change(99 + ERROR_MS), Some(100 + ERROR_MS));
        assert_eq!(r.frame(100 + ERROR_MS), Some(BLACK_FRAME));
        assert_eq!(r.next_change(100 + ERROR_MS), None);
    }

    #[test]
    fn an_error_ends_dark_even_without_an_idle() {
        let mut r = ring();
        r.show(Ui::Listening, 0);
        r.show(Ui::Error, 0);
        assert_eq!(r.frame(ERROR_MS), Some(BLACK_FRAME));
    }

    #[test]
    fn another_state_replaces_the_error_at_once() {
        let mut r = ring();
        r.show(Ui::Error, 0);
        r.show(Ui::TimerRing, 10);
        assert_eq!(r.frame(10), Some(bytes(TIMER)));
        assert_eq!(r.next_change(10), Some(510));
    }

    #[test]
    fn voice_indication_hides_voice_states_but_not_errors_or_the_timer() {
        let mut r = ring();
        r.show(Ui::Speaking, 0);
        r.set_voice_indication(false, 10);
        assert_eq!(r.frame(10), Some(BLACK_FRAME));
        r.show(Ui::Listening, 20);
        assert_eq!(r.frame(20), Some(BLACK_FRAME));
        r.show(Ui::Error, 30);
        assert_eq!(r.frame(30), Some(bytes(ERROR)));
        r.set_voice_indication(true, 40);
        assert_eq!(
            r.frame(40),
            Some(bytes(ERROR)),
            "an error is not cut by the switch"
        );
        r.show(Ui::TimerRing, 50);
        r.set_voice_indication(false, 60);
        assert_eq!(r.frame(60), Some(bytes(TIMER)));
        r.set_voice_indication(true, 70);
        r.show(Ui::Thinking, 80);
        assert_eq!(r.frame(80), Some(bytes(THINK_A)));
    }

    #[test]
    fn muting_plays_the_mute_pattern_over_a_state_that_keeps_running() {
        let mut r = ring();
        r.set_muted(false, 0);
        assert_eq!(
            r.frame(0),
            Some(BLACK_FRAME),
            "the first state plays nothing"
        );
        r.show(Ui::Thinking, 0);
        r.set_muted(true, 150);
        assert_eq!(r.frame(150), Some(bytes(MUTE)));
        assert_eq!(
            run_to(&mut r, 150, 150 + MUTE_MS),
            Some(bytes(THINK_B)),
            "1110 ms into a 200 ms loop, not its restart"
        );
        r.set_muted(true, 2_000);
        assert_eq!(
            r.frame(2_000),
            Some(bytes(THINK_A)),
            "no change, no overlay"
        );
    }

    #[test]
    fn listening_that_ends_under_the_overlay_holds_its_last_frame() {
        let mut r = ring();
        r.set_muted(false, 0);
        r.show(Ui::Listening, 0);
        r.set_muted(true, 50);
        assert_eq!(r.frame(60), Some(bytes(MUTE)));
        assert_eq!(r.frame(50 + MUTE_MS), Some(bytes(LISTEN_B)));
    }

    #[test]
    fn unmuting_keeps_the_ring_dark_while_the_state_runs_on() {
        let mut r = ring();
        r.set_muted(true, 0);
        r.show(Ui::Thinking, 0);
        r.set_muted(false, 150);
        assert_eq!(r.frame(150), Some(BLACK_FRAME));
        assert_eq!(run_to(&mut r, 150, 150 + MUTE_MS - 1), Some(BLACK_FRAME));
        assert_eq!(run_to(&mut r, 150, 150 + MUTE_MS), Some(bytes(THINK_B)));
    }

    #[test]
    fn a_state_entered_under_an_overlay_starts_when_it_ends() {
        let mut r = ring();
        r.set_muted(false, 0);
        r.set_muted(true, 0);
        r.show(Ui::Listening, 500);
        assert_eq!(r.frame(500), Some(bytes(MUTE)));
        assert_eq!(r.frame(MUTE_MS), Some(bytes(LISTEN_A)));
        assert_eq!(r.next_change(MUTE_MS), Some(MUTE_MS + 100));
        assert_eq!(r.frame(MUTE_MS + 100), Some(bytes(LISTEN_B)));
    }

    #[test]
    fn io_sdk_owns_the_ring_while_it_shows_the_volume() {
        let mut r = ring();
        r.show(Ui::Thinking, 0);
        r.foreign_draw(150);
        assert_eq!(r.frame(150), None);
        assert_eq!(run_to(&mut r, 150, 150 + VOLUME_MS - 1), None);
        assert_eq!(run_to(&mut r, 150, 150 + VOLUME_MS), Some(bytes(THINK_B)));
    }

    #[test]
    fn a_state_shown_under_io_sdk_volume_starts_after_it() {
        let mut r = ring();
        r.foreign_draw(0);
        r.show(Ui::Listening, 500);
        assert_eq!(run_to(&mut r, 500, VOLUME_MS), Some(bytes(LISTEN_A)));
        assert_eq!(
            run_to(&mut r, VOLUME_MS, VOLUME_MS + 100),
            Some(bytes(LISTEN_B))
        );
    }

    #[test]
    fn a_second_overlay_before_a_state_shows_moves_its_start() {
        let mut r = ring();
        r.set_muted(false, 0);
        r.set_muted(true, 0);
        r.show(Ui::Listening, 100);
        r.set_muted(false, 800);
        assert_eq!(run_to(&mut r, 100, 800 + MUTE_MS), Some(bytes(LISTEN_A)));
        r.foreign_draw(800 + MUTE_MS + 50);
        assert_eq!(
            run_to(&mut r, 800 + MUTE_MS, 800 + MUTE_MS + 50 + VOLUME_MS),
            Some(bytes(LISTEN_B)),
            "a state already shown keeps running under the volume"
        );
    }

    #[test]
    fn a_state_waiting_for_an_overlay_also_waits_for_io_sdk_volume() {
        let mut r = ring();
        r.set_muted(false, 0);
        r.set_muted(true, 0);
        r.show(Ui::Listening, 100);
        r.foreign_draw(900);
        assert_eq!(run_to(&mut r, 100, 1_900), Some(bytes(LISTEN_A)));
        assert_eq!(run_to(&mut r, 1_900, 2_000), Some(bytes(LISTEN_B)));

        let mut r = ring();
        r.set_muted(false, 0);
        r.set_muted(true, 0);
        r.foreign_draw(500);
        r.show(Ui::Listening, 600);
        assert_eq!(run_to(&mut r, 600, 1_500), Some(bytes(LISTEN_A)));
    }

    #[test]
    fn a_replaced_error_lets_idle_through_and_a_new_error_restarts() {
        let mut r = ring();
        r.show(Ui::Error, 0);
        r.show(Ui::Listening, 100);
        r.show(Ui::Idle, 200);
        assert_eq!(r.frame(200), Some(BLACK_FRAME));
        assert_eq!(r.next_change(200), None);

        r.show(Ui::Error, 1_000);
        assert_eq!(run_to(&mut r, 1_000, 1_600), Some(bytes([0x31, 0, 0])));
        r.show(Ui::Error, 1_600);
        assert_eq!(r.frame(1_600), Some(bytes(ERROR)));
        assert_eq!(r.next_change(1_600), Some(2_100));
    }

    #[test]
    fn output_writes_changes_once_and_dark_frames_only_once() {
        let mut r = ring();
        let mut out = Output::default();
        assert_eq!(
            out.step(&mut r, 0),
            Step {
                write: Some(BLACK_FRAME),
                wake: None
            }
        );
        assert_eq!(
            out.step(&mut r, 5_000),
            Step {
                write: None,
                wake: None
            }
        );
    }

    #[test]
    fn output_rewrites_a_lit_frame_that_did_not_change() {
        let mut r = ring();
        let mut out = Output::default();
        r.show(Ui::Listening, 0);
        assert_eq!(out.step(&mut r, 0).write, Some(bytes(LISTEN_A)));
        let held = out.step(&mut r, 200);
        assert_eq!(held.write, Some(bytes(LISTEN_B)));
        assert_eq!(held.wake, Some(200 + REASSERT_MS));
        assert_eq!(out.step(&mut r, 300).write, None);
        let again = out.step(&mut r, 200 + REASSERT_MS);
        assert_eq!(again.write, Some(bytes(LISTEN_B)));
        assert_eq!(again.wake, Some(200 + 2 * REASSERT_MS));
    }

    #[test]
    fn output_wakes_for_the_next_frame_before_the_reassert() {
        let mut r = ring();
        let mut out = Output::default();
        r.show(Ui::Thinking, 0);
        assert_eq!(
            out.step(&mut r, 0),
            Step {
                write: Some(bytes(THINK_A)),
                wake: Some(100)
            }
        );
    }

    #[test]
    fn output_redraws_after_io_sdk_drew_even_the_same_frame() {
        let mut r = ring();
        let mut out = Output::default();
        r.show(Ui::Speaking, 0);
        assert_eq!(out.step(&mut r, 0).write, Some(bytes(SPEAK)));
        r.foreign_draw(10);
        let paused = out.step(&mut r, 10);
        assert_eq!(paused.write, None);
        assert_eq!(paused.wake, Some(50), "the hidden state keeps its time");
        assert_eq!(out.step(&mut r, 10 + VOLUME_MS).write, Some(bytes(SPEAK)));
    }

    #[test]
    fn a_missing_pattern_leaves_its_state_dark() {
        let mut r = Ring::new(Patterns::default());
        for ui in [Ui::Listening, Ui::Error, Ui::TimerRing] {
            r.show(ui, 0);
            assert_eq!(r.frame(0), Some(BLACK_FRAME), "{ui:?}");
        }
        r.set_muted(false, 0);
        r.set_muted(true, 0);
        assert_eq!(r.frame(0), Some(BLACK_FRAME));
    }

    #[test]
    fn every_role_has_a_stock_file() {
        let mut p = Patterns::default();
        for (role, file) in FILES {
            assert!(file.ends_with(".led"));
            p.set(role, pattern(&solid("010101", 10)));
        }
        for role in [
            Role::Listening,
            Role::Thinking,
            Role::Speaking,
            Role::Error,
            Role::Timer,
            Role::Mute,
        ] {
            assert!(p.get(role).is_some(), "{role:?}");
        }
    }
}
