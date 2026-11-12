use log::{debug, info, warn};
use max_core::audio::{apply_gain, gain_from_db};
use max_core::dialog::{Channel, Dialog, Effects, PlayId, Timer, Va};
use std::path::PathBuf;

use max_core::dialog::Phase;
use max_core::media::{self, AlertQueue, MediaAction, MediaCmd, MediaRequest, Shown};
use max_core::player_proto::{PlayState, PlayerEvent};
use pulsar_core::Timers;
use pulsar_esphome::{
    AnnounceRequest, ApiEvent, ApiHandle, MediaCommand, MediaCommandRequest, MediaState, NumberId,
    SelectId, SwitchId, TimerEvent, UpdateState, VaEvent,
};
use pulsar_lan::mdns::MdnsHandle;

use crate::player::PlayerHandle;
use crate::{ESPHOME_SERVICE, VERSION};

/// Provisional: at 1-2 m `vqe_0` speech sits near -62 dBFS (docs/max.md, "Звук").
const DEFAULT_MIC_GAIN_DB: u32 = 24;
const DEFAULT_FOLLOWUP_S: u32 = 10;
const MUSIC_VIZ_OPTION: &str = "Off";

pub const MUSIC_STYLES: &[&str] = &[MUSIC_VIZ_OPTION];

pub enum Flow {
    Continue,
    Exit,
}

pub struct Controller {
    dialog: Dialog,
    fx: Fx,
    mdns: Option<MdnsHandle>,
    mic_gain_db: u32,
    followup_s: u32,
    mic_enabled: bool,
    alerts: AlertQueue,
    alert_path: PathBuf,
}

pub struct Fx {
    api: ApiHandle,
    player: PlayerHandle,
    pub timers: Timers<Timer>,
    streaming: bool,
    ducked: bool,
    announcing: bool,
    muted: bool,
    volume: Option<f32>,
    music: PlayState,
}

impl Controller {
    pub fn new(
        api: ApiHandle,
        player: PlayerHandle,
        mdns: Option<MdnsHandle>,
        alert_path: PathBuf,
    ) -> Self {
        Self {
            dialog: Dialog::new(DEFAULT_FOLLOWUP_S),
            fx: Fx {
                api,
                player,
                timers: Timers::new(),
                streaming: false,
                ducked: false,
                announcing: false,
                muted: false,
                volume: None,
                music: PlayState::Idle,
            },
            mdns,
            mic_gain_db: DEFAULT_MIC_GAIN_DB,
            followup_s: DEFAULT_FOLLOWUP_S,
            mic_enabled: true,
            alerts: AlertQueue::default(),
            alert_path,
        }
    }

    pub fn after_event(&mut self) {
        let free = self.dialog.phase() == Phase::Idle && !self.dialog.announcing();
        if self.alerts.take_if_free(free) {
            let path = self.alert_path.to_string_lossy().into_owned();
            self.dialog.on_media_announce(path, &mut self.fx);
        }
    }

    pub fn timers(&mut self) -> &mut Timers<Timer> {
        &mut self.fx.timers
    }

    pub fn publish_all(&self) {
        let api = &self.fx.api;
        self.fx.publish_media();
        api.publish_switch(SwitchId::Microphones, self.mic_enabled);
        api.publish_switch(SwitchId::AutoUpdate, false);
        api.publish_switch(SwitchId::NearestAnswers, false);
        api.publish_switch(SwitchId::VoiceIndication, true);
        api.publish_number(NumberId::MicGain, self.mic_gain_db as f32);
        api.publish_number(NumberId::FollowupDuration, self.followup_s as f32);
        api.publish_number(NumberId::CommandOffset, 0.0);
        api.publish_number(NumberId::LedSpeed, 1.0);
        api.publish_select(SelectId::MusicViz, MUSIC_VIZ_OPTION);
        api.publish_update(&UpdateState {
            in_progress: false,
            has_progress: false,
            progress: 0.0,
            current_version: VERSION.into(),
            latest_version: VERSION.into(),
            title: "pulsar-max".into(),
            release_url: String::new(),
        });
    }

    pub fn on_timer(&mut self, t: Timer) {
        self.dialog.on_timer(t, &mut self.fx);
    }

    pub fn on_audio(&mut self, mut chunk: Vec<i16>) {
        if !self.fx.streaming {
            return;
        }
        apply_gain(&mut chunk, gain_from_db(self.mic_gain_db));
        self.fx.api.voice_send_audio(chunk);
    }

    pub fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Done { id, ok } => self.dialog.on_play_done(id, ok, &mut self.fx),
            PlayerEvent::State(Channel::Music, st) => {
                self.fx.music = st;
                self.fx.publish_media();
            }
            PlayerEvent::State(Channel::Announce, _) | PlayerEvent::Ready => {}
            PlayerEvent::Volume { index, max } => {
                self.fx.volume = Some(media::volume_from_index(index, max));
                self.fx.publish_media();
            }
        }
    }

    pub fn on_api(&mut self, ev: ApiEvent) -> Flow {
        match ev {
            ApiEvent::ClientConnected { client_info } => {
                info!("api: client {client_info}");
                self.publish_all();
            }
            ApiEvent::ClientDisconnected | ApiEvent::VoiceSubscribed(false) => {
                self.dialog.on_link_lost(&mut self.fx);
            }
            ApiEvent::VoiceSubscribed(true) => {}
            ApiEvent::VoiceEvent { event, data } => {
                self.dialog.on_va(va(event), &data, &mut self.fx);
            }
            ApiEvent::VoiceStartFailed { run } => self.dialog.on_start_failed(run, &mut self.fx),
            ApiEvent::VoiceAudioOverflow { run } => {
                self.dialog.on_audio_overflow(run, &mut self.fx);
            }
            ApiEvent::Announce(req) => self.on_announce(req),
            ApiEvent::MediaCommand(req) => self.on_media_command(req),
            ApiEvent::Switch(id, on) => self.on_switch(id, on),
            ApiEvent::Number(id, v) => self.on_number(id, v),
            ApiEvent::Select(id, v) => self.fx.api.publish_select(id, &v),
            ApiEvent::MdnsTxtChanged(txt) => {
                if let Some(m) = &self.mdns {
                    m.set_txt(ESPHOME_SERVICE, txt);
                }
            }
            ApiEvent::Timer(t) if t.event == TimerEvent::Finished => {
                info!("timer '{}' finished", t.name);
                self.alerts.request();
            }
            ApiEvent::Restart => return Flow::Exit,
            other => debug!("api: {other:?}"),
        }
        Flow::Continue
    }

    fn on_announce(&mut self, req: AnnounceRequest) {
        let urls = media::announce_urls(req.preannounce_media_id, req.media_id);
        self.dialog
            .on_announce(urls, req.start_conversation, &mut self.fx);
    }

    fn on_media_command(&mut self, req: MediaCommandRequest) {
        let req = MediaRequest {
            command: req.command.map(media_cmd),
            volume: req.volume,
            url: req.media_url,
            announcement: req.announcement == Some(true),
        };
        for action in media::media_actions(req, self.fx.music, self.fx.volume) {
            match action {
                MediaAction::PlayMusic(url) => {
                    self.fx.player.play(Channel::Music, vec![url]);
                }
                MediaAction::Announce(url) => self.dialog.on_media_announce(url, &mut self.fx),
                MediaAction::SetVolume(v) => self.fx.player.set_volume(v),
                MediaAction::Resume => self.fx.player.resume(Channel::Music),
                MediaAction::Pause => self.fx.player.pause(Channel::Music),
                MediaAction::Stop => self.fx.player.stop(Channel::Music),
                MediaAction::SetMuted(m) => self.fx.set_muted(m),
            }
        }
        self.fx.publish_media();
    }

    fn on_switch(&mut self, id: SwitchId, on: bool) {
        if id == SwitchId::Microphones {
            self.mic_enabled = on;
            self.dialog.set_mic_enabled(on, &mut self.fx);
        }
        self.fx.api.publish_switch(id, on);
    }

    fn on_number(&mut self, id: NumberId, v: f32) {
        match id {
            NumberId::MicGain => {
                self.mic_gain_db = v.round() as u32;
                self.fx.api.publish_number(id, self.mic_gain_db as f32);
            }
            NumberId::FollowupDuration => {
                self.followup_s = v.round() as u32;
                self.dialog
                    .set_followup_duration(self.followup_s, &mut self.fx);
                self.fx.api.publish_number(id, self.followup_s as f32);
            }
            _ => self.fx.api.publish_number(id, v),
        }
    }
}

impl Fx {
    fn publish_media(&self) {
        let st = match media::shown_state(self.announcing, self.music) {
            Shown::Idle => MediaState::Idle,
            Shown::Playing => MediaState::Playing,
            Shown::Paused => MediaState::Paused,
            Shown::Announcing => MediaState::Announcing,
        };
        self.api
            .publish_media(st, self.volume.unwrap_or(0.0), self.muted);
    }

    fn apply_gains(&self) {
        let g = media::channel_gains(self.muted, self.ducked);
        self.player.set_gain(Channel::Music, g.music);
        self.player.set_gain(Channel::Announce, g.announce);
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gains();
    }
}

impl Effects for Fx {
    fn voice_subscribed(&self) -> bool {
        self.api.voice_subscribed()
    }

    fn start_run(&mut self, phrase: &str) -> u64 {
        self.api.voice_request_start(phrase)
    }

    fn stop_run(&mut self) {
        self.api.voice_request_stop();
    }

    fn answer_finished(&mut self, ok: bool) {
        self.api.voice_answer_finished(ok);
    }

    fn announce_finished(&mut self, ok: bool) {
        self.api.voice_announce_finished(ok);
    }

    fn set_streaming(&mut self, on: bool) {
        if self.streaming != on {
            debug!("voice: streaming {on}");
        }
        self.streaming = on;
    }

    fn play(&mut self, ch: Channel, urls: Vec<String>) -> PlayId {
        self.player.play(ch, urls)
    }

    fn stop_play(&mut self, ch: Channel) {
        self.player.stop(ch);
    }

    fn arm(&mut self, timer: Timer, ms: u64) {
        self.timers.set(timer, ms);
    }

    fn cancel(&mut self, timer: Timer) {
        self.timers.cancel(timer);
    }

    fn set_duck(&mut self, on: bool) {
        if self.ducked != on {
            self.ducked = on;
            self.apply_gains();
        }
    }

    fn set_announcing(&mut self, on: bool) {
        self.announcing = on;
        self.publish_media();
    }
}

fn media_cmd(cmd: MediaCommand) -> MediaCmd {
    match cmd {
        MediaCommand::Play => MediaCmd::Play,
        MediaCommand::Pause => MediaCmd::Pause,
        MediaCommand::Stop => MediaCmd::Stop,
        MediaCommand::Mute => MediaCmd::Mute,
        MediaCommand::Unmute => MediaCmd::Unmute,
        MediaCommand::Toggle => MediaCmd::Toggle,
        MediaCommand::VolumeUp => MediaCmd::VolumeUp,
        MediaCommand::VolumeDown => MediaCmd::VolumeDown,
        _ => MediaCmd::Other,
    }
}

fn va(ev: VaEvent) -> Va {
    match ev {
        VaEvent::Error => Va::Error,
        VaEvent::RunStart => Va::RunStart,
        VaEvent::RunEnd => Va::RunEnd,
        VaEvent::SttStart => Va::SttStart,
        VaEvent::SttEnd => Va::SttEnd,
        VaEvent::IntentStart => Va::IntentStart,
        VaEvent::IntentEnd => Va::IntentEnd,
        VaEvent::TtsStart => Va::TtsStart,
        VaEvent::TtsEnd => Va::TtsEnd,
        VaEvent::WakeWordStart => Va::WakeWordStart,
        VaEvent::WakeWordEnd => Va::WakeWordEnd,
        VaEvent::SttVadStart => Va::SttVadStart,
        VaEvent::SttVadEnd => Va::SttVadEnd,
        VaEvent::TtsStreamStart => Va::TtsStreamStart,
        VaEvent::TtsStreamEnd => Va::TtsStreamEnd,
        VaEvent::IntentProgress => Va::IntentProgress,
        VaEvent::Unknown(code) => {
            warn!("voice: unknown pipeline event {code}");
            Va::Other
        }
    }
}
