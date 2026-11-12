use crate::player_proto::PlayState;

pub const DUCK_GAIN: f32 = 0.15;
const VOLUME_STEP: f32 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Idle,
    Playing,
    Paused,
    Announcing,
}

pub fn shown_state(announcing: bool, music: PlayState) -> Shown {
    if announcing {
        return Shown::Announcing;
    }
    match music {
        PlayState::Playing | PlayState::Loading => Shown::Playing,
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

pub fn volume_from_index(index: u32, max: u32) -> f32 {
    if max == 0 {
        return 0.0;
    }
    (index.min(max) as f32 / max as f32).clamp(0.0, 1.0)
}

/// Lands on the 5 % grid, so repeated steps from an odd volume do not drift.
pub fn step_volume(volume: f32, up: bool) -> f32 {
    let volume = if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, 1.0)
    };
    let level = (volume / VOLUME_STEP).round();
    let next = if up { level + 1.0 } else { level - 1.0 };
    (next * VOLUME_STEP).clamp(0.0, 1.0)
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
}

/// pulsar's order: url, then volume, then the command. Volume steps need a known volume.
pub fn media_actions(req: MediaRequest, music: PlayState, volume: Option<f32>) -> Vec<MediaAction> {
    let mut out = Vec::new();
    if let Some(url) = req.url {
        out.push(if req.announcement {
            MediaAction::Announce(url)
        } else {
            MediaAction::PlayMusic(url)
        });
    }
    let mut volume = volume;
    if let Some(v) = req.volume {
        let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        volume = Some(v);
        out.push(MediaAction::SetVolume(v));
    }
    match req.command {
        Some(MediaCmd::Play) => out.push(MediaAction::Resume),
        Some(MediaCmd::Pause) => out.push(MediaAction::Pause),
        Some(MediaCmd::Toggle) if music == PlayState::Playing => out.push(MediaAction::Pause),
        Some(MediaCmd::Toggle) => out.push(MediaAction::Resume),
        Some(MediaCmd::Stop) => out.push(MediaAction::Stop),
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

/// A finished HA timer rings once the station is free: never on top of a dialog or an
/// announcement.
#[derive(Debug, Default)]
pub struct AlertQueue {
    pending: bool,
}

impl AlertQueue {
    pub fn request(&mut self) {
        self.pending = true;
    }

    pub fn take_if_free(&mut self, free: bool) -> bool {
        let go = self.pending && free;
        if go {
            self.pending = false;
        }
        go
    }
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
            media_actions(r, PlayState::Idle, Some(0.2)),
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
            media_actions(r, PlayState::Playing, None),
            [MediaAction::Announce("http://a".into())]
        );
    }

    #[test]
    fn commands_map_to_player_actions() {
        let one = |cmd, music| media_actions(req(Some(cmd)), music, Some(0.5));
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
        assert!(media_actions(req(None), PlayState::Idle, None).is_empty());
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
        assert!(media_actions(req(Some(MediaCmd::VolumeUp)), PlayState::Idle, None).is_empty());
        let down = media_actions(req(Some(MediaCmd::VolumeDown)), PlayState::Idle, Some(0.5));
        assert!(close(volumes(&down), &[0.45]), "{down:?}");
        let r = MediaRequest {
            command: Some(MediaCmd::VolumeUp),
            volume: Some(f32::NAN),
            ..MediaRequest::default()
        };
        let up = media_actions(r, PlayState::Idle, Some(0.9));
        assert!(close(volumes(&up), &[0.0, 0.05]), "{up:?}");
    }

    #[test]
    fn announce_urls_drop_empty_ids() {
        assert_eq!(announce_urls(String::new(), "m".into()), ["m"]);
        assert_eq!(announce_urls("p".into(), "m".into()), ["p", "m"]);
        assert!(announce_urls(String::new(), String::new()).is_empty());
    }

    #[test]
    fn alert_waits_until_the_station_is_free_and_rings_once() {
        let mut q = AlertQueue::default();
        assert!(!q.take_if_free(true));
        q.request();
        assert!(!q.take_if_free(false));
        assert!(q.take_if_free(true));
        assert!(!q.take_if_free(true));
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
        assert_eq!(shown_state(true, PlayState::Playing), Shown::Announcing);
        assert_eq!(shown_state(false, PlayState::Loading), Shown::Playing);
        assert_eq!(shown_state(false, PlayState::Playing), Shown::Playing);
        assert_eq!(shown_state(false, PlayState::Paused), Shown::Paused);
        assert_eq!(shown_state(false, PlayState::Error), Shown::Idle);
        assert_eq!(shown_state(false, PlayState::Idle), Shown::Idle);
    }

    #[test]
    fn gains_follow_mute_then_duck() {
        let g = |music, announce| Gains { music, announce };
        assert_eq!(channel_gains(true, true), g(0.0, 0.0));
        assert_eq!(channel_gains(true, false), g(0.0, 0.0));
        assert_eq!(channel_gains(false, true), g(DUCK_GAIN, 1.0));
        assert_eq!(channel_gains(false, false), g(1.0, 1.0));
    }

    #[test]
    fn index_maps_to_unit_range() {
        assert_eq!(volume_from_index(20, 100), 0.2);
        assert_eq!(volume_from_index(150, 100), 1.0);
        assert_eq!(volume_from_index(3, 0), 0.0);
    }

    #[test]
    fn steps_snap_to_the_grid_and_stop_at_the_ends() {
        assert!((step_volume(0.2, true) - 0.25).abs() < 1e-6);
        assert!((step_volume(0.23, true) - 0.3).abs() < 1e-6);
        assert!((step_volume(0.23, false) - 0.2).abs() < 1e-6);
        assert_eq!(step_volume(1.0, true), 1.0);
        assert_eq!(step_volume(0.0, false), 0.0);
        assert_eq!(step_volume(f32::NAN, true), 0.05);
    }
}
