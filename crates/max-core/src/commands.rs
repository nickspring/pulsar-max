use crate::player_proto::PlayState;

/// HA may end STT of a short "Алиса, громче" before the spotter confirms the word.
pub const COMMAND_AFTER_WAKE_MS: u64 = 2500;

pub const PAUSED_MUSIC_MS: i64 = 2 * 60 * 60 * 1000;

const ALICE_STEPS: i32 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceCommand {
    Stop,
    Pause,
    Resume,
    Louder,
    Quieter,
    Next,
    Prev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MusicAction {
    /// The HA channel if it plays or loads, and Sendspin if it streams: both may apply.
    Pause,
    /// The HA channel if it is paused, else Sendspin.
    Resume,
    /// `step_alice`, unmuting; `click` goes to `want_volume_click` as `allowed`.
    Volume {
        up: bool,
        click: bool,
    },
    /// Sendspin only.
    Next,
    Prev,
}

pub fn command_of(word: &str) -> Option<VoiceCommand> {
    Some(match word {
        "стоп" | "хватит" => VoiceCommand::Stop,
        "пауза" => VoiceCommand::Pause,
        "продолжи" | "играй" => VoiceCommand::Resume,
        "громче" => VoiceCommand::Louder,
        "тише" => VoiceCommand::Quieter,
        "дальше" | "следующий" => VoiceCommand::Next,
        "назад" => VoiceCommand::Prev,
        _ => return None,
    })
}

/// Whole phrase only, so "хватит ли времени" stays a request.
pub fn stt_stop(text: &str) -> bool {
    let text = text.to_lowercase();
    let sep = |c: char| {
        c.is_whitespace()
            || c.is_ascii_punctuation()
            || matches!(c, '«' | '»' | '\u{2010}'..='\u{2027}' | '‹' | '›')
    };
    let mut words = text.split(sep).filter(|w| !w.is_empty()).peekable();
    words.next_if(|w| matches!(*w, "алиса" | "яндекс"));
    words.peek().is_some() && words.all(|w| command_of(w) == Some(VoiceCommand::Stop))
}

/// No arbitration for commands: every station that hears a word acts. Accepted compromise:
/// "хватит ли ..." past the window cancels the dialog.
pub fn command_allowed(
    c: VoiceCommand,
    wake: bool,
    in_dialog: bool,
    sounding: bool,
    owns_music: bool,
) -> bool {
    if wake {
        return true;
    }
    if in_dialog {
        return c == VoiceCommand::Stop;
    }
    match c {
        VoiceCommand::Stop => true,
        VoiceCommand::Louder | VoiceCommand::Quieter => sounding || owns_music,
        VoiceCommand::Pause | VoiceCommand::Resume | VoiceCommand::Next | VoiceCommand::Prev => {
            owns_music
        }
    }
}

pub fn step_alice(v: f32, up: bool) -> f32 {
    grid_step(v, ALICE_STEPS, up)
}

/// Within 1e-3 of a point counts as on it: earlier f32 steps leave ~1e-7 of error.
pub fn grid_step(v: f32, steps: i32, up: bool) -> f32 {
    let p = clamp_unit(v) * steps as f32;
    let point = if up {
        (p + 1e-3).floor() as i32 + 1
    } else {
        (p - 1e-3).ceil() as i32 - 1
    };
    point.clamp(0, steps) as f32 / steps as f32
}

fn clamp_unit(v: f32) -> f32 {
    let v = if 0.0 < v { v } else { 0.0 };
    if v < 1.0 { v } else { 1.0 }
}

/// (volume, muted); whole percent, so f32 drift does not click.
pub fn want_volume_click(before: (f32, bool), after: (f32, bool), allowed: bool) -> bool {
    let percent = |v: f32| (v * 100.0).round() as i32;
    allowed && (percent(before.0), before.1) != (percent(after.0), after.1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportCmd {
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MusicOp {
    PausePlayer,
    ResumePlayer,
    Transport(TransportCmd),
    Unmute,
    SetVolume { volume: f32, click: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MusicState {
    pub music: PlayState,
    pub sendspin_streaming: bool,
    pub volume: Option<f32>,
    pub muted: bool,
}

/// pulsar's music_pause / music_resume / set_volume(feedback) for one voice command.
pub fn music_ops(action: MusicAction, s: &MusicState) -> Vec<MusicOp> {
    let mut ops = Vec::new();
    match action {
        MusicAction::Pause => {
            if matches!(s.music, PlayState::Playing | PlayState::Loading) {
                ops.push(MusicOp::PausePlayer);
            }
            if s.sendspin_streaming {
                ops.push(MusicOp::Transport(TransportCmd::Pause));
            }
        }
        MusicAction::Resume if s.music == PlayState::Paused => ops.push(MusicOp::ResumePlayer),
        MusicAction::Resume => ops.push(MusicOp::Transport(TransportCmd::Play)),
        MusicAction::Volume { up, click } => {
            if s.muted {
                ops.push(MusicOp::Unmute);
            }
            if let Some(v) = s.volume {
                let to = step_alice(v, up);
                ops.push(MusicOp::SetVolume {
                    volume: to,
                    click: want_volume_click((v, s.muted), (to, false), click),
                });
            }
        }
        MusicAction::Next => ops.push(MusicOp::Transport(TransportCmd::Next)),
        MusicAction::Prev => ops.push(MusicOp::Transport(TransportCmd::Previous)),
    }
    ops
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MusicOwnership {
    paused_ms: Option<i64>,
    sendspin_ended_ms: Option<i64>,
}

impl MusicOwnership {
    pub fn on_music_state(&mut self, state: PlayState, now_ms: i64) {
        match state {
            PlayState::Paused => self.paused_ms = Some(now_ms),
            PlayState::Loading | PlayState::Playing => self.sendspin_ended_ms = None,
            PlayState::Idle | PlayState::Error => {}
        }
    }

    /// A stream ended by the HA channel taking the speaker was not paused in Music Assistant.
    pub fn on_sendspin_stream(&mut self, active: bool, music: PlayState, now_ms: i64) {
        let taken_over = matches!(music, PlayState::Playing | PlayState::Loading);
        self.sendspin_ended_ms = (!active && !taken_over).then_some(now_ms);
    }

    pub fn owns(
        &self,
        now_ms: i64,
        music: PlayState,
        sendspin_streaming: bool,
        sendspin_connected: bool,
    ) -> bool {
        owns_music(
            now_ms,
            music,
            self.paused_ms,
            sendspin_streaming,
            sendspin_connected,
            self.sendspin_ended_ms,
        )
    }
}

fn owns_music(
    now: i64,
    music: PlayState,
    music_paused_since: Option<i64>,
    sendspin_streaming: bool,
    sendspin_connected: bool,
    sendspin_ended_since: Option<i64>,
) -> bool {
    let recent = |t: Option<i64>| t.is_some_and(|t| now - t < PAUSED_MUSIC_MS);
    matches!(music, PlayState::Playing | PlayState::Loading)
        || (music == PlayState::Paused && recent(music_paused_since))
        || sendspin_streaming
        || (sendspin_connected && recent(sendspin_ended_since))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::step_volume;

    fn state(music: PlayState, sendspin_streaming: bool) -> MusicState {
        MusicState {
            music,
            sendspin_streaming,
            volume: Some(0.3),
            muted: false,
        }
    }

    #[test]
    fn pause_stops_every_source_that_plays() {
        use MusicOp::*;
        let pause = |music, ss| music_ops(MusicAction::Pause, &state(music, ss));
        assert_eq!(pause(PlayState::Playing, false), [PausePlayer]);
        assert_eq!(pause(PlayState::Loading, false), [PausePlayer]);
        assert_eq!(
            pause(PlayState::Idle, true),
            [Transport(TransportCmd::Pause)]
        );
        assert_eq!(
            pause(PlayState::Playing, true),
            [PausePlayer, Transport(TransportCmd::Pause)]
        );
        assert!(pause(PlayState::Paused, false).is_empty());
    }

    #[test]
    fn resume_prefers_a_paused_ha_channel_and_otherwise_asks_sendspin() {
        use MusicOp::*;
        let resume = |music, ss| music_ops(MusicAction::Resume, &state(music, ss));
        assert_eq!(resume(PlayState::Paused, true), [ResumePlayer]);
        assert_eq!(
            resume(PlayState::Idle, false),
            [Transport(TransportCmd::Play)]
        );
        assert_eq!(
            resume(PlayState::Playing, true),
            [Transport(TransportCmd::Play)]
        );
    }

    #[test]
    fn next_and_previous_go_to_sendspin_only() {
        let s = state(PlayState::Playing, false);
        assert_eq!(
            music_ops(MusicAction::Next, &s),
            [MusicOp::Transport(TransportCmd::Next)]
        );
        assert_eq!(
            music_ops(MusicAction::Prev, &s),
            [MusicOp::Transport(TransportCmd::Previous)]
        );
    }

    #[test]
    fn a_volume_step_unmutes_and_clicks_only_on_a_change_it_may_click_for() {
        use MusicOp::*;
        let step = |volume, muted, up, click| {
            let s = MusicState {
                music: PlayState::Idle,
                sendspin_streaming: false,
                volume,
                muted,
            };
            music_ops(MusicAction::Volume { up, click }, &s)
        };
        let set = |volume: f32, click| SetVolume { volume, click };
        assert_eq!(step(Some(0.3), false, true, true), [set(0.4, true)]);
        assert_eq!(step(Some(0.33), false, false, true), [set(0.3, true)]);
        assert_eq!(
            step(Some(1.0), false, true, true),
            [set(1.0, false)],
            "no change"
        );
        assert_eq!(
            step(Some(1.0), true, true, true),
            [Unmute, set(1.0, true)],
            "unmute"
        );
        assert_eq!(
            step(Some(0.3), false, true, false),
            [set(0.4, false)],
            "not allowed"
        );
        assert_eq!(step(None, true, true, true), [Unmute], "unknown volume");
        assert!(step(None, false, false, true).is_empty());
    }

    #[test]
    fn command_words() {
        use VoiceCommand::*;
        for (w, c) in [
            ("стоп", Stop),
            ("хватит", Stop),
            ("пауза", Pause),
            ("продолжи", Resume),
            ("играй", Resume),
            ("громче", Louder),
            ("тише", Quieter),
            ("дальше", Next),
            ("следующий", Next),
            ("назад", Prev),
        ] {
            assert_eq!(command_of(w), Some(c), "{w}");
        }
        for w in ["", "включи свет", "вверх", "Стоп", "стоп "] {
            assert_eq!(command_of(w), None, "{w:?}");
        }
    }

    #[test]
    fn stt_stop_words() {
        for text in [
            "стоп",
            " Хватит! ",
            "СТОП.",
            "«стоп»",
            "‹стоп›",
            "“Хватит!”",
            "стоп, стоп",
            "стоп-стоп",
            "Хватит, стоп!",
            "Алиса\u{2010}стоп",
            "стоп\u{2027}стоп",
            "Алиса, стоп!",
            "Яндекс хватит…",
            "Алиса—стоп",
            "Яндекс – «стоп».",
            "Алиса, стоп, стоп",
        ] {
            assert!(stt_stop(text), "{text:?}");
        }
        for text in [
            "",
            "…",
            "Алиса",
            "Алиса, Алиса, стоп",
            "Стоп, Алиса",
            "алисастоп",
            "хватит ли времени",
            "не стоп",
            "стопка",
            "сколько стоит стоп кран",
            "стоп-кран",
            "погода",
            "стоп музыку",
            "«стоп музыку»",
            "Алиса—стоп—музыку",
            "«хватит ли времени»",
            "пауза",
            "Алиса, громче",
            "стоп, пауза",
        ] {
            assert!(!stt_stop(text), "{text:?}");
        }
    }

    #[test]
    fn music_ownership() {
        use PlayState as P;
        let now = 10 * PAUSED_MUSIC_MS;
        let own = |m, paused, ss, conn, ended| owns_music(now, m, paused, ss, conn, ended);
        assert!(own(P::Playing, None, false, false, None));
        assert!(own(P::Loading, None, false, false, None));
        assert!(own(P::Idle, None, true, true, None));
        assert!(!own(P::Idle, None, false, true, None));
        assert!(own(
            P::Paused,
            Some(now - PAUSED_MUSIC_MS + 1),
            false,
            false,
            None
        ));
        assert!(!own(
            P::Paused,
            Some(now - PAUSED_MUSIC_MS),
            false,
            false,
            None
        ));
        assert!(!own(P::Paused, None, false, false, None));
        assert!(own(
            P::Idle,
            None,
            false,
            true,
            Some(now - PAUSED_MUSIC_MS + 1)
        ));
        assert!(!own(
            P::Idle,
            None,
            false,
            true,
            Some(now - PAUSED_MUSIC_MS)
        ));
        assert!(!own(P::Idle, None, false, false, Some(now - 1000)));
        assert!(!own(P::Idle, Some(now - 1000), false, false, None));
    }

    #[test]
    fn ha_pause_is_stamped_and_owned_for_two_hours() {
        let mut m = MusicOwnership::default();
        assert!(!m.owns(0, PlayState::Paused, false, false));
        m.on_music_state(PlayState::Playing, 1_000);
        assert!(m.owns(1_000, PlayState::Playing, false, false));
        m.on_music_state(PlayState::Paused, 5_000);
        assert!(m.owns(5_000 + PAUSED_MUSIC_MS - 1, PlayState::Paused, false, false));
        assert!(!m.owns(5_000 + PAUSED_MUSIC_MS, PlayState::Paused, false, false));
        m.on_music_state(PlayState::Idle, 6_000);
        assert!(!m.owns(6_000, PlayState::Idle, false, false));
    }

    #[test]
    fn ended_sendspin_stream_is_owned_while_its_server_stays_connected() {
        let mut m = MusicOwnership::default();
        m.on_sendspin_stream(true, PlayState::Idle, 1_000);
        assert!(m.owns(1_000, PlayState::Idle, true, true));
        m.on_sendspin_stream(false, PlayState::Idle, 2_000);
        assert!(m.owns(2_000 + PAUSED_MUSIC_MS - 1, PlayState::Idle, false, true));
        assert!(!m.owns(2_000 + PAUSED_MUSIC_MS, PlayState::Idle, false, true));
        assert!(!m.owns(3_000, PlayState::Idle, false, false));
        m.on_sendspin_stream(true, PlayState::Idle, 4_000);
        m.on_sendspin_stream(false, PlayState::Paused, 5_000);
        assert!(
            m.owns(5_000, PlayState::Idle, false, true),
            "a paused HA channel"
        );
    }

    #[test]
    fn ha_music_taking_the_speaker_ends_sendspin_ownership_in_either_order() {
        for playing in [PlayState::Playing, PlayState::Loading] {
            let mut m = MusicOwnership::default();
            m.on_sendspin_stream(false, playing, 1_000);
            assert!(!m.owns(1_000, PlayState::Idle, false, true), "{playing:?}");

            let mut m = MusicOwnership::default();
            m.on_sendspin_stream(false, PlayState::Idle, 1_000);
            m.on_music_state(playing, 1_100);
            assert!(!m.owns(1_200, PlayState::Idle, false, true), "{playing:?}");
        }
    }

    #[test]
    fn command_permission() {
        use VoiceCommand::*;
        for c in [Stop, Pause, Resume, Louder, Quieter, Next, Prev] {
            assert!(
                command_allowed(c, true, false, false, false),
                "{c:?} after the wake word"
            );
            assert!(
                command_allowed(c, false, false, false, true),
                "{c:?} with own music"
            );
            assert_eq!(
                command_allowed(c, false, false, false, false),
                c == Stop,
                "{c:?} silent"
            );
            assert_eq!(
                command_allowed(c, false, false, true, false),
                matches!(c, Stop | Louder | Quieter),
                "{c:?} sounding"
            );
            assert_eq!(
                command_allowed(c, false, true, true, true),
                c == Stop,
                "{c:?} in a dialog"
            );
            assert_eq!(
                command_allowed(c, false, true, false, false),
                c == Stop,
                "{c:?} in a silent dialog"
            );
        }
    }

    #[test]
    fn steps_from_a_level_move_exactly_one() {
        for k in 0..=20 {
            let v = k as f32 / 20.0;
            assert_eq!(step_volume(v, true), (k + 1).min(20) as f32 / 20.0);
            assert_eq!(step_volume(v, false), (k - 1).max(0) as f32 / 20.0);
        }
        for k in 0..=10 {
            let v = k as f32 / 10.0;
            assert_eq!(step_alice(v, true), (k + 1).min(10) as f32 / 10.0);
            assert_eq!(step_alice(v, false), (k - 1).max(0) as f32 / 10.0);
        }
    }

    #[test]
    fn steps_from_between_levels_go_to_the_next_level() {
        assert_eq!(step_volume(0.07, false), 0.05);
        assert_eq!(step_volume(0.07, true), 0.1);
        assert_eq!(step_volume(0.33, true), 0.35);
        assert_eq!(step_volume(0.33, false), 0.3);
        assert_eq!(step_alice(0.05, true), 0.1);
        assert_eq!(step_alice(0.15, false), 0.1);
        assert_eq!(step_alice(0.33, false), 0.3);
        assert_eq!(step_alice(0.33, true), 0.4);
        assert_eq!(step_volume(0.150_000_1, false), 0.1);
        assert_eq!(step_volume(0.149_999_9, true), 0.2);
        assert_eq!(step_alice(0.199_999_9, true), 0.3);
    }

    #[test]
    fn steps_down_reach_exact_silence() {
        let mut v = 0.15;
        for _ in 0..3 {
            v = step_volume(v, false);
        }
        assert_eq!(v, 0.0);
        let mut v = 0.3;
        for _ in 0..3 {
            v = step_alice(v, false);
        }
        assert_eq!(v, 0.0);
        assert_eq!(step_volume(1.0, true), 1.0);
        assert_eq!(step_alice(1.0, true), 1.0);
        assert_eq!(step_alice(0.0, false), 0.0);
    }

    #[test]
    fn out_of_range_volumes_step_from_the_nearest_end() {
        assert_eq!(step_alice(f32::NAN, true), 0.1);
        assert_eq!(step_alice(-0.5, true), 0.1);
        assert_eq!(step_alice(1.5, false), 0.9);
        assert_eq!(grid_step(f32::INFINITY, 4, true), 1.0);
    }

    #[test]
    fn volume_click_needs_a_change_in_whole_percent_or_mute() {
        let step = ((0.15, false), (0.1, false));
        assert!(want_volume_click(step.0, step.1, true));
        assert!(!want_volume_click(step.0, step.1, false));
        assert!(!want_volume_click((1.0, false), (1.0, false), true));
        assert!(!want_volume_click((7.45e-9, false), (0.0, false), true));
        assert!(want_volume_click((0.0, true), (0.0, false), true));
    }
}
