use log::{debug, info, warn};
use max_core::audio::{apply_gain, gain_from_db};
use max_core::commands::{
    MusicAction, MusicOp, MusicOwnership, MusicState, TransportCmd, command_of, music_ops,
};
use max_core::dialog::{Channel, Dialog, Effects, HaTimerEvent, PlayId, Sounds, Timer, Ui, Va};
use max_core::input::{InputAction, Keypad, Press, knob_turn};
use max_core::media::{self, HaVolumeEcho, MediaAction, MediaCmd, MediaRequest, Shown, VolumeSync};
use max_core::mic::Mics;
use max_core::player_proto::{PlayState, PlayerEvent};
use max_core::preroll::{self, Preroll};
use max_core::screen::{ScreenChoice, ScreenCmd, Viz, playback_started, screen_source};
use pulsar_core::Timers;
use pulsar_esphome::{
    AnnounceRequest, ApiEvent, ApiHandle, BinarySensorId, EventId, MediaCommand,
    MediaCommandRequest, MediaState, NumberId, SelectId, SwitchId, TimerEvent, UpdateState,
    VaEvent,
};
use pulsar_lan::mdns::MdnsHandle;
use pulsar_sendspin::{SendspinEvent, SendspinHandle, Transport};

use crate::capture::AudioChunk;
use crate::input::InputEvent;
use crate::player::PlayerHandle;
use crate::ring::RingHandle;
use crate::screen::ScreenHandle;
use crate::wake::WakeHandle;
use crate::{ESPHOME_SERVICE, VERSION, WAKE_PHRASE};

/// Provisional: at 1-2 m `vqe_0` speech sits near -62 dBFS (docs/max.md, "Звук").
const DEFAULT_MIC_GAIN_DB: u32 = 24;
const DEFAULT_FOLLOWUP_S: u32 = 10;
/// Two seconds of 16 kHz audio: longer than the wake thread's queue, so the pre-roll is complete.
const PREROLL_SAMPLES: usize = 32_000;
/// pulsar's pre-roll chunking.
const PREROLL_CHUNK: usize = 512;

pub enum Flow {
    Continue,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Dialog(Timer),
    Keypad,
}

pub struct Controller {
    dialog: Dialog,
    fx: Fx,
    mdns: Option<MdnsHandle>,
    mic_gain_db: u32,
    command_offset_ms: i32,
    voice_indication: bool,
    followup_s: u32,
    mics: Mics,
    keypad: Keypad,
    preroll: Preroll,
    wake: Option<WakeHandle>,
    pairing_prompt: bool,
    ownership: MusicOwnership,
    viz: Viz,
    screen_choice: ScreenChoice,
}

pub struct Fx {
    api: ApiHandle,
    player: PlayerHandle,
    ring: Option<RingHandle>,
    timers: Timers<Tick>,
    streaming: bool,
    ducked: bool,
    announcing: bool,
    muted: bool,
    volume: VolumeSync,
    ha_echo: HaVolumeEcho,
    music: PlayState,
    sendspin: Option<SendspinHandle>,
    volume_click: Option<String>,
    screen: Option<ScreenHandle>,
}

impl Controller {
    pub fn new(
        api: ApiHandle,
        player: PlayerHandle,
        mdns: Option<MdnsHandle>,
        sounds: Sounds,
        wake: Option<WakeHandle>,
        ring: Option<RingHandle>,
        sendspin: Option<SendspinHandle>,
    ) -> Self {
        Self {
            dialog: Dialog::new(DEFAULT_FOLLOWUP_S, sounds.clone()),
            fx: Fx {
                api,
                player,
                ring,
                timers: Timers::new(),
                streaming: false,
                ducked: false,
                announcing: false,
                muted: false,
                volume: VolumeSync::default(),
                ha_echo: HaVolumeEcho::default(),
                music: PlayState::Idle,
                sendspin,
                volume_click: sounds.volume,
                screen: None,
            },
            mdns,
            mic_gain_db: DEFAULT_MIC_GAIN_DB,
            command_offset_ms: 0,
            voice_indication: true,
            followup_s: DEFAULT_FOLLOWUP_S,
            mics: Mics::default(),
            keypad: Keypad::new(),
            preroll: Preroll::new(PREROLL_SAMPLES),
            wake,
            pairing_prompt: false,
            ownership: MusicOwnership::default(),
            viz: Viz::Random,
            screen_choice: ScreenChoice::default(),
        }
    }

    pub fn set_screen(&mut self, screen: Option<ScreenHandle>) {
        self.fx.screen = screen;
    }

    /// `restart`: a new playback began (io.sdk's spectrum start).
    fn update_screen(&mut self, restart: bool) {
        let Some(screen) = &self.fx.screen else {
            return;
        };
        let source = screen_source(self.fx.music, self.fx.sendspin_streaming());
        match self
            .screen_choice
            .update(self.viz, source, restart, random())
        {
            Some(ScreenCmd::Show {
                style,
                restart,
                source,
            }) => screen.show(style, restart, source),
            Some(ScreenCmd::Hide) => screen.hide(),
            None => {}
        }
    }

    pub fn timers(&mut self) -> &mut Timers<Tick> {
        &mut self.fx.timers
    }

    pub fn on_input(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::Opened => {
                self.keypad.reset();
                self.on_input_action(None);
            }
            InputEvent::Key { ev, at_ms } => {
                let action = self.keypad.on_event(ev, at_ms);
                self.on_input_action(action);
            }
            InputEvent::MicMutedState(muted) => self.set_hw_muted(muted),
            InputEvent::Knob(ev) => {
                self.fx.foreign_draw();
                if let Some(up) = knob_turn(ev) {
                    let event = if up { "volume_up" } else { "volume_down" };
                    self.fx.api.fire_event(EventId::Button, event);
                }
            }
        }
    }

    fn on_input_action(&mut self, action: Option<InputAction>) {
        match action {
            Some(InputAction::Action(Press::Short))
                if self.pairing_prompt
                    && let Some(ss) = &self.fx.sendspin =>
            {
                self.fx.api.fire_event(EventId::Button, "action");
                ss.confirm_pairing_window();
            }
            Some(InputAction::Action(Press::Short)) => {
                self.fx.api.fire_event(EventId::Button, "action");
                self.dialog.button(&mut self.fx);
            }
            Some(InputAction::Action(Press::Long)) => {
                self.fx.api.fire_event(EventId::Button, "action_long");
            }
            Some(InputAction::MicMuted(muted)) => self.set_hw_muted(muted),
            None => {}
        }
        match self.keypad.deadline() {
            Some(at) => {
                let delay = u64::try_from(at - pulsar_core::now_ms()).unwrap_or(0);
                self.fx.timers.set(Tick::Keypad, delay);
            }
            None => self.fx.timers.cancel(Tick::Keypad),
        }
    }

    fn set_hw_muted(&mut self, muted: bool) {
        if self.mics.hw_muted() != muted {
            info!("microphone switch: {}", if muted { "off" } else { "on" });
        }
        self.mics.set_hw(muted);
        self.fx
            .api
            .publish_binary_sensor(BinarySensorId::HwMicMute, muted);
        self.apply_mics();
    }

    /// HA sees the actual state, not its own command: "on" under the hardware mute stays off.
    fn apply_mics(&mut self) {
        let on = !self.mics.muted();
        self.fx.api.publish_switch(SwitchId::Microphones, on);
        if let Some(r) = &self.fx.ring {
            r.set_muted(!on);
        }
        self.dialog.set_mic_enabled(on, &mut self.fx);
        if let Some(w) = &self.wake {
            w.set_enabled(on);
        }
    }

    pub fn publish_all(&self) {
        let api = &self.fx.api;
        self.fx.publish_media();
        api.publish_switch(SwitchId::Microphones, !self.mics.muted());
        api.publish_binary_sensor(BinarySensorId::HwMicMute, self.mics.hw_muted());
        api.publish_switch(SwitchId::AutoUpdate, false);
        api.publish_switch(SwitchId::NearestAnswers, false);
        api.publish_switch(SwitchId::VoiceIndication, self.voice_indication);
        api.publish_number(NumberId::MicGain, self.mic_gain_db as f32);
        api.publish_number(NumberId::FollowupDuration, self.followup_s as f32);
        api.publish_number(NumberId::CommandOffset, self.command_offset_ms as f32);
        api.publish_number(NumberId::LedSpeed, 1.0);
        api.publish_select(SelectId::MusicViz, self.viz.label());
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

    pub fn on_timer(&mut self, t: Tick) {
        match t {
            Tick::Dialog(t) => self.dialog.on_timer(t, &mut self.fx),
            Tick::Keypad => {
                let action = self.keypad.on_tick(pulsar_core::now_ms());
                self.on_input_action(action);
            }
        }
    }

    pub fn on_audio(&mut self, chunk: AudioChunk) {
        self.preroll.push(chunk.pos, &chunk.samples);
        if self.fx.streaming {
            self.send_audio(chunk.samples);
        }
    }

    pub fn on_wake(&mut self, end: u64) {
        if !self.dialog.wake(WAKE_PHRASE, &mut self.fx) {
            return;
        }
        let start = preroll::command_start(end, self.command_offset_ms);
        let preroll = self.preroll.since(start);
        for chunk in preroll.chunks(PREROLL_CHUNK) {
            self.send_audio(chunk.to_vec());
        }
    }

    pub fn on_command(&mut self, phrase: &str) {
        let Some(c) = command_of(phrase) else {
            debug!("command '{phrase}': no action on this station");
            return;
        };
        let owns = self.ownership.owns(
            pulsar_core::now_ms(),
            self.fx.music,
            self.fx.sendspin_streaming(),
            self.fx
                .sendspin
                .as_ref()
                .is_some_and(SendspinHandle::connected),
        );
        let outcome = self.dialog.command(c, owns, &mut self.fx);
        info!("command '{phrase}': {outcome:?}");
    }

    fn send_audio(&self, mut samples: Vec<i16>) {
        apply_gain(&mut samples, gain_from_db(self.mic_gain_db));
        self.fx.api.voice_send_audio(samples);
    }

    pub fn on_player(&mut self, ev: PlayerEvent) {
        match ev {
            PlayerEvent::Done { id, ok } => self.dialog.on_play_done(id, ok, &mut self.fx),
            PlayerEvent::State(Channel::Music, st) => {
                let started = playback_started(self.fx.music, st);
                self.fx.music = st;
                self.ownership.on_music_state(st, pulsar_core::now_ms());
                self.fx.publish_media();
                self.update_screen(started);
            }
            PlayerEvent::State(Channel::Announce, _) | PlayerEvent::Ready => {}
            PlayerEvent::Volume { index, max } => {
                let v = media::volume_from_index(index, max);
                let c = self.fx.volume.on_report(v, pulsar_core::now_ms());
                if c.unmute && self.fx.muted {
                    self.fx.set_muted(false);
                }
                if c.settled
                    && let Some(ss) = &self.fx.sendspin
                {
                    ss.report_volume(c.volume, self.fx.muted);
                }
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
                let ev = va(event);
                if ev == Va::SttEnd {
                    info!(
                        "voice: stt '{}'",
                        data.get("text").map_or("", String::as_str)
                    );
                } else if ev == Va::Error {
                    info!("voice: error {:?}", data.get("code"));
                }
                self.dialog.on_va(ev, &data, &mut self.fx);
            }
            ApiEvent::VoiceStartFailed { run } => self.dialog.on_start_failed(run, &mut self.fx),
            ApiEvent::VoiceAudioOverflow { run } => {
                self.dialog.on_audio_overflow(run, &mut self.fx);
            }
            ApiEvent::Announce(req) => self.on_announce(req),
            ApiEvent::MediaCommand(req) => self.on_media_command(req),
            ApiEvent::Switch(id, on) => self.on_switch(id, on),
            ApiEvent::Number(id, v) => self.on_number(id, v),
            ApiEvent::Select(SelectId::MusicViz, v) => {
                if let Some(viz) = Viz::parse(&v) {
                    self.viz = viz;
                    self.update_screen(false);
                }
                self.fx
                    .api
                    .publish_select(SelectId::MusicViz, self.viz.label());
            }
            ApiEvent::Select(id, v) => self.fx.api.publish_select(id, &v),
            ApiEvent::MdnsTxtChanged(txt) => {
                if let Some(m) = &self.mdns {
                    m.set_txt(ESPHOME_SERVICE, txt);
                }
            }
            ApiEvent::Timer(t) => {
                if t.event == TimerEvent::Finished {
                    info!("timer '{}' finished", t.name);
                }
                let ev = match t.event {
                    TimerEvent::Started => HaTimerEvent::Started,
                    TimerEvent::Updated => HaTimerEvent::Updated,
                    TimerEvent::Cancelled => HaTimerEvent::Cancelled,
                    TimerEvent::Finished => HaTimerEvent::Finished,
                };
                self.dialog.on_ha_timer(ev, &mut self.fx);
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
        let volume = req.volume.filter(|&v| {
            let echo = self.fx.ha_echo.swallow(v, pulsar_core::now_ms());
            if echo {
                info!("media: HA volume {v} after a spoken level ignored");
            }
            !echo
        });
        let req = MediaRequest {
            command: req.command.map(media_cmd),
            volume,
            url: req.media_url,
            announcement: req.announcement == Some(true),
        };
        let sendspin = self.fx.sendspin_streaming();
        for action in media::media_actions(req, self.fx.music, self.fx.volume.shown(), sendspin) {
            match action {
                MediaAction::PlayMusic(url) => {
                    self.fx.player.play(Channel::Music, vec![url]);
                }
                MediaAction::Announce(url) => self.dialog.on_media_announce(url, &mut self.fx),
                MediaAction::SetVolume(v) => self.fx.set_volume(v),
                MediaAction::Resume => self.fx.player.resume(Channel::Music),
                MediaAction::Pause => self.fx.player.pause(Channel::Music),
                MediaAction::Stop => self.fx.player.stop(Channel::Music),
                MediaAction::SetMuted(m) => self.fx.set_muted(m),
                MediaAction::LeaveSendspin => {
                    if let Some(ss) = &self.fx.sendspin {
                        ss.yield_to_local();
                    }
                }
            }
        }
        self.fx.publish_media();
    }

    pub fn on_sendspin(&mut self, ev: SendspinEvent) {
        match ev {
            SendspinEvent::Stream(active) => {
                self.ownership
                    .on_sendspin_stream(active, self.fx.music, pulsar_core::now_ms());
                if media::sendspin_stops_music(active, self.fx.music) {
                    self.fx.player.stop(Channel::Music);
                }
                self.update_screen(active);
            }
            SendspinEvent::Volume(v) => self.fx.set_volume(v),
            SendspinEvent::Mute(m) => self.fx.set_muted(m),
            SendspinEvent::PairingWindow(open) => self.pairing_prompt = open,
            SendspinEvent::PairingCode(_) | SendspinEvent::Music(_) => return,
        }
        self.fx.publish_media();
    }

    fn on_switch(&mut self, id: SwitchId, on: bool) {
        match id {
            SwitchId::Microphones => {
                self.mics.set_switch(on);
                self.apply_mics();
                return;
            }
            SwitchId::VoiceIndication => {
                self.voice_indication = on;
                if let Some(r) = &self.fx.ring {
                    r.set_voice_indication(on);
                }
            }
            _ => {}
        }
        self.fx.api.publish_switch(id, on);
    }

    fn on_number(&mut self, id: NumberId, v: f32) {
        match id {
            NumberId::MicGain => {
                self.mic_gain_db = v.round() as u32;
                self.fx.api.publish_number(id, self.mic_gain_db as f32);
            }
            NumberId::CommandOffset => {
                let Some(ms) = pulsar_esphome::command_offset_ms(v) else {
                    return;
                };
                self.command_offset_ms = ms;
                self.fx.api.publish_number(id, ms as f32);
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
    /// io.sdk shows the volume on the ring by itself.
    fn foreign_draw(&self) {
        if let Some(r) = &self.ring {
            r.foreign_draw();
        }
        if let Some(s) = &self.screen {
            s.foreign_draw();
        }
    }

    fn sendspin_streaming(&self) -> bool {
        self.sendspin
            .as_ref()
            .is_some_and(SendspinHandle::streaming)
    }

    fn publish_media(&self) {
        let st = match media::shown_state(self.announcing, self.music, self.sendspin_streaming()) {
            Shown::Idle => MediaState::Idle,
            Shown::Playing => MediaState::Playing,
            Shown::Paused => MediaState::Paused,
            Shown::Announcing => MediaState::Announcing,
        };
        self.api
            .publish_media(st, self.volume.shown().unwrap_or(0.0), self.muted);
    }

    fn apply_gains(&self) {
        let g = media::channel_gains(self.muted, self.ducked);
        self.player.set_gain(Channel::Music, g.music);
        self.player.set_gain(Channel::Announce, g.announce);
        if let Some(ss) = &self.sendspin {
            ss.set_duck_gain(g.music);
        }
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gains();
        if let (Some(ss), Some(v)) = (&self.sendspin, self.volume.shown()) {
            ss.report_volume(v, muted);
        }
    }

    fn transport(&self, t: TransportCmd) {
        let t = match t {
            TransportCmd::Play => Transport::Play,
            TransportCmd::Pause => Transport::Pause,
            TransportCmd::Next => Transport::Next,
            TransportCmd::Previous => Transport::Previous,
        };
        if !self.sendspin.as_ref().is_some_and(|ss| ss.transport(t)) {
            info!("{t:?}: no Sendspin queue to act on");
        }
    }

    /// Shown once the helper reports STREAM_MUSIC back.
    fn set_volume(&mut self, v: f32) {
        self.player.set_volume(v);
        self.volume.request(v, pulsar_core::now_ms());
        self.foreign_draw();
    }
}

impl Effects for Fx {
    fn music(&mut self, action: MusicAction) {
        let state = MusicState {
            music: self.music,
            sendspin_streaming: self.sendspin_streaming(),
            volume: self.volume.shown(),
            muted: self.muted,
        };
        if state.volume.is_none() && matches!(action, MusicAction::Volume { .. }) {
            warn!("volume step: STREAM_MUSIC volume unknown");
        }
        if let MusicAction::Level { volume, .. } = action {
            self.ha_echo.expect(volume, pulsar_core::now_ms());
        }
        for op in music_ops(action, &state) {
            match op {
                MusicOp::PausePlayer => self.player.pause(Channel::Music),
                MusicOp::ResumePlayer => self.player.resume(Channel::Music),
                MusicOp::Transport(t) => self.transport(t),
                MusicOp::Unmute => self.set_muted(false),
                MusicOp::SetVolume { volume, click } => {
                    self.set_volume(volume);
                    if click && let Some(sound) = &self.volume_click {
                        self.player.play(Channel::Announce, vec![sound.clone()]);
                    }
                }
            }
        }
        self.publish_media();
    }

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
        self.timers.set(Tick::Dialog(timer), ms);
    }

    fn cancel(&mut self, timer: Timer) {
        self.timers.cancel(Tick::Dialog(timer));
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

    fn show(&mut self, ui: Ui) {
        if let Some(r) = &self.ring {
            r.show(ui);
        }
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

fn random() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_i64(pulsar_core::now_ms());
    h.finish()
}
