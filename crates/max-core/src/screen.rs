//! io.sdk rewrites its clock into the same screen every second, though the picture changes once a
//! minute, and draws its animations at up to 50 fps (docs/max.md, "Светодиоды и экран").

pub const WIDTH: usize = 25;
pub const HEIGHT: usize = 16;
pub const FRAME_BYTES: usize = WIDTH * HEIGHT * 3;

pub type Frame = [u8; FRAME_BYTES];

use crate::player_proto::PlayState;
use crate::spectrogram::{STYLES, Style, pick_random};

/// The HA select: stock "random" by default, as io.sdk's `led.music_equalizer_visualization` auto.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Viz {
    Off,
    Random,
    Fixed(Style),
}

impl Viz {
    pub fn label(self) -> &'static str {
        match self {
            Viz::Off => "Off",
            Viz::Random => "Random",
            Viz::Fixed(Style::Flame) => "Flame",
            Viz::Fixed(Style::WaveCenter) => "Wave",
            Viz::Fixed(Style::Pads) => "Pads",
            Viz::Fixed(Style::BarsBottom) => "Bars",
            Viz::Fixed(Style::BarsCenter) => "Bars center",
            Viz::Fixed(Style::BricksSmall) => "Bricks",
        }
    }

    pub fn all() -> impl Iterator<Item = Viz> {
        [Viz::Off, Viz::Random]
            .into_iter()
            .chain(STYLES.into_iter().map(Viz::Fixed))
    }

    pub fn parse(label: &str) -> Option<Viz> {
        Viz::all().find(|v| v.label() == label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The speaker reference: HA media, drawn as it is captured.
    Speaker,
    /// Sendspin's PCM before it plays, drawn when it plays.
    Sendspin,
}

pub fn screen_source(music: PlayState, sendspin_streaming: bool) -> Option<Source> {
    if sendspin_streaming {
        Some(Source::Sendspin)
    } else if matches!(music, PlayState::Playing | PlayState::Loading) {
        Some(Source::Speaker)
    } else {
        None
    }
}

/// io.sdk's spectrum start; a resume after a pause only unpauses.
pub fn playback_started(prev: PlayState, next: PlayState) -> bool {
    match next {
        PlayState::Loading => true,
        PlayState::Playing => matches!(prev, PlayState::Idle | PlayState::Error),
        PlayState::Idle | PlayState::Paused | PlayState::Error => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenCmd {
    /// `restart`: start the style afresh (filters reset).
    Show {
        style: Style,
        restart: bool,
        source: Source,
    },
    Hide,
}

#[derive(Debug, Default)]
pub struct ScreenChoice {
    shown: Option<(Style, Source)>,
    /// Outlives a hide: io.sdk's "random" never repeats its current style.
    last: Option<Style>,
    /// A playback that began while hidden is still new when the screen shows it.
    restart_pending: bool,
}

impl ScreenChoice {
    /// After a media change; `restart`: a new playback began. None: the screen stays as it is.
    pub fn update(
        &mut self,
        viz: Viz,
        source: Option<Source>,
        restart: bool,
        random: u64,
    ) -> Option<ScreenCmd> {
        let Some(source) = source.filter(|_| viz != Viz::Off) else {
            self.restart_pending |= restart;
            return self.shown.take().map(|_| ScreenCmd::Hide);
        };
        let restart = restart || std::mem::take(&mut self.restart_pending);
        let (style, fresh) = match viz {
            Viz::Fixed(s) => (s, restart || self.last != Some(s)),
            _ => match self.last {
                Some(last) if !restart => (last, false),
                last => (pick_random(last, random), true),
            },
        };
        self.last = Some(style);
        if !fresh && self.shown == Some((style, source)) {
            return None;
        }
        self.shown = Some((style, source));
        Some(ScreenCmd::Show {
            style,
            restart: fresh,
            source,
        })
    }
}

/// A second foreign change this soon after the first is an animation, not a clock tick.
const ANIMATION_GAP_MS: i64 = 200;
const ANIMATION_YIELD_MS: i64 = 1_000;
/// Hardcoded in io.sdk's ClockAnimationBase, which rewrites even an unchanged clock.
const CLOCK_PERIOD_MS: i64 = 1_000;
/// io.sdk's deviceStateHandlerConfig.showClockAfterMs: its animation ends this long after its
/// last frame.
const CLOCK_AFTER_ANIMATION_MS: i64 = 1_000;
const WATCH_BEFORE_MS: i64 = 15;
const WATCH_AFTER_MS: i64 = 25;
const HUNT_MS: i64 = CLOCK_PERIOD_MS + WATCH_AFTER_MS;
/// io.sdk may show a blank EmptyAnimation frame before the clock comes back.
const RETURN_HUNT_MS: i64 = 2_500;
/// io.sdk silent, or rewriting the very bytes we show: a hunt has nothing to catch.
const HUNT_EVERY_MS: i64 = 5_000;
const GRID_STALE_MS: i64 = 5_000;
const FOREIGN_DRAW_MS: i64 = crate::ring::VOLUME_MS as i64;
/// Without fresh frames (a capture stall) io.sdk's clock may come back instead of a frozen frame.
const GUARD_FRESH_MS: i64 = 200;

#[derive(Debug, Default)]
pub struct ScreenOwner {
    showing: bool,
    seen: Option<Frame>,
    ours: Option<Frame>,
    /// io.sdk's clock picture: the frame at start or a catch known to be the clock.
    clock: Option<Frame>,
    /// The clock the last foreign change replaced: that change may turn out to be an animation's
    /// first frame.
    clock_before: Option<Frame>,
    /// A caught frame that was not known as the clock: the clock if it comes again a period later.
    candidate: Option<(i64, Frame)>,
    last_foreign_ms: Option<i64>,
    /// io.sdk keeps this grid across its animations; its clock's return after one is off it.
    tick_ms: Option<i64>,
    grid_confirmed_ms: i64,
    hunt_from_ms: i64,
    hunt_until_ms: i64,
    yield_until_ms: i64,
    fed_ms: Option<i64>,
}

impl ScreenOwner {
    pub fn showing(&self) -> bool {
        self.showing
    }

    pub fn start(&mut self, current: &Frame) {
        if !self.showing {
            self.clock = Some(*current);
            self.candidate = None;
            self.seen = None;
            self.ours = None;
            self.last_foreign_ms = None;
            self.tick_ms = None;
            self.fed_ms = None;
        }
        self.showing = true;
    }

    /// watch(now_ms) spans more than one tick.
    pub fn hunting(&self, now_ms: i64) -> bool {
        self.watch(now_ms)
            .is_some_and(|w| self.grid_window(now_ms) != Some(w))
    }

    /// When to watch for io.sdk's next clock rewrite.
    pub fn watch(&self, now_ms: i64) -> Option<std::ops::Range<i64>> {
        if !self.showing || self.ours.is_none() || !self.fresh(now_ms) {
            return None;
        }
        let windows = [self.grid_window(now_ms), self.hunt_window(now_ms)];
        let open = windows.iter().flatten();
        open.clone()
            .find(|w| w.contains(&now_ms))
            .or_else(|| open.min_by_key(|w| w.start))
            .cloned()
    }

    fn grid(&self, now_ms: i64) -> Option<i64> {
        // io.sdk draws no tick while its animation has the screen.
        let confirmable_ms = self.grid_confirmed_ms.max(self.yield_until_ms);
        self.tick_ms
            .filter(|_| now_ms - confirmable_ms <= GRID_STALE_MS)
    }

    fn grid_window(&self, now_ms: i64) -> Option<std::ops::Range<i64>> {
        let at = Self::next_tick(self.grid(now_ms)?, now_ms.max(self.yield_until_ms));
        Some(at - WATCH_BEFORE_MS..at + WATCH_AFTER_MS)
    }

    fn hunt_window(&self, now_ms: i64) -> Option<std::ops::Range<i64>> {
        if now_ms < self.hunt_until_ms {
            return Some(self.hunt_from_ms..self.hunt_until_ms);
        }
        if self.grid(now_ms).is_some() {
            return None;
        }
        let from = self.hunt_from_ms;
        let mut start = from + (now_ms - from).div_euclid(HUNT_EVERY_MS) * HUNT_EVERY_MS;
        if now_ms >= start + HUNT_MS {
            start += HUNT_EVERY_MS;
        }
        Some(start..start + HUNT_MS)
    }

    fn next_tick(tick: i64, now_ms: i64) -> i64 {
        let ahead = (now_ms - tick - WATCH_AFTER_MS).div_euclid(CLOCK_PERIOD_MS) + 1;
        tick + ahead.max(1) * CLOCK_PERIOD_MS
    }

    fn fresh(&self, now_ms: i64) -> bool {
        self.fed_ms.is_some_and(|at| now_ms - at <= GUARD_FRESH_MS)
    }

    fn hunt(&mut self, from_ms: i64, len_ms: i64) {
        self.hunt_from_ms = from_ms;
        self.hunt_until_ms = from_ms + len_ms;
    }

    fn keep_hunting(&mut self, now_ms: i64) {
        if now_ms >= self.hunt_until_ms {
            self.hunt_from_ms = now_ms;
        }
        self.hunt_until_ms = self.hunt_until_ms.max(now_ms + HUNT_MS);
    }

    fn forget_stale_grid(&mut self, now_ms: i64) {
        if self.tick_ms.is_some() && self.grid(now_ms).is_none() {
            self.tick_ms = None;
            self.keep_hunting(now_ms);
        }
    }

    /// Our last frame again if io.sdk has just drawn over it.
    pub fn guard(&mut self, now_ms: i64, current: &Frame) -> Option<Frame> {
        let ours = self.ours?;
        if !self.showing || !self.fresh(now_ms) || self.seen == Some(*current) {
            return None;
        }
        if ours == *current {
            self.seen = Some(ours);
            return None;
        }
        self.forget_stale_grid(now_ms);
        let due = self.tick_ms.map(|t| Self::next_tick(t, now_ms));
        let on_grid =
            now_ms >= self.yield_until_ms && due.is_some_and(|at| now_ms >= at - WATCH_BEFORE_MS);
        // A blank frame or an animation's first one must not set the phase: only the clock does.
        let returned = self.clock == Some(*current)
            || self.candidate.is_some_and(|(at, f)| {
                f == *current && (now_ms - at - CLOCK_PERIOD_MS).abs() <= WATCH_AFTER_MS
            });
        self.foreign(now_ms, now_ms, current, returned || on_grid);
        if now_ms < self.yield_until_ms {
            self.seen = Some(*current);
            return None;
        }
        if on_grid || (returned && self.tick_ms.is_none()) {
            let at = due.unwrap_or(now_ms);
            // One slow tick on io.sdk's grid must not move the phase past the next window; a drift must.
            self.tick_ms = Some(at.min(now_ms) + (now_ms - at).max(0) / 2);
            self.grid_confirmed_ms = now_ms;
        }
        if returned {
            // A blank frame on the grid must not end the hunt for the clock's return.
            self.hunt_until_ms = self.hunt_until_ms.min(now_ms);
        }
        self.candidate = (!returned && !on_grid).then_some((now_ms, *current));
        self.seen = Some(ours);
        Some(ours)
    }

    fn foreign(&mut self, now_ms: i64, since_ms: i64, current: &Frame, clock: bool) {
        let animating = self
            .last_foreign_ms
            .is_some_and(|at| now_ms - at < ANIMATION_GAP_MS);
        // A stalled draw must not stretch the return over the whole yield.
        let since = since_ms
            .max(self.last_foreign_ms.unwrap_or(since_ms))
            .max(now_ms - GUARD_FRESH_MS);
        let replaced = self.clock_before.take();
        if !animating && now_ms >= self.hunt_from_ms {
            // io.sdk has ended its animation by its own timer: this is the clock or its blank.
            self.yield_until_ms = self.yield_until_ms.min(now_ms);
        }
        let yielding = now_ms < self.yield_until_ms;
        let back = since + CLOCK_AFTER_ANIMATION_MS - WATCH_BEFORE_MS;
        self.last_foreign_ms = Some(now_ms);
        if animating && (yielding || !clock) {
            // io.sdk counts from the frame itself, which a draw sees up to a draw period late.
            self.yield_until_ms = self.yield_until_ms.max(since + ANIMATION_YIELD_MS);
            if replaced.is_some() {
                self.clock = replaced;
            }
            self.candidate = None;
            self.hunt(back, RETURN_HUNT_MS);
            return;
        }
        if yielding && !clock {
            // A still overlay's next frame, or io.sdk's blank: watch again once we draw.
            self.hunt_from_ms = back.min(self.yield_until_ms);
            self.hunt_until_ms = back + RETURN_HUNT_MS;
            return;
        }
        if yielding {
            self.yield_until_ms = now_ms;
            self.hunt_until_ms = now_ms;
        } else if clock {
            self.clock_before = self.clock.replace(*current);
        }
        if self.tick_ms.is_none() {
            self.keep_hunting(now_ms);
        }
    }

    pub fn foreign_draw(&mut self, now_ms: i64) {
        self.candidate = None;
        self.yield_until_ms = self.yield_until_ms.max(now_ms + FOREIGN_DRAW_MS);
        self.hunt(self.yield_until_ms, RETURN_HUNT_MS);
    }

    pub fn frame(&mut self, now_ms: i64, current: &Frame, next: &Frame) -> Option<Frame> {
        if !self.showing {
            return None;
        }
        self.forget_stale_grid(now_ms);
        let since = self.fed_ms.replace(now_ms).unwrap_or(now_ms);
        if self.seen.is_some_and(|s| s != *current) {
            let clock = self.clock == Some(*current);
            self.foreign(now_ms, since, current, clock);
        }
        if now_ms < self.yield_until_ms {
            self.seen = Some(*current);
            return None;
        }
        if self.ours.is_none() {
            self.hunt(now_ms, HUNT_MS);
        }
        self.seen = Some(*next);
        self.ours = Some(*next);
        Some(*next)
    }

    pub fn stop(&mut self, now_ms: i64) -> Option<Frame> {
        if !self.showing {
            return None;
        }
        self.showing = false;
        self.seen = None;
        self.ours = None;
        if now_ms < self.yield_until_ms {
            return None;
        }
        self.clock
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_parses_back_and_random_comes_first_after_off() {
        let labels: Vec<_> = Viz::all().map(Viz::label).collect();
        assert_eq!(labels.len(), 8);
        assert_eq!(&labels[..2], ["Off", "Random"]);
        for v in Viz::all() {
            assert_eq!(Viz::parse(v.label()), Some(v));
        }
        assert_eq!(Viz::parse("nope"), None);
    }

    fn show(style: Style, restart: bool, source: Source) -> Option<ScreenCmd> {
        Some(ScreenCmd::Show {
            style,
            restart,
            source,
        })
    }

    #[test]
    fn the_screen_shows_music_only_while_it_plays_and_the_choice_allows() {
        use Source::{Sendspin, Speaker};
        let mut c = ScreenChoice::default();
        assert_eq!(c.update(Viz::Random, None, true, 0), None);
        assert_eq!(c.update(Viz::Off, Some(Speaker), true, 0), None);
        let pads = Viz::Fixed(Style::Pads);
        assert_eq!(
            c.update(pads, Some(Speaker), false, 0),
            show(Style::Pads, true, Speaker)
        );
        assert_eq!(c.update(pads, Some(Speaker), false, 0), None);
        assert_eq!(
            c.update(pads, Some(Sendspin), false, 0),
            show(Style::Pads, false, Sendspin)
        );
        assert_eq!(
            c.update(Viz::Fixed(Style::Flame), Some(Sendspin), false, 0),
            show(Style::Flame, true, Sendspin)
        );
        assert_eq!(
            c.update(Viz::Off, Some(Sendspin), false, 0),
            Some(ScreenCmd::Hide)
        );
        assert_eq!(c.update(Viz::Off, Some(Sendspin), false, 0), None);
    }

    #[test]
    fn random_keeps_its_style_until_the_next_playback_and_never_repeats_it() {
        for r in 0..30 {
            let mut c = ScreenChoice::default();
            let Some(ScreenCmd::Show {
                style: first,
                restart: true,
                ..
            }) = c.update(Viz::Random, Some(Source::Speaker), true, r)
            else {
                panic!("no fresh show")
            };
            assert_eq!(
                c.update(Viz::Random, Some(Source::Speaker), false, r + 1),
                None
            );
            assert_eq!(c.update(Viz::Random, None, false, 0), Some(ScreenCmd::Hide));
            let Some(ScreenCmd::Show {
                style: next,
                restart: true,
                ..
            }) = c.update(Viz::Random, Some(Source::Sendspin), true, r)
            else {
                panic!("no fresh show")
            };
            assert_ne!(next, first, "r {r}");
        }
    }

    #[test]
    fn a_resume_keeps_the_style_and_its_filters() {
        for viz in [Viz::Random, Viz::Fixed(Style::BricksSmall)] {
            let mut c = ScreenChoice::default();
            let Some(ScreenCmd::Show { style, .. }) = c.update(viz, Some(Source::Speaker), true, 4)
            else {
                panic!("no show")
            };
            assert_eq!(c.update(viz, None, false, 0), Some(ScreenCmd::Hide));
            assert_eq!(
                c.update(viz, Some(Source::Speaker), false, 5),
                show(style, false, Source::Speaker),
                "{viz:?}"
            );
        }
    }

    #[test]
    fn a_playback_begun_while_hidden_starts_its_style_afresh() {
        use Source::Speaker;
        for r in 0..30 {
            let mut c = ScreenChoice::default();
            let Some(ScreenCmd::Show { style: first, .. }) =
                c.update(Viz::Random, Some(Speaker), true, r)
            else {
                panic!("no show")
            };
            assert_eq!(
                c.update(Viz::Off, Some(Speaker), false, 0),
                Some(ScreenCmd::Hide)
            );
            assert_eq!(c.update(Viz::Off, Some(Speaker), true, 0), None);
            assert_eq!(
                c.update(Viz::Off, Some(Speaker), false, 0),
                None,
                "Loading then Playing"
            );
            let Some(ScreenCmd::Show {
                style: next,
                restart: true,
                ..
            }) = c.update(Viz::Random, Some(Speaker), false, r)
            else {
                panic!("r {r}: taken for a resume")
            };
            assert_ne!(next, first, "r {r}");
            assert_eq!(
                c.update(Viz::Fixed(next), Some(Speaker), false, 0),
                None,
                "the new playback restarts once"
            );
        }
        let pads = Viz::Fixed(Style::Pads);
        let mut c = ScreenChoice::default();
        c.update(pads, Some(Speaker), true, 0);
        assert_eq!(
            c.update(Viz::Off, Some(Speaker), true, 0),
            Some(ScreenCmd::Hide)
        );
        assert_eq!(
            c.update(pads, Some(Speaker), false, 0),
            show(Style::Pads, true, Speaker)
        );
    }

    #[test]
    fn only_a_new_media_or_a_start_from_idle_is_a_new_playback() {
        use PlayState::{Error, Idle, Loading, Paused, Playing};
        for prev in [Idle, Loading, Playing, Paused, Error] {
            assert!(playback_started(prev, Loading), "{prev:?}");
            assert!(!playback_started(prev, Paused));
            assert!(!playback_started(prev, Idle));
        }
        assert!(playback_started(Idle, Playing));
        assert!(playback_started(Error, Playing));
        assert!(!playback_started(Paused, Playing), "a resume");
        assert!(!playback_started(Loading, Playing));
        assert!(!playback_started(Playing, Playing));
    }

    #[test]
    fn sendspin_takes_the_screen_over_local_media() {
        use PlayState::{Idle, Loading, Paused, Playing};
        assert_eq!(screen_source(Playing, true), Some(Source::Sendspin));
        assert_eq!(screen_source(Idle, true), Some(Source::Sendspin));
        assert_eq!(screen_source(Playing, false), Some(Source::Speaker));
        assert_eq!(screen_source(Loading, false), Some(Source::Speaker));
        assert_eq!(screen_source(Paused, false), None);
        assert_eq!(screen_source(Idle, false), None);
    }

    fn frame(v: u8) -> Frame {
        [v; FRAME_BYTES]
    }

    #[test]
    fn nothing_is_drawn_until_started() {
        let mut s = ScreenOwner::default();
        assert!(!s.showing());
        assert_eq!(s.frame(0, &frame(0), &frame(9)), None);
        assert_eq!(s.stop(0), None);
    }

    #[test]
    fn music_frames_replace_the_clock_and_the_clock_comes_back() {
        let mut s = ScreenOwner::default();
        let clock = frame(1);
        s.start(&clock);
        assert!(s.showing());
        assert_eq!(s.frame(0, &clock, &frame(5)), Some(frame(5)));
        assert_eq!(s.frame(40, &frame(5), &frame(6)), Some(frame(6)));
        assert_eq!(s.stop(80), Some(clock));
        assert!(!s.showing());
    }

    #[test]
    fn a_new_minute_is_the_clock_once_it_comes_again_a_period_later() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(s.frame(40, &frame(2), &frame(6)), Some(frame(6)));
        s.frame(990, &frame(6), &frame(6));
        assert_eq!(s.guard(1_040, &frame(2)), Some(frame(6)));
        assert!(s.tick_ms.is_none(), "not the saved clock: no phase yet");
        assert!(s.watch(1_040).unwrap().contains(&2_040));
        s.frame(1_990, &frame(6), &frame(6));
        assert_eq!(s.guard(2_040, &frame(2)), Some(frame(6)));
        assert!(s.tick_ms.is_some());
        assert_eq!(s.watch(2_040), Some(3_025..3_065));
        assert_eq!(s.stop(2_100), Some(frame(2)));

        let mut other = ScreenOwner::default();
        other.start(&frame(1));
        other.frame(0, &frame(1), &frame(5));
        other.frame(1_030, &frame(5), &frame(5));
        other.guard(1_040, &frame(2));
        other.frame(2_030, &frame(5), &frame(5));
        assert_eq!(other.guard(2_040, &frame(3)), Some(frame(5)));
        assert!(other.tick_ms.is_none(), "another picture a period later");

        for (at, clock) in [(2_014, false), (2_015, true), (2_065, true), (2_066, false)] {
            let mut s = ScreenOwner::default();
            s.start(&frame(1));
            s.frame(0, &frame(1), &frame(5));
            s.frame(1_030, &frame(5), &frame(5));
            s.guard(1_040, &frame(2));
            s.frame(at - 10, &frame(5), &frame(5));
            assert_eq!(s.guard(at, &frame(2)), Some(frame(5)));
            assert_eq!(s.tick_ms.is_none(), !clock, "at {at}");
        }
    }

    #[test]
    fn an_iosdk_animation_gets_the_screen_until_it_stops() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(
            s.frame(40, &frame(30), &frame(6)),
            Some(frame(6)),
            "one change: a tick"
        );
        assert_eq!(
            s.frame(80, &frame(31), &frame(7)),
            None,
            "another soon after: animating"
        );
        assert_eq!(s.frame(120, &frame(32), &frame(8)), None);
        assert_eq!(s.frame(160, &frame(33), &frame(9)), None);
        assert_eq!(
            s.frame(1_110, &frame(33), &frame(9)),
            None,
            "still within a second of the draw before its last frame"
        );
        assert_eq!(s.frame(1_130, &frame(33), &frame(10)), Some(frame(10)));
        assert_eq!(
            s.stop(1_170),
            Some(frame(1)),
            "animation frames are not the clock"
        );
    }

    #[test]
    fn the_knob_overlay_is_left_alone_and_not_taken_for_the_clock() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        s.foreign_draw(10);
        assert_eq!(s.frame(40, &frame(40), &frame(6)), None);
        assert_eq!(s.stop(100), None, "io.sdk is drawing: leave it");
        s.start(&frame(1));
        assert_eq!(s.frame(1_300, &frame(1), &frame(7)), Some(frame(7)));
        assert_eq!(s.stop(1_340), Some(frame(1)));

        let mut g = ScreenOwner::default();
        g.start(&frame(1));
        g.frame(0, &frame(1), &frame(5));
        g.foreign_draw(10);
        assert_eq!(g.guard(20, &frame(40)), None);
        assert_eq!(g.frame(1_300, &frame(40), &frame(7)), Some(frame(7)));
        assert_eq!(
            g.stop(1_340),
            Some(frame(1)),
            "the overlay is not the clock"
        );
    }

    #[test]
    fn an_overlay_after_a_minute_tick_keeps_that_tick_as_the_clock() {
        let mut s = with_rhythm();
        s.frame(2_990, &frame(5), &frame(5));
        assert_eq!(s.guard(3_000, &frame(4)), Some(frame(5)));
        s.foreign_draw(3_500);
        assert_eq!(s.frame(3_520, &frame(40), &frame(7)), None);
        assert_eq!(s.frame(3_560, &frame(41), &frame(8)), None);
        assert_eq!(s.frame(4_800, &frame(41), &frame(9)), Some(frame(9)));
        assert_eq!(s.stop(4_840), Some(frame(4)));
    }

    #[test]
    fn a_tick_that_starts_an_animation_is_not_kept_as_the_clock() {
        let mut s = with_rhythm();
        s.frame(2_990, &frame(5), &frame(5));
        assert_eq!(s.guard(3_000, &frame(40)), Some(frame(5)), "in rhythm");
        assert_eq!(s.frame(3_020, &frame(41), &frame(6)), None);
        assert_eq!(s.stop(4_100), Some(frame(3)));
    }

    #[test]
    fn only_a_catch_in_the_tick_window_moves_the_grid() {
        for (at, grid) in [(2_984, 2_000), (2_985, 2_985), (3_010, 3_005)] {
            let mut s = with_rhythm();
            s.frame(2_980, &frame(5), &frame(5));
            assert_eq!(s.guard(at, &frame(4)), Some(frame(5)));
            assert_eq!(s.tick_ms, Some(grid), "at {at}");
            assert!(!s.hunting(at), "at {at}");
        }
    }

    #[test]
    fn the_clock_is_hunted_from_our_first_frame_and_then_watched_once_a_second() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        assert_eq!(s.watch(0), None, "nothing of ours to guard yet");
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(
            s.watch(0),
            Some(0..1_025),
            "phase unknown: guard a whole period"
        );
        assert!(s.tick_ms.is_none());
        assert_eq!(s.guard(5, &frame(5)), None, "nothing drawn over ours");
        s.frame(200, &frame(5), &frame(5));
        assert_eq!(s.watch(210), Some(0..1_025));
        assert_eq!(s.guard(227, &frame(1)), Some(frame(5)));
        assert!(s.tick_ms.is_some());
        assert_eq!(
            s.watch(227),
            Some(1_212..1_252),
            "the next second, not this one"
        );
        s.frame(1_200, &frame(5), &frame(6));
        assert_eq!(s.watch(1_212), Some(1_212..1_252));
        assert_eq!(s.guard(1_229, &frame(2)), Some(frame(6)));
        assert_eq!(
            s.watch(1_229),
            Some(2_213..2_253),
            "half way to a late catch"
        );
        s.frame(2_200, &frame(6), &frame(6));
        assert_eq!(s.guard(2_229, &frame(3)), Some(frame(6)));
        assert_eq!(
            s.stop(2_300),
            Some(frame(3)),
            "the freshest clock comes back"
        );
    }

    #[test]
    fn a_missed_tick_moves_the_watch_to_the_next_one() {
        let mut s = with_rhythm();
        s.frame(2_990, &frame(5), &frame(5));
        assert_eq!(s.watch(3_024), Some(2_985..3_025));
        assert_eq!(s.watch(3_025), Some(3_985..4_025));
        s.frame(5_000, &frame(5), &frame(5));
        assert_eq!(s.watch(5_100), Some(5_985..6_025), "two ticks missed");
    }

    #[test]
    fn a_change_seen_only_by_the_draw_keeps_the_grid() {
        let mut s = with_rhythm();
        assert_eq!(s.frame(2_500, &frame(4), &frame(6)), Some(frame(6)));
        assert_eq!(s.watch(2_500), Some(2_985..3_025));
        assert_eq!(s.tick_ms, Some(2_000));
        s.frame(2_990, &frame(6), &frame(6));
        assert_eq!(s.guard(3_000, &frame(4)), Some(frame(6)));
        assert_eq!(s.tick_ms, Some(3_000));
    }

    #[test]
    fn after_an_animation_the_grid_and_the_return_are_watched_until_the_grid_goes_stale() {
        let mut s = with_rhythm();
        s.frame(2_380, &frame(5), &frame(5));
        assert_eq!(
            s.guard(2_400, &frame(40)),
            Some(frame(5)),
            "an animation's first frame is drawn over"
        );
        assert_eq!(s.tick_ms, Some(2_000), "but does not move the grid");
        assert_eq!(s.frame(2_420, &frame(41), &frame(6)), None);
        assert_eq!(
            s.watch(2_420),
            Some(3_385..5_885),
            "from the clock's earliest return, by the guard's look at 2_400"
        );
        assert_eq!(s.frame(3_390, &frame(41), &frame(6)), None);
        assert_eq!(s.frame(3_420, &frame(41), &frame(7)), Some(frame(7)));
        for now in (3_420..5_885).step_by(100) {
            s.frame(now, &frame(7), &frame(7));
            let tick = now.rem_euclid(1_000) < 25;
            let want = if tick {
                now - now.rem_euclid(1_000) - 15..now - now.rem_euclid(1_000) + 25
            } else {
                3_385..5_885
            };
            assert_eq!(s.watch(now), Some(want), "{now}");
            assert_eq!(s.hunting(now), !tick, "{now}");
        }
        s.frame(5_880, &frame(7), &frame(7));
        assert_eq!(s.watch(5_884), Some(3_385..5_885));
        assert_eq!(s.watch(5_885), Some(5_985..6_025), "then only the grid");
        s.frame(7_990, &frame(7), &frame(7));
        assert_eq!(s.watch(8_000), Some(7_985..8_025));
        s.frame(8_400, &frame(7), &frame(7));
        assert_eq!(s.watch(8_400), Some(8_985..9_025));
        s.frame(8_401, &frame(7), &frame(7));
        assert_eq!(
            s.watch(8_401),
            Some(8_401..9_426),
            "five seconds without a tick after the animation's yield"
        );
        assert_eq!(s.stop(9_500), Some(frame(3)));
    }

    #[test]
    fn an_animation_frame_caught_again_a_period_later_is_not_the_clock() {
        let mut s = with_rhythm();
        s.frame(2_380, &frame(5), &frame(5));
        s.guard(2_400, &frame(40));
        s.frame(2_420, &frame(41), &frame(6));
        s.frame(3_390, &frame(41), &frame(6));
        assert_eq!(s.guard(3_400, &frame(40)), Some(frame(5)));
        assert_eq!(s.tick_ms, Some(2_000));
        assert_eq!(
            s.watch(3_400),
            Some(3_385..5_885),
            "the return is still due"
        );

        let mut k = with_rhythm();
        k.frame(2_380, &frame(5), &frame(5));
        k.guard(2_400, &frame(40));
        k.foreign_draw(2_500);
        k.frame(3_390, &frame(5), &frame(6));
        assert_eq!(k.guard(3_400, &frame(40)), None, "still the knob's");
    }

    /// An overlay from 2_400 to 2_600 over the rhythm of with_rhythm(); its last frame is 50.
    fn overlay() -> ScreenOwner {
        let mut s = with_rhythm();
        s.frame(2_380, &frame(5), &frame(5));
        assert_eq!(s.guard(2_400, &frame(40)), Some(frame(5)));
        for (i, now) in (2_420..=2_600).step_by(20).enumerate() {
            assert_eq!(s.frame(now, &frame(41 + i as u8), &frame(6)), None);
            assert_eq!(
                s.watch(now),
                Some(now + 965..now + 3_465),
                "moves with it, from the draw before"
            );
        }
        s
    }

    #[test]
    fn the_clock_back_a_second_after_an_overlay_is_caught_by_the_hunt() {
        for at in [3_570, 3_600, 3_620] {
            let mut s = overlay();
            assert_eq!(s.frame(3_560, &frame(50), &frame(6)), None);
            assert!(!s.watch(3_564).unwrap().contains(&3_564));
            assert_eq!(s.watch(at), Some(3_565..6_065));
            assert!(s.hunting(at));
            assert_eq!(s.guard(at, &frame(3)), Some(frame(5)), "at {at}");
            assert_eq!(s.tick_ms, Some(2_000), "off the grid: at {at}");
            assert_eq!(s.watch(at), Some(3_985..4_025), "at {at}");
            assert!(!s.hunting(at), "at {at}");
            assert_eq!(s.frame(at + 10, &frame(5), &frame(7)), Some(frame(7)));
            assert_eq!(s.stop(at + 20), Some(frame(3)));
        }
    }

    fn draw(s: &mut ScreenOwner, from: i64, to: i64) {
        for now in (from..to).step_by(40) {
            let current = s.seen.unwrap();
            s.frame(now, &current, &frame(7));
        }
    }

    /// Volume overlays on the station, 13 November 2026: after the last frame io.sdk's blank frame
    /// came about a second later and its clock a quarter of a second after that.
    const EVIDENCE: [([i64; 3], i64, i64); 2] = [
        ([2_400, 2_486, 2_641], 3_621, 3_960),
        ([2_400, 2_439, 2_516], 3_481, 3_717),
    ];

    fn overlay_then_blank(overlay: [i64; 3], blank: i64) -> ScreenOwner {
        let mut s = with_rhythm();
        for (i, at) in overlay.into_iter().enumerate() {
            s.frame(at, &frame(40 + i as u8), &frame(6));
        }
        draw(&mut s, overlay[2] + 40, blank);
        assert!(s.watch(blank).unwrap().contains(&blank), "{blank}");
        assert_eq!(s.guard(blank, &frame(0)), Some(frame(7)), "{blank}");
        assert_eq!(
            s.tick_ms,
            Some(2_000),
            "the blank frame does not move the grid"
        );
        assert_eq!(
            s.watch(blank).map(|w| w.end),
            Some(overlay[1] + 3_485),
            "nor cuts the hunt short"
        );
        s
    }

    #[test]
    fn after_an_overlay_the_blank_frame_and_the_later_clock_are_both_caught() {
        for (overlay, blank, clock) in EVIDENCE {
            let mut s = overlay_then_blank(overlay, blank);
            draw(&mut s, blank + 10, clock);
            assert!(s.watch(clock).unwrap().contains(&clock), "{clock}");
            assert_eq!(s.guard(clock, &frame(3)), Some(frame(7)), "{clock}");
            assert_eq!(s.tick_ms, Some(2_000), "{clock}: off the grid");
            assert_eq!(s.watch(clock), Some(3_985..4_025), "the return is over");

            draw(&mut s, clock + 10, 3_990);
            assert_eq!(s.guard(4_000, &frame(3)), Some(frame(7)), "{clock}");
            assert_eq!(s.tick_ms, Some(4_000));
            draw(&mut s, 4_010, blank + 1_000);
            assert_eq!(s.guard(blank + 1_000, &frame(0)), Some(frame(7)));
            assert_eq!(
                s.tick_ms,
                Some(4_000),
                "a blank a period later is still not the clock"
            );
            assert_eq!(s.stop(blank + 1_010), Some(frame(3)));
        }
    }

    #[test]
    fn a_new_minute_after_an_overlay_is_the_clock_once_caught_on_the_grid() {
        for (overlay, blank, clock) in EVIDENCE {
            let mut s = overlay_then_blank(overlay, blank);
            draw(&mut s, blank + 10, clock);
            assert_eq!(s.guard(clock, &frame(4)), Some(frame(7)), "{clock}");
            assert!(
                s.hunting(clock),
                "not the saved clock: the return is still due"
            );
            draw(&mut s, clock + 10, 3_990);
            assert!(s.watch(4_000).unwrap().contains(&4_000));
            assert_eq!(s.guard(4_000, &frame(4)), Some(frame(7)));
            assert_eq!(s.tick_ms, Some(4_000));
            assert_eq!(s.stop(4_010), Some(frame(4)));
        }
    }

    #[test]
    fn a_clock_right_after_the_blank_frame_is_not_taken_for_an_animation() {
        let (overlay, blank, _) = EVIDENCE[0];
        let mut s = overlay_then_blank(overlay, blank);
        s.frame(blank + 140, &frame(7), &frame(7));
        assert_eq!(s.guard(blank + 150, &frame(3)), Some(frame(7)));
        assert_eq!(s.tick_ms, Some(2_000));
        assert!(!s.hunting(blank + 150), "the clock is back");
    }

    #[test]
    fn a_blank_frame_on_the_grid_does_not_end_the_hunt_for_the_return() {
        let mut s = overlay();
        draw(&mut s, 3_600, 3_990);
        assert_eq!(s.guard(4_000, &frame(0)), Some(frame(7)));
        assert_eq!(s.tick_ms, Some(4_000));
        assert_eq!(s.watch(4_030), Some(3_565..6_065));
        draw(&mut s, 4_040, 4_250);
        assert_eq!(s.guard(4_250, &frame(3)), Some(frame(7)));
    }

    /// Knob turns on the station, 13 November 2026, with io.sdk's clock on a grid at .440: the
    /// overlay's first and last frames, the clock's one frame off the grid, the next tick.
    const KNOB: [(i64, i64, i64, i64); 3] = [
        (53_601, 53_762, 54_772, 55_440),
        (58_600, 58_800, 59_780, 60_440),
        (32_120, 32_282, 33_265, 33_440),
    ];

    /// `turns`: each overlay's first and last frames and the clock's return off the grid.
    fn knob_turns(grid: i64, knobs: &[i64], turns: &[(i64, i64, i64)]) -> ScreenOwner {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(grid - 1_500, &frame(1), &frame(7));
        draw(&mut s, grid - 1_460, grid - 1_000);
        assert_eq!(s.guard(grid - 1_000, &frame(1)), Some(frame(7)));
        draw(&mut s, grid - 990, grid);
        assert_eq!(s.guard(grid, &frame(1)), Some(frame(7)));
        assert_eq!(s.tick_ms, Some(grid));

        let mut shown = 40;
        let mut from = grid + 10;
        for &(first, last, back) in turns {
            for now in (from..back).step_by(40) {
                for &knob in knobs.iter().filter(|&&k| now >= k && now - 40 < k) {
                    s.foreign_draw(knob);
                }
                let current = if (first..last + 40).contains(&now) {
                    shown += 1;
                    frame(shown)
                } else {
                    s.seen.unwrap()
                };
                s.frame(now, &current, &frame(7));
            }
            assert!(s.hunting(back), "{back}");
            assert!(s.watch(back).unwrap().contains(&back), "{back}");
            assert_eq!(s.guard(back, &frame(1)), Some(frame(7)), "{back}");
            assert_eq!(s.tick_ms, Some(grid), "{back}: off the grid");
            from = back + 10;
        }
        s
    }

    fn the_next_tick_is_caught_on_the_grid(mut s: ScreenOwner, back: i64, tick: i64) {
        draw(&mut s, back + 10, tick - 10);
        assert_eq!(s.watch(tick - 10), Some(tick - 15..tick + 25), "{tick}");
        assert!(!s.hunting(tick), "{tick}");
        assert_eq!(s.guard(tick, &frame(1)), Some(frame(7)), "{tick}");
        assert_eq!(s.tick_ms, Some(tick));
    }

    #[test]
    fn after_the_knob_the_clock_comes_back_off_its_grid_and_ticks_on_it() {
        for (first, last, back, tick) in KNOB {
            for knob in [first - 5, first + 30] {
                let grid = first - (first - 440).rem_euclid(1_000);
                let s = knob_turns(grid, &[knob], &[(first, last, back)]);
                the_next_tick_is_caught_on_the_grid(s, back, tick);
            }
        }
    }

    #[test]
    fn the_grid_outlives_a_knob_overlay_longer_than_its_stale_limit() {
        let knobs: Vec<i64> = (20_596..24_641).step_by(300).collect();
        let s = knob_turns(20_440, &knobs, &[(20_601, 24_641, 25_641)]);
        the_next_tick_is_caught_on_the_grid(s, 25_641, 26_440);
    }

    #[test]
    fn the_grid_outlives_knob_turns_that_hide_every_tick_past_its_stale_limit() {
        let turns: Vec<_> = (20_601..25_700)
            .step_by(1_250)
            .map(|first| (first, first + 160, first + 1_160))
            .collect();
        let knobs: Vec<i64> = turns.iter().map(|&(first, ..)| first + 30).collect();
        let s = knob_turns(20_440, &knobs, &turns);
        the_next_tick_is_caught_on_the_grid(s, 26_761, 27_440);
    }

    #[test]
    fn a_clock_restarted_off_the_grid_is_learned_again_once_the_grid_goes_stale() {
        let mut s = with_rhythm();
        for at in [2_500, 3_500, 4_500, 5_500, 6_500] {
            draw(&mut s, at - 480, at);
            assert_eq!(s.frame(at, &frame(3), &frame(7)), Some(frame(7)), "{at}");
            assert_eq!(s.watch(at), Some(at + 485..at + 525), "{at}");
        }
        draw(&mut s, 6_520, 7_001);
        assert_eq!(s.tick_ms, Some(2_000));
        s.frame(7_040, &frame(7), &frame(7));
        assert_eq!(s.tick_ms, None);
        assert_eq!(s.watch(7_040), Some(7_040..8_065));
        draw(&mut s, 7_080, 7_500);
        assert_eq!(s.guard(7_500, &frame(3)), Some(frame(7)));
        assert_eq!(s.tick_ms, Some(7_500));
        assert_eq!(s.watch(7_500), Some(8_485..8_525));

        let mut g = with_rhythm();
        draw(&mut g, 2_020, 7_001);
        assert_eq!(g.guard(7_010, &frame(3)), Some(frame(7)));
        assert_eq!(g.tick_ms, Some(7_010), "a stale grid is learned afresh");
    }

    #[test]
    fn the_window_opens_a_draw_early_for_a_last_frame_the_draw_saw_late() {
        let mut s = with_rhythm();
        s.frame(2_380, &frame(5), &frame(5));
        assert_eq!(s.guard(2_400, &frame(40)), Some(frame(5)));
        for (i, now) in (2_440..=2_560).step_by(40).enumerate() {
            s.frame(now, &frame(41 + i as u8), &frame(6));
        }
        assert_eq!(
            s.frame(2_600, &frame(50), &frame(6)),
            None,
            "drawn by io.sdk at 2_570"
        );
        assert_eq!(s.watch(2_600), Some(3_545..6_045));
        assert_eq!(s.frame(3_550, &frame(50), &frame(6)), None);
        assert!(s.watch(3_570).unwrap().contains(&3_570));
        assert_eq!(s.guard(3_570, &frame(3)), Some(frame(5)));
        assert_eq!(s.stop(3_600), Some(frame(3)));

        let mut stalled = with_rhythm();
        stalled.frame(2_380, &frame(5), &frame(5));
        assert_eq!(stalled.guard(2_400, &frame(40)), Some(frame(5)));
        stalled.frame(2_420, &frame(41), &frame(6));
        stalled.foreign_draw(2_850);
        stalled.frame(2_900, &frame(50), &frame(6));
        assert_eq!(
            stalled.watch(2_900),
            Some(3_685..6_185),
            "a stalled draw does not stretch the return over the yield"
        );
    }

    #[test]
    fn a_static_overlay_keeps_its_yield() {
        let mut s = overlay();
        assert_eq!(s.frame(3_340, &frame(50), &frame(6)), None);
        assert_eq!(
            s.frame(3_360, &frame(60), &frame(6)),
            None,
            "760 ms without a frame"
        );
        assert_eq!(s.frame(3_400, &frame(61), &frame(6)), None);
        assert_eq!(s.frame(3_600, &frame(61), &frame(6)), None);
        assert_eq!(s.frame(4_350, &frame(61), &frame(6)), None);
        assert_eq!(s.watch(4_390), Some(4_345..6_845));
        assert_eq!(s.guard(4_395, &frame(3)), Some(frame(5)));
    }

    #[test]
    fn the_clock_back_ends_the_yield_even_when_seen_by_the_draw() {
        let mut s = overlay();
        assert_eq!(
            s.frame(3_100, &frame(3), &frame(8)),
            Some(frame(8)),
            "the saved clock, not an overlay frame"
        );
        assert_eq!(s.tick_ms, Some(2_000));
        assert_eq!(s.watch(3_100), Some(3_985..4_025), "nor ends the grid");

        let mut a = overlay();
        assert_eq!(
            a.frame(2_620, &frame(3), &frame(8)),
            None,
            "within an animation the clock's picture is a frame of it"
        );
    }

    #[test]
    fn the_clock_back_after_the_knob_ends_its_longer_yield() {
        let mut s = with_rhythm();
        s.foreign_draw(2_300);
        assert_eq!(s.frame(2_320, &frame(40), &frame(6)), None);
        assert_eq!(s.frame(2_360, &frame(41), &frame(6)), None);
        assert_eq!(s.frame(3_340, &frame(41), &frame(6)), None);
        assert_eq!(s.watch(3_340), Some(3_305..5_805));
        assert_eq!(s.guard(3_362, &frame(3)), Some(frame(5)));
        assert_eq!(s.tick_ms, Some(2_000));
        assert_eq!(s.frame(3_380, &frame(5), &frame(7)), Some(frame(7)));
        assert_eq!(
            s.stop(3_420),
            Some(frame(3)),
            "the overlay is not the clock"
        );
    }

    #[test]
    fn after_the_knob_overlay_a_new_minute_is_drawn_over_and_caught_again() {
        let mut s = with_rhythm();
        s.foreign_draw(2_300);
        s.frame(2_320, &frame(40), &frame(6));
        s.frame(2_360, &frame(41), &frame(6));
        s.frame(3_300, &frame(41), &frame(6));
        assert_eq!(
            s.guard(3_305, &frame(4)),
            Some(frame(5)),
            "io.sdk's overlay is over by its own timer"
        );
        assert_eq!(s.tick_ms, Some(2_000));
        assert!(s.hunting(3_305));
        assert_eq!(
            s.frame(3_380, &frame(5), &frame(7)),
            Some(frame(7)),
            "and so is the knob's yield"
        );
        s.frame(3_990, &frame(7), &frame(7));
        assert!(s.watch(3_990).unwrap().contains(&4_000));
        assert_eq!(s.guard(4_000, &frame(4)), Some(frame(7)));
        assert_eq!(s.tick_ms, Some(4_000));
        assert_eq!(s.stop(4_100), Some(frame(4)));
    }

    #[test]
    fn after_the_knob_the_clock_is_hunted_again() {
        let mut s = with_rhythm();
        s.foreign_draw(2_100);
        assert_eq!(s.frame(2_200, &frame(5), &frame(6)), None);
        assert_eq!(s.frame(2_980, &frame(5), &frame(6)), None);
        assert_eq!(s.watch(2_985), Some(3_300..5_800), "from the yield's end");
        assert_eq!(s.frame(3_300, &frame(5), &frame(6)), Some(frame(6)));
        assert_eq!(s.watch(3_300), Some(3_300..5_800));
    }

    #[test]
    fn one_late_tick_keeps_the_next_one_on_the_grid_in_the_window() {
        let mut s = with_rhythm();
        s.frame(2_990, &frame(5), &frame(5));
        assert_eq!(s.guard(3_019, &frame(4)), Some(frame(5)));
        let w = s.watch(3_019).unwrap();
        assert!(w.contains(&4_000), "{w:?}");
        s.frame(3_990, &frame(5), &frame(5));
        assert_eq!(s.guard(4_000, &frame(2)), Some(frame(5)));
        assert_eq!(s.watch(4_000), Some(4_985..5_025), "back on the grid");
    }

    #[test]
    fn a_drifting_clock_stays_in_the_window() {
        let mut s = with_rhythm();
        for k in 3..=20 {
            let at = k * 1_000 + (k - 2) * 5;
            s.frame(at - 50, &frame(5), &frame(5));
            assert!(s.watch(at - 50).unwrap().contains(&at), "tick {k}");
            assert_eq!(s.guard(at, &frame(2 + (k % 2) as u8)), Some(frame(5)));
        }
    }

    #[test]
    fn a_hunt_that_catches_nothing_rests_and_retries() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        s.frame(1_000, &frame(5), &frame(5));
        assert_eq!(s.watch(1_024), Some(0..1_025));
        assert_eq!(s.watch(1_025), Some(5_000..6_025));
        s.frame(4_990, &frame(5), &frame(5));
        assert_eq!(s.watch(5_000), Some(5_000..6_025));
        s.frame(6_100, &frame(5), &frame(5));
        assert_eq!(s.watch(6_100), Some(10_000..11_025));
        s.frame(7_000, &frame(2), &frame(5));
        assert_eq!(
            s.watch(7_000),
            Some(7_000..8_025),
            "a change seen by the draw starts a new hunt"
        );
        s.frame(8_025, &frame(3), &frame(5));
        assert_eq!(s.watch(8_025), Some(8_025..9_050), "and so at its end");
    }

    fn with_rhythm() -> ScreenOwner {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        s.frame(990, &frame(5), &frame(5));
        assert_eq!(s.guard(1_000, &frame(1)), Some(frame(5)));
        s.frame(1_990, &frame(5), &frame(5));
        assert_eq!(s.guard(2_000, &frame(3)), Some(frame(5)), "a new minute");
        assert_eq!(s.watch(2_000), Some(2_985..3_025));
        s
    }

    #[test]
    fn an_unchanged_foreign_frame_counts_once_while_yielding() {
        let mut s = with_rhythm();
        s.foreign_draw(2_500);
        assert_eq!(s.frame(2_990, &frame(5), &frame(6)), None);
        for now in 3_000..3_010 {
            assert_eq!(s.guard(now, &frame(4)), None, "yielding");
        }
        assert_eq!(s.watch(3_010), Some(3_700..6_485), "from the yield's end");
        assert_eq!(s.frame(3_720, &frame(4), &frame(7)), Some(frame(7)));
        assert_eq!(
            s.tick_ms,
            Some(2_000),
            "a change met while yielding does not move the grid"
        );
    }

    #[test]
    fn without_new_frames_the_clock_is_left_alone() {
        let mut s = with_rhythm();
        assert_eq!(s.watch(2_300), None);
        assert_eq!(s.guard(3_000, &frame(4)), None);
        assert_eq!(s.frame(3_100, &frame(4), &frame(8)), Some(frame(8)));
        assert!(s.watch(3_100).is_some(), "guarding again with fresh frames");
    }

    #[test]
    fn guarding_yields_to_an_animation_and_to_the_knob() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(s.guard(100, &frame(40)), Some(frame(5)));
        assert_eq!(
            s.guard(140, &frame(41)),
            None,
            "a second change this soon: animating"
        );
        assert_eq!(
            s.watch(140),
            Some(1_125..3_625),
            "no clock rhythm in an animation, only its return"
        );
        let mut k = ScreenOwner::default();
        k.start(&frame(1));
        k.frame(0, &frame(1), &frame(5));
        k.foreign_draw(10);
        assert_eq!(k.guard(20, &frame(40)), None);
    }

    #[test]
    fn a_new_show_hunts_the_clock_again() {
        let mut s = with_rhythm();
        s.stop(2_100);
        s.start(&frame(3));
        s.frame(2_200, &frame(3), &frame(5));
        assert_eq!(s.tick_ms, None);
        assert_eq!(s.watch(2_200), Some(2_200..3_225));

        let mut c = ScreenOwner::default();
        c.start(&frame(1));
        c.frame(0, &frame(1), &frame(5));
        c.guard(100, &frame(2));
        c.stop(200);
        c.start(&frame(1));
        c.frame(1_090, &frame(1), &frame(5));
        assert_eq!(c.guard(1_100, &frame(2)), Some(frame(5)));
        assert!(c.tick_ms.is_none(), "a catch of the last show is forgotten");

        let mut k = ScreenOwner::default();
        k.foreign_draw(0);
        k.start(&frame(1));
        assert_eq!(k.frame(1_190, &frame(40), &frame(5)), None);
        assert_eq!(k.watch(1_250), None, "nothing of ours drawn yet");
    }

    #[test]
    fn a_restart_while_showing_keeps_the_saved_clock() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        s.start(&frame(5));
        assert_eq!(s.stop(40), Some(frame(1)));
    }
}
