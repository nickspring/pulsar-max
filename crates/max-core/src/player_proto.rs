use crate::dialog::{Channel, PlayId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Idle,
    Loading,
    Playing,
    Paused,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerEvent {
    Ready,
    State(Channel, PlayState),
    Done { id: PlayId, ok: bool },
    Volume { index: u32, max: u32 },
}

pub fn channel_name(ch: Channel) -> &'static str {
    match ch {
        Channel::Music => "music",
        Channel::Announce => "announce",
    }
}

fn channel_of(s: &str) -> Option<Channel> {
    match s {
        "music" => Some(Channel::Music),
        "announce" => Some(Channel::Announce),
        _ => None,
    }
}

/// None when a URL cannot travel in one tab-separated line.
pub fn play_line(id: PlayId, ch: Channel, urls: &[String]) -> Option<String> {
    if urls.is_empty()
        || urls
            .iter()
            .any(|u| u.is_empty() || u.contains(['\t', '\n', '\r']))
    {
        return None;
    }
    Some(format!(
        "play\t{}\t{}\t{}",
        id.0,
        channel_name(ch),
        urls.join("\t")
    ))
}

pub fn simple_line(command: &str, ch: Channel) -> String {
    format!("{command}\t{}", channel_name(ch))
}

pub fn gain_line(ch: Channel, gain: f32) -> String {
    format!("gain\t{}\t{:.3}", channel_name(ch), clamp01(gain))
}

pub fn volume_line(volume: f32) -> String {
    format!("volume\t{:.3}", clamp01(volume))
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// The id a `play_line` carries, so a command that never reached the helper can be failed.
pub fn play_id_of(line: &str) -> Option<PlayId> {
    let mut f = line.split('\t');
    (f.next() == Some("play")).then_some(())?;
    f.next()?.parse().ok().map(PlayId)
}

pub fn parse_event(line: &str) -> Option<PlayerEvent> {
    let f: Vec<&str> = line.trim_end_matches(['\r', '\n']).split('\t').collect();
    match f.as_slice() {
        ["ready"] => Some(PlayerEvent::Ready),
        ["state", ch, st] => {
            let st = match *st {
                "idle" => PlayState::Idle,
                "loading" => PlayState::Loading,
                "playing" => PlayState::Playing,
                "paused" => PlayState::Paused,
                "error" => PlayState::Error,
                _ => return None,
            };
            Some(PlayerEvent::State(channel_of(ch)?, st))
        }
        ["done", id, ok] => Some(PlayerEvent::Done {
            id: PlayId(id.parse().ok()?),
            ok: match *ok {
                "1" => true,
                "0" => false,
                _ => return None,
            },
        }),
        ["volume", index, max] => {
            let (index, max) = (index.parse().ok()?, max.parse().ok()?);
            (max > 0 && index <= max).then_some(PlayerEvent::Volume { index, max })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn play_line_round_trips_its_id() {
        let line = play_line(
            PlayId(7),
            Channel::Announce,
            &urls(&["http://a/1.mp3", "file:///b.wav"]),
        );
        assert_eq!(
            line.as_deref(),
            Some("play\t7\tannounce\thttp://a/1.mp3\tfile:///b.wav")
        );
        assert_eq!(play_id_of(&line.unwrap()), Some(PlayId(7)));
    }

    #[test]
    fn play_line_rejects_what_breaks_the_framing() {
        assert_eq!(play_line(PlayId(1), Channel::Music, &[]), None);
        assert_eq!(play_line(PlayId(1), Channel::Music, &urls(&[""])), None);
        assert_eq!(play_line(PlayId(1), Channel::Music, &urls(&["a\tb"])), None);
        assert_eq!(play_line(PlayId(1), Channel::Music, &urls(&["a\nb"])), None);
    }

    #[test]
    fn play_id_only_from_play_commands() {
        assert_eq!(play_id_of("stop\tmusic"), None);
        assert_eq!(play_id_of("play\tx\tmusic\tu"), None);
        assert_eq!(play_id_of("play"), None);
    }

    #[test]
    fn numeric_commands_are_clamped() {
        assert_eq!(gain_line(Channel::Music, 0.15), "gain\tmusic\t0.150");
        assert_eq!(gain_line(Channel::Announce, 3.0), "gain\tannounce\t1.000");
        assert_eq!(volume_line(-1.0), "volume\t0.000");
        assert_eq!(volume_line(f32::NAN), "volume\t0.000");
        assert_eq!(simple_line("pause", Channel::Music), "pause\tmusic");
    }

    #[test]
    fn parses_every_event() {
        assert_eq!(parse_event("ready\n"), Some(PlayerEvent::Ready));
        assert_eq!(
            parse_event("state\tmusic\tloading"),
            Some(PlayerEvent::State(Channel::Music, PlayState::Loading))
        );
        for (word, st) in [
            ("idle", PlayState::Idle),
            ("playing", PlayState::Playing),
            ("paused", PlayState::Paused),
            ("error", PlayState::Error),
        ] {
            assert_eq!(
                parse_event(&format!("state\tannounce\t{word}")),
                Some(PlayerEvent::State(Channel::Announce, st))
            );
        }
        assert_eq!(
            parse_event("done\t12\t1\r\n"),
            Some(PlayerEvent::Done {
                id: PlayId(12),
                ok: true
            })
        );
        assert_eq!(
            parse_event("done\t3\t0"),
            Some(PlayerEvent::Done {
                id: PlayId(3),
                ok: false
            })
        );
        assert_eq!(
            parse_event("volume\t20\t100"),
            Some(PlayerEvent::Volume {
                index: 20,
                max: 100
            })
        );
    }

    #[test]
    fn malformed_events_are_dropped() {
        for line in [
            "",
            "state\tradio\tidle",
            "state\tmusic\tbusy",
            "done\tx\t1",
            "done\t1\tyes",
            "volume\t5\t0",
            "volume\t101\t100",
            "volume\ta\t100",
            "ready\textra",
        ] {
            assert_eq!(parse_event(line), None, "{line:?}");
        }
    }
}
