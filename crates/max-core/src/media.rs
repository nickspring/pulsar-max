use crate::commands::grid_step;
use crate::player_proto::PlayState;

pub const DUCK_GAIN: f32 = 0.15;
const VOLUME_STEPS: i32 = 20;
/// The player helper polls STREAM_MUSIC every 500 ms.
const VOLUME_CONFIRM_MS: i64 = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Idle,
    Playing,
    Paused,
    Announcing,
}

/// pulsar's rule: a Sendspin stream shows as playing over a paused or idle HA channel.
/// pulsar: Music Assistant taking the speaker stops HA's own music.
pub fn sendspin_stops_music(stream_started: bool, music: PlayState) -> bool {
    stream_started && music != PlayState::Idle
}

pub fn shown_state(announcing: bool, music: PlayState, sendspin: bool) -> Shown {
    if announcing {
        return Shown::Announcing;
    }
    match music {
        PlayState::Playing | PlayState::Loading => Shown::Playing,
        _ if sendspin => Shown::Playing,
        PlayState::Paused => Shown::Paused,
        PlayState::Idle | PlayState::Error => Shown::Idle,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gains {
    pub music: f32,
    pub announce: f32,
}

pub fn channel_gains(muted: bool, ducked: bool) -> Gains {
    if muted {
        return Gains {
            music: 0.0,
            announce: 0.0,
        };
    }
    Gains {
        music: if ducked { DUCK_GAIN } else { 1.0 },
        announce: 1.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeChange {
    pub volume: f32,
    pub settled: bool,
    /// pulsar: a local change (here the knob, through io.sdk) unmutes.
    pub unmute: bool,
}

/// STREAM_MUSIC as the player helper reports it, told apart from the daemon's own requests.
#[derive(Clone, Debug, Default)]
pub struct VolumeSync {
    shown: Option<f32>,
    requested: Option<(f32, i64)>,
}

impl VolumeSync {
    pub fn shown(&self) -> Option<f32> {
        self.shown
    }

    pub fn request(&mut self, volume: f32, now_ms: i64) {
        self.requested = Some((volume, now_ms + VOLUME_CONFIRM_MS));
    }

    /// The helper answers every request, so a report that matches none is a local change.
    pub fn on_report(&mut self, volume: f32, now_ms: i64) -> VolumeChange {
        let changed = self.shown.is_some_and(|s| percent(s) != percent(volume));
        self.shown = Some(volume);
        let own = |settled| VolumeChange {
            volume,
            settled,
            unmute: false,
        };
        if let Some((want, until)) = self.requested {
            if percent(want) == percent(volume) {
                self.requested = None;
                return own(true);
            }
            if now_ms < until {
                return own(false);
            }
            self.requested = None;
        }
        VolumeChange {
            volume,
            settled: true,
            unmute: changed,
        }
    }
}

fn percent(v: f32) -> i32 {
    (v * 100.0).round() as i32
}

pub fn volume_from_index(index: u32, max: u32) -> f32 {
    if max == 0 {
        return 0.0;
    }
    (index.min(max) as f32 / max as f32).clamp(0.0, 1.0)
}

/// pulsar's `step_level` on the 5 % grid.
pub fn step_volume(volume: f32, up: bool) -> f32 {
    grid_step(volume, VOLUME_STEPS, up)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCmd {
    Play,
    Pause,
    Stop,
    Mute,
    Unmute,
    Toggle,
    VolumeUp,
    VolumeDown,
    Other,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaRequest {
    pub command: Option<MediaCmd>,
    pub volume: Option<f32>,
    pub url: Option<String>,
    pub announcement: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MediaAction {
    PlayMusic(String),
    Announce(String),
    SetVolume(f32),
    Resume,
    Pause,
    Stop,
    SetMuted(bool),
    /// Leave the Sendspin group: Music Assistant stops this speaker, the rest of the group plays on.
    LeaveSendspin,
}

/// pulsar's order: url, then volume, then the command. Volume steps need a known volume. While
/// Sendspin streams (`sendspin`), HA's own playback and its pause / stop take the speaker from it.
pub fn media_actions(
    req: MediaRequest,
    music: PlayState,
    volume: Option<f32>,
    sendspin: bool,
) -> Vec<MediaAction> {
    let mut out = Vec::new();
    let leave = |out: &mut Vec<MediaAction>| {
        if sendspin {
            out.push(MediaAction::LeaveSendspin);
        }
    };
    if let Some(url) = req.url {
        if req.announcement {
            out.push(MediaAction::Announce(url));
        } else {
            leave(&mut out);
            out.push(MediaAction::PlayMusic(url));
        }
    }
    let mut volume = volume;
    if let Some(v) = req.volume {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        volume = Some(v);
        out.push(MediaAction::SetVolume(v));
    }
    match req.command {
        Some(MediaCmd::Play) => {
            if music == PlayState::Paused {
                leave(&mut out);
            }
            out.push(MediaAction::Resume);
        }
        Some(MediaCmd::Pause) => {
            if music != PlayState::Playing {
                leave(&mut out);
            }
            out.push(MediaAction::Pause);
        }
        Some(MediaCmd::Toggle) if music == PlayState::Playing => out.push(MediaAction::Pause),
        Some(MediaCmd::Toggle) if sendspin => leave(&mut out),
        Some(MediaCmd::Toggle) => out.push(MediaAction::Resume),
        Some(MediaCmd::Stop) => {
            out.push(MediaAction::Stop);
            leave(&mut out);
        }
        Some(MediaCmd::Mute) => out.push(MediaAction::SetMuted(true)),
        Some(MediaCmd::Unmute) => out.push(MediaAction::SetMuted(false)),
        Some(cmd @ (MediaCmd::VolumeUp | MediaCmd::VolumeDown)) => {
            if let Some(v) = volume {
                out.push(MediaAction::SetVolume(step_volume(
                    v,
                    cmd == MediaCmd::VolumeUp,
                )));
            }
        }
        Some(MediaCmd::Other) | None => {}
    }
    out
}

/// An empty id would make the whole list unplayable.
pub fn announce_urls(preannounce: String, media: String) -> Vec<String> {
    [preannounce, media]
        .into_iter()
        .filter(|u| !u.is_empty())
        .collect()
}

pub const ALERT_RATE: u32 = 16_000;
const ALERT_HZ: f32 = 880.0;
const ALERT_AMPLITUDE: f32 = 0.25;
const ALERT_ROUNDS: usize = 4;

/// Mono s16 WAV: rounds of three 150 ms beeps, 100 ms apart, then 600 ms of silence.
pub fn alert_wav() -> Vec<u8> {
    let ms = |n: u32| (ALERT_RATE / 1000 * n) as usize;
    let mut samples: Vec<i16> = Vec::new();
    for _ in 0..ALERT_ROUNDS {
        for beep in 0..3 {
            let len = ms(150);
            samples.extend((0..len).map(|i| {
                let t = i as f32 / ALERT_RATE as f32;
                let fade = (i.min(len - 1 - i) as f32 / ms(5) as f32).min(1.0);
                let v = (2.0 * std::f32::consts::PI * ALERT_HZ * t).sin();
                (v * fade * ALERT_AMPLITUDE * f32::from(i16::MAX)) as i16
            }));
            let gap = if beep < 2 { ms(100) } else { ms(600) };
            samples.extend(std::iter::repeat_n(0, gap));
        }
    }
    wav_s16_mono(ALERT_RATE, &samples)
}

fn wav_s16_mono(rate: u32, samples: &[i16]) -> Vec<u8> {
    let data_len = u32::try_from(samples.len() * 2).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(command: Option<MediaCmd>) -> MediaRequest {
        MediaRequest {
            command,
            ..MediaRequest::default()
        }
    }

    #[test]
    fn url_then_volume_then_command() {
        let r = MediaRequest {
            command: Some(MediaCmd::Play),
            volume: Some(0.4),
            url: Some("http://m".into()),
            announcement: false,
        };
        assert_eq!(
            media_actions(r, PlayState::Idle, Some(0.2), false),
            [
                MediaAction::PlayMusic("http://m".into()),
                MediaAction::SetVolume(0.4),
                MediaAction::Resume
            ]
        );
    }

    #[test]
    fn announcement_urls_go_to_the_announce_channel() {
        let r = MediaRequest {
            url: Some("http://a".into()),
            announcement: true,
            ..MediaRequest::default()
        };
        assert_eq!(
            media_actions(r, PlayState::Playing, None, true),
            [MediaAction::Announce("http://a".into())]
        );
    }

    #[test]
    fn commands_map_to_player_actions() {
        let one = |cmd, music| media_actions(req(Some(cmd)), music, Some(0.5), false);
        assert_eq!(
            one(MediaCmd::Pause, PlayState::Playing),
            [MediaAction::Pause]
        );
        assert_eq!(one(MediaCmd::Stop, PlayState::Playing), [MediaAction::Stop]);
        assert_eq!(
            one(MediaCmd::Toggle, PlayState::Playing),
            [MediaAction::Pause]
        );
        assert_eq!(
            one(MediaCmd::Toggle, PlayState::Paused),
            [MediaAction::Resume]
        );
        assert_eq!(
            one(MediaCmd::Mute, PlayState::Idle),
            [MediaAction::SetMuted(true)]
        );
        assert_eq!(
            one(MediaCmd::Unmute, PlayState::Idle),
            [MediaAction::SetMuted(false)]
        );
        assert!(one(MediaCmd::Other, PlayState::Idle).is_empty());
        assert!(media_actions(req(None), PlayState::Idle, None, false).is_empty());
    }

    #[test]
    fn volume_steps_need_a_known_volume_and_use_the_requested_one() {
        fn volumes(actions: &[MediaAction]) -> Vec<f32> {
            actions
                .iter()
                .map(|a| match a {
                    MediaAction::SetVolume(v) => *v,
                    other => panic!("unexpected {other:?}"),
                })
                .collect()
        }
        let close = |got: Vec<f32>, want: &[f32]| {
            got.len() == want.len() && got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-6)
        };
        assert!(
            media_actions(req(Some(MediaCmd::VolumeUp)), PlayState::Idle, None, false).is_empty()
        );
        let down = media_actions(
            req(Some(MediaCmd::VolumeDown)),
            PlayState::Idle,
            Some(0.5),
            false,
        );
        assert!(close(volumes(&down), &[0.45]), "{down:?}");
        let between = media_actions(
            req(Some(MediaCmd::VolumeDown)),
            PlayState::Idle,
            Some(0.07),
            false,
        );
        assert!(close(volumes(&between), &[0.05]), "{between:?}");
        let r = MediaRequest {
            command: Some(MediaCmd::VolumeUp),
            volume: Some(f32::NAN),
            ..MediaRequest::default()
        };
        let up = media_actions(r, PlayState::Idle, Some(0.9), false);
        assert!(close(volumes(&up), &[0.0, 0.05]), "{up:?}");
    }

    #[test]
    fn announce_urls_drop_empty_ids() {
        assert_eq!(announce_urls(String::new(), "m".into()), ["m"]);
        assert_eq!(announce_urls("p".into(), "m".into()), ["p", "m"]);
        assert!(announce_urls(String::new(), String::new()).is_empty());
    }

    #[test]
    fn alert_wav_is_a_valid_header_with_beeps_and_silence() {
        let w = alert_wav();
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        let u32_at = |i: usize| u32::from_le_bytes([w[i], w[i + 1], w[i + 2], w[i + 3]]);
        assert_eq!(u32_at(4) as usize, w.len() - 8);
        assert_eq!(u32_at(24), ALERT_RATE);
        assert_eq!(u32_at(40) as usize, w.len() - 44);
        let samples: Vec<i16> = w[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| i16::from_le_bytes(c))
            .collect();
        let round = (ALERT_RATE as usize / 1000) * (3 * 150 + 2 * 100 + 600);
        assert_eq!(samples.len(), ALERT_ROUNDS * round);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(peak > 7000 && peak <= 8192, "peak {peak}");
        let ms = ALERT_RATE as usize / 1000;
        assert!(samples[150 * ms..250 * ms].iter().all(|&s| s == 0));
    }

    #[test]
    fn announcing_wins_over_music() {
        assert_eq!(
            shown_state(true, PlayState::Playing, true),
            Shown::Announcing
        );
        assert_eq!(
            shown_state(false, PlayState::Loading, false),
            Shown::Playing
        );
        assert_eq!(
            shown_state(false, PlayState::Playing, false),
            Shown::Playing
        );
        assert_eq!(shown_state(false, PlayState::Paused, false), Shown::Paused);
        assert_eq!(shown_state(false, PlayState::Error, false), Shown::Idle);
        assert_eq!(shown_state(false, PlayState::Idle, false), Shown::Idle);
    }

    #[test]
    fn sendspin_shows_as_playing_unless_announcing() {
        for music in [PlayState::Idle, PlayState::Paused, PlayState::Error] {
            assert_eq!(shown_state(false, music, true), Shown::Playing, "{music:?}");
        }
        assert_eq!(shown_state(true, PlayState::Idle, true), Shown::Announcing);
    }

    #[test]
    fn ha_playback_and_its_pause_or_stop_take_the_speaker_from_sendspin() {
        let one = |cmd, music| media_actions(req(Some(cmd)), music, Some(0.5), true);
        use MediaAction::*;
        assert_eq!(
            one(MediaCmd::Pause, PlayState::Idle),
            [LeaveSendspin, Pause]
        );
        assert_eq!(one(MediaCmd::Pause, PlayState::Playing), [Pause]);
        assert_eq!(one(MediaCmd::Stop, PlayState::Idle), [Stop, LeaveSendspin]);
        assert_eq!(one(MediaCmd::Toggle, PlayState::Idle), [LeaveSendspin]);
        assert_eq!(one(MediaCmd::Toggle, PlayState::Playing), [Pause]);
        assert_eq!(
            one(MediaCmd::Play, PlayState::Paused),
            [LeaveSendspin, Resume]
        );
        assert_eq!(one(MediaCmd::Play, PlayState::Idle), [Resume]);
        assert_eq!(one(MediaCmd::Mute, PlayState::Idle), [SetMuted(true)]);
        let url = MediaRequest {
            url: Some("http://m".into()),
            ..MediaRequest::default()
        };
        assert_eq!(
            media_actions(url, PlayState::Idle, None, true),
            [LeaveSendspin, PlayMusic("http://m".into())]
        );
    }

    #[test]
    fn gains_follow_mute_then_duck() {
        let g = |music, announce| Gains { music, announce };
        assert_eq!(channel_gains(true, true), g(0.0, 0.0));
        assert_eq!(channel_gains(true, false), g(0.0, 0.0));
        assert_eq!(channel_gains(false, true), g(DUCK_GAIN, 1.0));
        assert_eq!(channel_gains(false, false), g(1.0, 1.0));
    }

    fn change(volume: f32, settled: bool, unmute: bool) -> VolumeChange {
        VolumeChange {
            volume,
            settled,
            unmute,
        }
    }

    #[test]
    fn own_volume_requests_are_reported_and_only_knob_turns_unmute() {
        let mut v = VolumeSync::default();
        assert_eq!(v.shown(), None);
        assert_eq!(v.on_report(0.26, 0), change(0.26, true, false));
        assert_eq!(v.on_report(0.3, 20), change(0.3, true, true));
        v.request(0.3, 100);
        assert_eq!(
            v.on_report(0.3, 110),
            change(0.3, true, false),
            "unchanged request"
        );
        assert_eq!(
            v.on_report(0.31, 300),
            change(0.31, true, true),
            "knob right after it"
        );
    }

    #[test]
    fn a_sendspin_stream_start_stops_any_ha_music() {
        for music in [
            PlayState::Loading,
            PlayState::Playing,
            PlayState::Paused,
            PlayState::Error,
        ] {
            assert!(sendspin_stops_music(true, music), "{music:?}");
            assert!(!sendspin_stops_music(false, music), "{music:?}");
        }
        assert!(!sendspin_stops_music(true, PlayState::Idle));
    }

    #[test]
    fn steps_on_the_way_to_a_request_are_shown_but_not_reported() {
        let mut v = VolumeSync::default();
        v.on_report(0.2, 0);
        v.request(0.3, 100);
        v.request(0.4, 110);
        assert_eq!(v.on_report(0.3, 120), change(0.3, false, false));
        assert_eq!(v.shown(), Some(0.3));
        assert_eq!(v.on_report(0.4, 130), change(0.4, true, false));
        assert_eq!(v.on_report(0.45, 600), change(0.45, true, true));
    }

    #[test]
    fn a_request_that_never_comes_back_expires() {
        let mut v = VolumeSync::default();
        v.on_report(0.2, 0);
        v.request(0.5, 0);
        assert_eq!(
            v.on_report(0.25, VOLUME_CONFIRM_MS),
            change(0.25, true, true)
        );
        assert_eq!(
            v.on_report(0.5, VOLUME_CONFIRM_MS + 10),
            change(0.5, true, true)
        );
    }

    #[test]
    fn index_maps_to_unit_range() {
        assert_eq!(volume_from_index(20, 100), 0.2);
        assert_eq!(volume_from_index(150, 100), 1.0);
        assert_eq!(volume_from_index(3, 0), 0.0);
    }

    #[test]
    fn steps_go_to_the_next_grid_point_and_stop_at_the_ends() {
        assert_eq!(step_volume(0.2, true), 0.25);
        assert_eq!(step_volume(0.23, true), 0.25);
        assert_eq!(step_volume(0.23, false), 0.2);
        assert_eq!(step_volume(0.08, true), 0.1);
        assert_eq!(step_volume(1.0, true), 1.0);
        assert_eq!(step_volume(0.0, false), 0.0);
        assert_eq!(step_volume(f32::NAN, true), 0.05);
    }
}
