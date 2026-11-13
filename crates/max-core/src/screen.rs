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
/// io.sdk rewrites its clock every second even when it looks the same.
const CLOCK_PERIOD_MS: std::ops::RangeInclusive<i64> = 900..=1_100;
const WATCH_BEFORE_MS: i64 = 15;
const WATCH_AFTER_MS: i64 = 25;
pub const FOREIGN_DRAW_MS: i64 = 1_200;
/// Without fresh frames (a capture stall) io.sdk's clock may come back instead of a frozen frame.
const GUARD_FRESH_MS: i64 = 200;

#[derive(Debug, Default)]
pub struct ScreenOwner {
    showing: bool,
    seen: Option<Frame>,
    ours: Option<Frame>,
    clock: Option<Frame>,
    /// The clock the last foreign change replaced: that change may turn out to be an animation's
    /// first frame.
    clock_before: Option<Frame>,
    last_foreign_ms: Option<i64>,
    period_ms: Option<i64>,
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
            self.seen = None;
            self.ours = None;
            self.last_foreign_ms = None;
            self.period_ms = None;
            self.fed_ms = None;
        }
        self.showing = true;
    }

    /// When to watch the screen for io.sdk's next clock rewrite, once its rhythm is known.
    pub fn watch(&self, now_ms: i64) -> Option<std::ops::Range<i64>> {
        let at = self.last_foreign_ms? + self.period_ms?;
        (self.showing && self.ours.is_some() && self.fresh(now_ms))
            .then_some(at - WATCH_BEFORE_MS..at + WATCH_AFTER_MS)
    }

    fn fresh(&self, now_ms: i64) -> bool {
        self.fed_ms.is_some_and(|at| now_ms - at <= GUARD_FRESH_MS)
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
        self.foreign(now_ms, current);
        if now_ms < self.yield_until_ms {
            self.seen = Some(*current);
            return None;
        }
        self.seen = Some(ours);
        Some(ours)
    }

    fn foreign(&mut self, now_ms: i64, current: &Frame) {
        let gap = self.last_foreign_ms.map(|at| now_ms - at);
        let replaced = self.clock_before.take();
        if gap.is_some_and(|g| g < ANIMATION_GAP_MS) {
            self.yield_until_ms = self.yield_until_ms.max(now_ms + ANIMATION_YIELD_MS);
            if replaced.is_some() {
                self.clock = replaced;
            }
        } else if now_ms >= self.yield_until_ms {
            self.clock_before = self.clock.replace(*current);
        }
        self.period_ms = gap.filter(|g| CLOCK_PERIOD_MS.contains(g));
        self.last_foreign_ms = Some(now_ms);
    }

    pub fn foreign_draw(&mut self, now_ms: i64) {
        self.yield_until_ms = self.yield_until_ms.max(now_ms + FOREIGN_DRAW_MS);
    }

    pub fn frame(&mut self, now_ms: i64, current: &Frame, next: &Frame) -> Option<Frame> {
        if !self.showing {
            return None;
        }
        self.fed_ms = Some(now_ms);
        if self.seen.is_some_and(|s| s != *current) {
            self.foreign(now_ms, current);
        }
        if now_ms < self.yield_until_ms {
            self.seen = Some(*current);
            return None;
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
    fn a_minute_tick_is_kept_as_the_new_clock_and_drawn_over() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(s.frame(40, &frame(2), &frame(6)), Some(frame(6)));
        assert_eq!(s.frame(80, &frame(6), &frame(7)), Some(frame(7)));
        assert_eq!(s.stop(120), Some(frame(2)));
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
            s.frame(1_150, &frame(33), &frame(9)),
            None,
            "still within a second"
        );
        assert_eq!(s.frame(1_170, &frame(33), &frame(10)), Some(frame(10)));
        assert_eq!(
            s.stop(1_210),
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
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(s.frame(40, &frame(2), &frame(6)), Some(frame(6)));
        s.foreign_draw(500);
        assert_eq!(s.frame(520, &frame(40), &frame(7)), None);
        assert_eq!(s.frame(560, &frame(41), &frame(8)), None);
        assert_eq!(s.frame(1_800, &frame(41), &frame(9)), Some(frame(9)));
        assert_eq!(s.stop(1_840), Some(frame(2)));
    }

    #[test]
    fn the_clock_rhythm_is_learned_and_its_rewrite_is_undone_at_once() {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        assert_eq!(s.watch(0), None);
        s.frame(0, &frame(1), &frame(5));
        assert_eq!(s.guard(5, &frame(5)), None, "nothing drawn over ours");
        s.frame(200, &frame(5), &frame(5));
        assert_eq!(
            s.guard(227, &frame(1)),
            Some(frame(5)),
            "a tick, before its rhythm is known"
        );
        assert_eq!(s.watch(227), None);
        s.frame(240, &frame(5), &frame(6));
        s.frame(1_200, &frame(6), &frame(6));
        assert_eq!(s.guard(1_228, &frame(2)), Some(frame(6)));
        assert_eq!(s.watch(1_228), Some(2_214..2_254));
        s.frame(2_200, &frame(6), &frame(6));
        assert_eq!(s.guard(2_229, &frame(3)), Some(frame(6)));
        assert_eq!(
            s.stop(2_300),
            Some(frame(3)),
            "the freshest clock comes back"
        );
    }

    fn with_rhythm() -> ScreenOwner {
        let mut s = ScreenOwner::default();
        s.start(&frame(1));
        s.frame(0, &frame(1), &frame(5));
        s.frame(990, &frame(5), &frame(5));
        assert_eq!(s.guard(1_000, &frame(2)), Some(frame(5)));
        s.frame(1_990, &frame(5), &frame(5));
        assert_eq!(s.guard(2_000, &frame(3)), Some(frame(5)));
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
        assert_eq!(s.watch(3_010), Some(3_985..4_025), "still a clock rhythm");
        assert_eq!(s.frame(3_720, &frame(4), &frame(7)), Some(frame(7)));
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
        assert_eq!(s.watch(140), None, "no clock rhythm in an animation");
        let mut k = ScreenOwner::default();
        k.start(&frame(1));
        k.frame(0, &frame(1), &frame(5));
        k.foreign_draw(10);
        assert_eq!(k.guard(20, &frame(40)), None);
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
