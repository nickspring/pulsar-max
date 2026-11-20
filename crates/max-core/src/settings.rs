//! Unknown, missing or bad keys keep their default: a build rolled back to `prev/` and a newer one
//! read each other's file.

use crate::dialog::FOLLOWUP_DURATION_MAX_S;
use crate::screen::Viz;

/// At 1-2 m `vqe_0` speech sits near -62 dBFS (docs/max.md, "Звук"); with 24 dB faster-whisper
/// (large-v3) dropped such requests as silence, with 36 dB it heard them.
pub const DEFAULT_MIC_GAIN_DB: u32 = 36;
pub const MIC_GAIN_MAX_DB: u32 = 40;
pub const DEFAULT_FOLLOWUP_S: u32 = 10;
pub const COMMAND_OFFSET_MIN_MS: i32 = -100;
pub const COMMAND_OFFSET_MAX_MS: i32 = 200;
/// A change is written this long after the last one: a slider or an automation burst is one flash
/// write, as in pulsar's `state.conf`.
pub const SAVE_DELAY_MS: u64 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub viz: Viz,
    pub voice_indication: bool,
    pub mic_gain_db: u32,
    pub followup_s: u32,
    pub command_offset_ms: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            viz: Viz::Random,
            voice_indication: true,
            mic_gain_db: DEFAULT_MIC_GAIN_DB,
            followup_s: DEFAULT_FOLLOWUP_S,
            command_offset_ms: 0,
        }
    }
}

impl Settings {
    /// A missing file is a first start, not an error to report.
    pub fn from_read(read: std::io::Result<String>) -> (Self, Option<std::io::Error>) {
        match read {
            Ok(text) => (Self::parse(&text), None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(e)),
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            let num = || value.parse::<f32>().ok();
            match key.trim() {
                "music_viz" => s.viz = Viz::parse(value).unwrap_or(s.viz),
                "voice_indication" => {
                    s.voice_indication = flag(value).unwrap_or(s.voice_indication)
                }
                "mic_gain_db" => {
                    s.mic_gain_db = num().and_then(mic_gain_db).unwrap_or(s.mic_gain_db)
                }
                "followup_duration_s" => {
                    s.followup_s = num().and_then(followup_s).unwrap_or(s.followup_s);
                }
                "command_start_offset_ms" => {
                    s.command_offset_ms = num()
                        .and_then(command_offset_ms)
                        .unwrap_or(s.command_offset_ms);
                }
                _ => {}
            }
        }
        s
    }

    pub fn serialize(&self) -> String {
        format!(
            "music_viz={}\nvoice_indication={}\nmic_gain_db={}\nfollowup_duration_s={}\n\
             command_start_offset_ms={}\n",
            self.viz.label(),
            u8::from(self.voice_indication),
            self.mic_gain_db,
            self.followup_s,
            self.command_offset_ms,
        )
    }
}

/// What is on disk. A failed write keeps the old value, so the next `flush` retries it.
#[derive(Debug)]
pub struct Store {
    saved: Settings,
    failing: bool,
}

impl Store {
    pub fn new(saved: Settings) -> Self {
        Self {
            saved,
            failing: false,
        }
    }

    pub fn saved(&self) -> Settings {
        self.saved
    }

    /// Writes `s` unless it is already on disk. Returns the error to log: only the first one of a
    /// run of failures.
    pub fn flush<E>(
        &mut self,
        s: &Settings,
        write: impl FnOnce(&str) -> Result<(), E>,
    ) -> Option<E> {
        if *s == self.saved {
            return None;
        }
        match write(&s.serialize()) {
            Ok(()) => {
                self.saved = *s;
                self.failing = false;
                None
            }
            Err(e) => (!std::mem::replace(&mut self.failing, true)).then_some(e),
        }
    }
}

fn flag(v: &str) -> Option<bool> {
    match v {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

pub fn mic_gain_db(v: f32) -> Option<u32> {
    v.is_finite()
        .then(|| v.round().clamp(0.0, MIC_GAIN_MAX_DB as f32) as u32)
}

pub fn followup_s(v: f32) -> Option<u32> {
    v.is_finite()
        .then(|| v.round().clamp(0.0, FOLLOWUP_DURATION_MAX_S as f32) as u32)
}

pub fn command_offset_ms(v: f32) -> Option<i32> {
    v.is_finite().then(|| {
        v.round()
            .clamp(COMMAND_OFFSET_MIN_MS as f32, COMMAND_OFFSET_MAX_MS as f32) as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spectrogram::Style;

    fn custom() -> Settings {
        Settings {
            viz: Viz::Fixed(Style::BarsCenter),
            voice_indication: false,
            mic_gain_db: 0,
            followup_s: 3,
            command_offset_ms: -40,
        }
    }

    #[test]
    fn round_trip_keeps_every_value() {
        assert_eq!(Settings::parse(&custom().serialize()), custom());
        for viz in Viz::all() {
            let s = Settings { viz, ..custom() };
            assert_eq!(Settings::parse(&s.serialize()), s);
        }
    }

    #[test]
    fn defaults() {
        let d = Settings::default();
        assert_eq!(d.viz, Viz::Random);
        assert!(d.voice_indication);
        assert_eq!(
            (d.mic_gain_db, d.followup_s, d.command_offset_ms),
            (36, 10, 0)
        );
    }

    #[test]
    fn empty_or_garbage_text_gives_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("\n\nno equals sign\n=\n"),
            Settings::default()
        );
    }

    #[test]
    fn missing_keys_keep_defaults() {
        let s = Settings::parse("mic_gain_db=12\n");
        assert_eq!(
            s,
            Settings {
                mic_gain_db: 12,
                ..Settings::default()
            }
        );
    }

    #[test]
    fn unknown_keys_are_skipped() {
        let text = format!(
            "from_a_newer_build=7\n{}led_speed=2\n",
            custom().serialize()
        );
        assert_eq!(Settings::parse(&text), custom());
    }

    #[test]
    fn bad_values_keep_the_default() {
        let text = "music_viz=Spiral\nvoice_indication=yes\nmic_gain_db=loud\n\
                    followup_duration_s=NaN\ncommand_start_offset_ms=inf\n";
        assert_eq!(Settings::parse(text), Settings::default());
    }

    #[test]
    fn numbers_are_rounded_and_clamped_like_ha_commands() {
        let s = Settings::parse(
            "mic_gain_db=99\nfollowup_duration_s=-3\ncommand_start_offset_ms=-500\n",
        );
        assert_eq!(
            (s.mic_gain_db, s.followup_s, s.command_offset_ms),
            (40, 0, -100)
        );
        let s = Settings::parse(
            "mic_gain_db=-1\nfollowup_duration_s=60\ncommand_start_offset_ms=999\n",
        );
        assert_eq!(
            (s.mic_gain_db, s.followup_s, s.command_offset_ms),
            (0, 10, 200)
        );
        let s = Settings::parse("mic_gain_db=17.6\ncommand_start_offset_ms=20.4\n");
        assert_eq!((s.mic_gain_db, s.command_offset_ms), (18, 20));
    }

    #[test]
    fn non_finite_gain_and_word_booleans_are_bad_values() {
        for v in ["nan", "inf", "-inf"] {
            let s = Settings::parse(&format!("mic_gain_db={v}\n"));
            assert_eq!(s.mic_gain_db, DEFAULT_MIC_GAIN_DB, "{v}");
        }
        assert!(Settings::parse("voice_indication=false\n").voice_indication);
        assert!(!Settings::parse("voice_indication=0\nvoice_indication=true\n").voice_indication);
    }

    #[test]
    fn followup_is_rounded() {
        let parse = |v: &str| Settings::parse(&format!("followup_duration_s={v}\n")).followup_s;
        assert_eq!((parse("9.6"), parse("2.4")), (10, 2));
    }

    #[test]
    fn a_bad_later_line_keeps_the_earlier_value() {
        let text = format!(
            "{}music_viz=Spiral\nvoice_indication=yes\nmic_gain_db=nan\n\
             followup_duration_s=x\ncommand_start_offset_ms=inf\n",
            custom().serialize()
        );
        assert_eq!(Settings::parse(&text), custom());
    }

    #[test]
    fn a_missing_file_is_silent_other_read_errors_are_reported() {
        use std::io::{Error, ErrorKind};
        let (s, err) = Settings::from_read(Ok(custom().serialize()));
        assert_eq!((s, err.is_none()), (custom(), true));
        let (s, err) = Settings::from_read(Err(Error::from(ErrorKind::NotFound)));
        assert_eq!((s, err.is_none()), (Settings::default(), true));
        let (s, err) = Settings::from_read(Err(Error::from(ErrorKind::InvalidData)));
        assert_eq!(s, Settings::default());
        assert_eq!(err.map(|e| e.kind()), Some(ErrorKind::InvalidData));
    }

    #[test]
    fn store_writes_only_what_is_not_on_disk() {
        let mut store = Store::new(Settings::default());
        let mut written = Vec::new();
        let mut write = |text: &str| {
            written.push(text.to_owned());
            Ok::<(), ()>(())
        };
        assert_eq!(store.flush(&Settings::default(), &mut write), None);
        assert_eq!(store.flush(&custom(), &mut write), None);
        assert_eq!(store.flush(&custom(), &mut write), None);
        assert_eq!(written, [custom().serialize()]);
        assert_eq!(store.saved(), custom());
    }

    #[test]
    fn store_retries_a_failed_write_and_reports_once_per_run() {
        let mut store = Store::new(Settings::default());
        let mut tries = 0;
        let mut fail = |_: &str| {
            tries += 1;
            Err::<(), u32>(tries)
        };
        assert_eq!(store.flush(&custom(), &mut fail), Some(1));
        assert_eq!(store.flush(&custom(), &mut fail), None);
        assert_eq!(tries, 2);
        assert_eq!(store.saved(), Settings::default());
        assert_eq!(store.flush(&Settings::default(), |_| Err(9)), None);

        assert_eq!(store.flush(&custom(), |_| Ok::<(), u32>(())), None);
        assert_eq!(store.saved(), custom());
        assert_eq!(store.flush(&Settings::default(), |_| Err(3)), Some(3));
    }

    #[test]
    fn whitespace_and_crlf_are_tolerated() {
        let s = Settings::parse(" music_viz = Bars center \r\nvoice_indication=0\r\n");
        assert_eq!(s.viz, Viz::Fixed(Style::BarsCenter));
        assert!(!s.voice_indication);
    }

    #[test]
    fn a_later_line_wins() {
        assert_eq!(
            Settings::parse("followup_duration_s=2\nfollowup_duration_s=5\n").followup_s,
            5
        );
    }
}
