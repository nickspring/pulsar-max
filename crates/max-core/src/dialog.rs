use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Va {
    Error,
    RunStart,
    RunEnd,
    SttStart,
    SttEnd,
    IntentStart,
    IntentEnd,
    TtsStart,
    TtsEnd,
    WakeWordStart,
    WakeWordEnd,
    SttVadStart,
    SttVadEnd,
    TtsStreamStart,
    TtsStreamEnd,
    IntentProgress,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Listening,
    Processing,
    Speaking,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    Music,
    Announce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlayId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Timer {
    Watchdog,
    FollowupWait,
}

pub const LISTEN_TIMEOUT_MS: u64 = 30_000;
pub const PROCESS_TIMEOUT_MS: u64 = 120_000;
pub const FOLLOWUP_DURATION_MAX_S: u32 = 10;

// HA's VAD sends STT_VAD_START only after 0.3 s of speech.
const FOLLOWUP_VAD_GRACE_MS: u64 = 1000;

pub trait Effects {
    fn voice_subscribed(&self) -> bool;
    fn start_run(&mut self, phrase: &str) -> u64;
    fn stop_run(&mut self);
    fn answer_finished(&mut self, ok: bool);
    fn announce_finished(&mut self, ok: bool);
    fn set_streaming(&mut self, on: bool);
    /// The superseded play still gets `on_play_done(id, false)`.
    fn play(&mut self, ch: Channel, urls: Vec<String>) -> PlayId;
    fn stop_play(&mut self, ch: Channel);
    fn arm(&mut self, timer: Timer, ms: u64);
    fn cancel(&mut self, timer: Timer);
    fn set_duck(&mut self, on: bool);
    fn set_announcing(&mut self, on: bool);
}

/// Pipeline events carry no run id. HA ends a run it replaces with a RUN_END sent before the new
/// run's RUN_START, so until RUN_START every event belongs to the replaced run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RunBinding {
    awaiting_run_start: bool,
    run_active: bool,
    stale_run_end: bool,
}

impl RunBinding {
    fn start(&mut self) {
        self.stale_run_end = self.run_active;
        self.awaiting_run_start = true;
    }

    fn accept(&mut self, ev: Va) -> bool {
        if ev == Va::RunStart {
            self.run_active = true;
            self.awaiting_run_start = false;
        } else if self.awaiting_run_start {
            if ev != Va::RunEnd || self.stale_run_end {
                if ev == Va::RunEnd {
                    self.stale_run_end = false;
                    self.run_active = false;
                }
                return false;
            }
            self.awaiting_run_start = false;
        } else if ev == Va::RunEnd {
            self.run_active = false;
        }
        true
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Followup {
    requested: bool,
    failed: bool,
    vad_seen: bool,
    awaiting_speech: bool,
    /// Only after a run that sent STT_VAD_START: without it nothing cancels the timer, and
    /// ESPHome does not tell the VAD type at STT_START.
    timed: bool,
}

impl Followup {
    fn on_event(&mut self, ev: Va, continue_conversation: bool) {
        match ev {
            Va::RunStart => {
                self.requested = false;
                self.failed = false;
                self.vad_seen = false;
            }
            Va::IntentEnd => self.requested = continue_conversation && !self.failed,
            Va::Error => {
                self.failed = true;
                self.requested = false;
            }
            Va::SttVadStart => {
                self.vad_seen = true;
                self.awaiting_speech = false;
            }
            _ => {}
        }
    }

    fn should_continue(&self, ok: bool, seconds: u32) -> bool {
        ok && self.requested && seconds > 0
    }

    fn next_turn(self) -> Self {
        Self {
            awaiting_speech: true,
            timed: self.vad_seen,
            ..Self::default()
        }
    }

    fn window_ms(&self, seconds: u32) -> Option<u64> {
        (self.awaiting_speech && self.timed && seconds > 0)
            .then(|| u64::from(seconds) * 1000 + FOLLOWUP_VAD_GRACE_MS)
    }
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    Tts { dialog: u64 },
    Announce { conv: bool, seq: u64 },
    MediaAnnounce { seq: u64 },
}

/// Port of pulsar's controller/voice.rs and controller/logic.rs (tag v0.6.1).
pub struct Dialog {
    phase: Phase,
    followup_s: u32,
    mic_enabled: bool,
    announcing: bool,
    dialog: u64,
    announce_seq: u64,
    run: RunBinding,
    voice_run: u64,
    stop_sent: bool,
    tts_url: String,
    tts_played: bool,
    tts_report_owed: bool,
    followup: Followup,
    pending: HashMap<PlayId, Pending>,
}

impl Dialog {
    pub fn new(followup_s: u32) -> Self {
        Self {
            phase: Phase::Idle,
            followup_s: followup_s.min(FOLLOWUP_DURATION_MAX_S),
            mic_enabled: true,
            announcing: false,
            dialog: 0,
            announce_seq: 0,
            run: RunBinding::default(),
            voice_run: 0,
            stop_sent: false,
            tts_url: String::new(),
            tts_played: false,
            tts_report_owed: false,
            followup: Followup::default(),
            pending: HashMap::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn announcing(&self) -> bool {
        self.announcing
    }

    pub fn set_followup_duration(&mut self, seconds: u32, fx: &mut impl Effects) {
        self.followup_s = seconds.min(FOLLOWUP_DURATION_MAX_S);
        if self.followup_s == 0 && self.awaiting_followup_speech() {
            self.end_interaction(fx);
        }
    }

    pub fn set_mic_enabled(&mut self, on: bool, fx: &mut impl Effects) {
        self.mic_enabled = on;
        if !on && self.phase == Phase::Listening {
            self.end_interaction(fx);
        }
    }

    pub fn button(&mut self, fx: &mut impl Effects) {
        if self.phase == Phase::Idle && !self.announcing {
            self.start("", fx);
            return;
        }
        self.interrupt_speech(fx);
        self.end_interaction(fx);
    }

    pub fn start(&mut self, phrase: &str, fx: &mut impl Effects) -> bool {
        if !self.can_listen(fx) {
            return false;
        }
        if self.phase == Phase::Speaking || self.announcing {
            self.interrupt_speech(fx);
        }
        self.listen(phrase, fx);
        true
    }

    pub fn on_va(&mut self, ev: Va, data: &HashMap<String, String>, fx: &mut impl Effects) {
        let get = |k: &str| data.get(k).map_or("", String::as_str);
        if !self.run.accept(ev) {
            return;
        }
        if self.phase == Phase::Idle {
            // A TTS_START that got out before our stop still leaves HA waiting for the report.
            if ev == Va::TtsStart {
                self.tts_report_owed = true;
                self.send_tts_report(false, fx);
            }
            return;
        }
        self.followup
            .on_event(ev, get("continue_conversation") == "1");
        match ev {
            Va::RunStart => {
                self.tts_url = get("url").to_owned();
                self.tts_played = false;
            }
            Va::SttStart => {
                if self.phase == Phase::Listening
                    && let Some(ms) = self.followup.window_ms(self.followup_s)
                {
                    fx.arm(Timer::FollowupWait, ms);
                }
            }
            Va::SttVadStart => fx.cancel(Timer::FollowupWait),
            Va::SttVadEnd | Va::SttEnd => self.stop_streaming(fx),
            Va::IntentStart => {
                if self.phase == Phase::Listening {
                    self.stop_streaming(fx);
                }
            }
            Va::IntentProgress => {
                if get("tts_start_streaming") == "1" {
                    self.play_tts(fx);
                }
            }
            Va::TtsStart => self.tts_report_owed = true,
            Va::TtsEnd => {
                let url = get("url");
                if !url.is_empty() {
                    self.tts_url = url.to_owned();
                }
                self.play_tts(fx);
            }
            // pulsar tells error codes apart only for its error ring and earcons.
            Va::Error => self.stop_streaming(fx),
            Va::RunEnd => {
                self.stop_streaming(fx);
                if !self.tts_played {
                    self.end_interaction(fx);
                }
            }
            _ => {}
        }
        self.arm_watchdog(fx);
    }

    pub fn on_start_failed(&mut self, run: u64, fx: &mut impl Effects) {
        if !self.is_current_run(run) {
            return;
        }
        self.run.awaiting_run_start = false;
        self.end_interaction(fx);
    }

    /// The server already sent the stop.
    pub fn on_audio_overflow(&mut self, run: u64, fx: &mut impl Effects) {
        if !self.is_current_run(run) {
            return;
        }
        self.stop_sent = true;
        self.end_interaction(fx);
    }

    pub fn on_link_lost(&mut self, fx: &mut impl Effects) {
        self.run = RunBinding::default();
        self.followup = Followup::default();
        // The owed report would reach the next client and could end its announcement's wait.
        self.tts_report_owed = false;
        // The next client did not send these announcements: no AnnounceFinished or conversation.
        for p in self.pending.values_mut() {
            if let Pending::Announce { seq, .. } = *p {
                *p = Pending::MediaAnnounce { seq };
            }
        }
        if matches!(self.phase, Phase::Listening | Phase::Processing) {
            self.end_interaction(fx);
        }
    }

    pub fn on_announce(
        &mut self,
        urls: Vec<String>,
        start_conversation: bool,
        fx: &mut impl Effects,
    ) {
        let seq = self.begin_announce(fx);
        let id = fx.play(Channel::Announce, urls);
        self.pending.insert(
            id,
            Pending::Announce {
                conv: start_conversation,
                seq,
            },
        );
    }

    /// Media player announcement: no AnnounceFinished, HA follows the media state.
    pub fn on_media_announce(&mut self, url: String, fx: &mut impl Effects) {
        let seq = self.begin_announce(fx);
        let id = fx.play(Channel::Announce, vec![url]);
        self.pending.insert(id, Pending::MediaAnnounce { seq });
    }

    pub fn on_play_done(&mut self, id: PlayId, ok: bool, fx: &mut impl Effects) {
        let Some(p) = self.pending.remove(&id) else {
            return;
        };
        match p {
            Pending::Tts { dialog } => {
                if dialog == self.dialog && self.phase == Phase::Speaking {
                    self.tts_finished(ok, fx);
                }
            }
            Pending::Announce { conv, seq } => {
                fx.announce_finished(ok);
                if seq == self.announce_seq {
                    self.announcing = false;
                    if ok && conv && self.phase == Phase::Idle {
                        self.start("", fx);
                    }
                    self.end_announce(fx);
                }
            }
            Pending::MediaAnnounce { seq } => {
                if seq == self.announce_seq {
                    self.announcing = false;
                    self.end_announce(fx);
                }
            }
        }
    }

    pub fn on_timer(&mut self, timer: Timer, fx: &mut impl Effects) {
        // Effects does not promise that a cancelled timer never fires.
        let live = match timer {
            Timer::Watchdog => matches!(self.phase, Phase::Listening | Phase::Processing),
            Timer::FollowupWait => self.awaiting_followup_speech(),
        };
        if live {
            self.end_interaction(fx);
        }
    }

    fn can_listen(&self, fx: &impl Effects) -> bool {
        fx.voice_subscribed() && self.mic_enabled
    }

    fn listen(&mut self, phrase: &str, fx: &mut impl Effects) {
        fx.cancel(Timer::FollowupWait);
        self.phase = Phase::Listening;
        self.dialog += 1;
        self.run.start();
        self.stop_sent = false;
        self.tts_url.clear();
        self.tts_played = false;
        self.followup = Followup::default();
        self.update_duck(fx);
        // HA applies AnnounceFinished by forcing idle: an answer's report must not arrive over the
        // new dialog's listening state.
        self.send_tts_report(false, fx);
        self.voice_run = fx.start_run(phrase);
        fx.set_streaming(true);
        self.arm_watchdog(fx);
    }

    fn is_current_run(&self, run: u64) -> bool {
        run == self.voice_run && matches!(self.phase, Phase::Listening | Phase::Processing)
    }

    fn awaiting_followup_speech(&self) -> bool {
        self.phase == Phase::Listening && self.followup.awaiting_speech
    }

    fn stop_streaming(&mut self, fx: &mut impl Effects) {
        fx.cancel(Timer::FollowupWait);
        fx.set_streaming(false);
        if self.phase == Phase::Listening {
            self.phase = Phase::Processing;
        }
    }

    fn play_tts(&mut self, fx: &mut impl Effects) {
        if self.tts_played || self.tts_url.is_empty() {
            return;
        }
        // pulsar forwards audio only while LISTENING; here streaming is the only gate.
        if self.phase == Phase::Listening {
            self.stop_streaming(fx);
        }
        self.tts_played = true;
        self.phase = Phase::Speaking;
        let id = fx.play(Channel::Announce, vec![self.tts_url.clone()]);
        self.pending.insert(
            id,
            Pending::Tts {
                dialog: self.dialog,
            },
        );
    }

    fn tts_finished(&mut self, ok: bool, fx: &mut impl Effects) {
        self.send_tts_report(ok, fx);
        let followup = self.followup;
        // Not start(): it would cut an announcement that began after the answer. Unlike pulsar, a
        // refused follow-up start ends the dialog instead of leaving it SPEAKING.
        if followup.should_continue(ok, self.followup_s) && self.can_listen(fx) {
            self.listen("", fx);
            self.followup = followup.next_turn();
            return;
        }
        self.end_interaction(fx);
    }

    // Not while an HA announcement is pending: its own AnnounceFinished ends HA's wait too, and
    // one more would end the announcement's wait early.
    fn send_tts_report(&mut self, ok: bool, fx: &mut impl Effects) {
        if !std::mem::take(&mut self.tts_report_owed)
            || self
                .pending
                .values()
                .any(|p| matches!(p, Pending::Announce { .. }))
        {
            return;
        }
        fx.answer_finished(ok);
    }

    fn end_interaction(&mut self, fx: &mut impl Effects) {
        fx.cancel(Timer::FollowupWait);
        if (self.run.run_active || self.run.awaiting_run_start) && !self.stop_sent {
            fx.stop_run();
            self.stop_sent = true;
        }
        self.run.awaiting_run_start = false;
        self.send_tts_report(false, fx);
        fx.set_streaming(false);
        self.phase = Phase::Idle;
        self.dialog += 1;
        self.tts_played = false;
        self.tts_url.clear();
        self.followup = Followup::default();
        self.update_duck(fx);
        self.arm_watchdog(fx);
    }

    fn arm_watchdog(&self, fx: &mut impl Effects) {
        match self.phase {
            Phase::Listening => fx.arm(Timer::Watchdog, LISTEN_TIMEOUT_MS),
            Phase::Processing => fx.arm(Timer::Watchdog, PROCESS_TIMEOUT_MS),
            Phase::Speaking | Phase::Idle => fx.cancel(Timer::Watchdog),
        }
    }

    fn interrupt_speech(&mut self, fx: &mut impl Effects) {
        fx.stop_play(Channel::Announce);
        if self.announcing {
            self.announcing = false;
            self.announce_seq += 1;
            fx.set_announcing(false);
        }
    }

    fn begin_announce(&mut self, fx: &mut impl Effects) -> u64 {
        self.announcing = true;
        self.announce_seq += 1;
        self.update_duck(fx);
        fx.set_announcing(true);
        self.announce_seq
    }

    fn end_announce(&self, fx: &mut impl Effects) {
        self.update_duck(fx);
        fx.set_announcing(false);
    }

    fn update_duck(&self, fx: &mut impl Effects) {
        fx.set_duck(self.phase != Phase::Idle || self.announcing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        StartRun(String),
        StopRun,
        AnswerFinished(bool),
        AnnounceFinished(bool),
        Streaming(bool),
        Play(Channel, Vec<String>),
        StopPlay(Channel),
        Arm(Timer, u64),
        Cancel(Timer),
        Duck(bool),
        Announcing(bool),
    }

    struct Fx {
        subscribed: bool,
        calls: Vec<Call>,
        runs: u64,
        plays: u64,
    }

    impl Fx {
        fn index(&self, call: &Call) -> Option<usize> {
            self.calls.iter().position(|c| c == call)
        }

        fn count(&self, call: &Call) -> usize {
            self.calls.iter().filter(|c| *c == call).count()
        }

        fn stops(&self) -> usize {
            self.count(&Call::StopRun)
        }

        fn answers(&self) -> Vec<bool> {
            self.calls
                .iter()
                .filter_map(|c| match c {
                    Call::AnswerFinished(ok) => Some(*ok),
                    _ => None,
                })
                .collect()
        }

        fn acks(&self) -> Vec<bool> {
            self.calls
                .iter()
                .filter_map(|c| match c {
                    Call::AnnounceFinished(ok) => Some(*ok),
                    _ => None,
                })
                .collect()
        }

        fn runs_started(&self) -> Vec<&str> {
            self.calls
                .iter()
                .filter_map(|c| match c {
                    Call::StartRun(p) => Some(p.as_str()),
                    _ => None,
                })
                .collect()
        }

        fn plays(&self) -> Vec<(Channel, Vec<String>)> {
            self.calls
                .iter()
                .filter_map(|c| match c {
                    Call::Play(ch, urls) => Some((*ch, urls.clone())),
                    _ => None,
                })
                .collect()
        }

        fn last_flag(&self, pick: fn(&Call) -> Option<bool>) -> Option<bool> {
            self.calls.iter().rev().find_map(pick)
        }

        fn streaming(&self) -> Option<bool> {
            self.last_flag(|c| match c {
                Call::Streaming(on) => Some(*on),
                _ => None,
            })
        }

        fn duck(&self) -> Option<bool> {
            self.last_flag(|c| match c {
                Call::Duck(on) => Some(*on),
                _ => None,
            })
        }

        fn shown_announcing(&self) -> Option<bool> {
            self.last_flag(|c| match c {
                Call::Announcing(on) => Some(*on),
                _ => None,
            })
        }

        fn armed(&self, timer: Timer) -> Option<u64> {
            self.calls
                .iter()
                .rev()
                .find_map(|c| match c {
                    Call::Arm(t, ms) if *t == timer => Some(Some(*ms)),
                    Call::Cancel(t) if *t == timer => Some(None),
                    _ => None,
                })
                .flatten()
        }

        fn last_play(&self) -> PlayId {
            PlayId(self.plays)
        }
    }

    impl Effects for Fx {
        fn voice_subscribed(&self) -> bool {
            self.subscribed
        }
        fn start_run(&mut self, phrase: &str) -> u64 {
            self.runs += 1;
            self.calls.push(Call::StartRun(phrase.to_owned()));
            self.runs
        }
        fn stop_run(&mut self) {
            self.calls.push(Call::StopRun);
        }
        fn answer_finished(&mut self, ok: bool) {
            self.calls.push(Call::AnswerFinished(ok));
        }
        fn announce_finished(&mut self, ok: bool) {
            self.calls.push(Call::AnnounceFinished(ok));
        }
        fn set_streaming(&mut self, on: bool) {
            self.calls.push(Call::Streaming(on));
        }
        fn play(&mut self, ch: Channel, urls: Vec<String>) -> PlayId {
            self.plays += 1;
            self.calls.push(Call::Play(ch, urls));
            PlayId(self.plays)
        }
        fn stop_play(&mut self, ch: Channel) {
            self.calls.push(Call::StopPlay(ch));
        }
        fn arm(&mut self, timer: Timer, ms: u64) {
            self.calls.push(Call::Arm(timer, ms));
        }
        fn cancel(&mut self, timer: Timer) {
            self.calls.push(Call::Cancel(timer));
        }
        fn set_duck(&mut self, on: bool) {
            self.calls.push(Call::Duck(on));
        }
        fn set_announcing(&mut self, on: bool) {
            self.calls.push(Call::Announcing(on));
        }
    }

    const URL: &str = "http://ha.local/tts/answer.flac";

    struct Rig {
        d: Dialog,
        fx: Fx,
    }

    impl Rig {
        fn new(followup_s: u32) -> Self {
            Self {
                d: Dialog::new(followup_s),
                fx: Fx {
                    subscribed: true,
                    calls: Vec::new(),
                    runs: 0,
                    plays: 0,
                },
            }
        }

        fn phase(&self) -> Phase {
            self.d.phase()
        }

        fn ev(&mut self, ev: Va, kv: &[(&str, &str)]) {
            let data = kv
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            self.d.on_va(ev, &data, &mut self.fx);
        }

        fn start(&mut self, phrase: &str) -> bool {
            self.d.start(phrase, &mut self.fx)
        }

        fn button(&mut self) {
            self.d.button(&mut self.fx);
        }

        fn timer(&mut self, t: Timer) {
            self.d.on_timer(t, &mut self.fx);
        }

        fn done(&mut self, id: PlayId, ok: bool) {
            self.d.on_play_done(id, ok, &mut self.fx);
        }

        fn announce(&mut self, conv: bool) -> PlayId {
            self.d
                .on_announce(vec!["pre".into(), "media".into()], conv, &mut self.fx);
            self.fx.last_play()
        }

        fn clear(&mut self) {
            self.fx.calls.clear();
        }

        fn listening(followup_s: u32) -> Self {
            let mut r = Self::new(followup_s);
            assert!(r.start(""));
            r.ev(Va::RunStart, &[("url", URL)]);
            r
        }

        fn processing() -> Self {
            let mut r = Self::listening(5);
            r.ev(Va::SttEnd, &[]);
            assert_eq!(r.phase(), Phase::Processing);
            r
        }

        fn answer(&mut self, vad: bool, continue_conversation: bool) -> PlayId {
            self.ev(Va::RunStart, &[("url", URL)]);
            self.ev(Va::SttStart, &[]);
            if vad {
                self.ev(Va::SttVadStart, &[]);
            }
            self.ev(Va::SttVadEnd, &[]);
            self.ev(Va::SttEnd, &[]);
            self.ev(Va::IntentStart, &[]);
            let cont = if continue_conversation { "1" } else { "0" };
            self.ev(Va::IntentEnd, &[("continue_conversation", cont)]);
            self.ev(Va::TtsStart, &[]);
            self.ev(Va::TtsEnd, &[]);
            assert_eq!(self.phase(), Phase::Speaking);
            self.fx.last_play()
        }

        fn speaking(followup_s: u32, continue_conversation: bool) -> (Self, PlayId) {
            let mut r = Self::new(followup_s);
            assert!(r.start(""));
            let tts = r.answer(true, continue_conversation);
            (r, tts)
        }

        fn followup_turn() -> Self {
            let (mut r, tts) = Self::speaking(5, true);
            r.done(tts, true);
            assert_eq!(r.phase(), Phase::Listening);
            r.ev(Va::RunStart, &[("url", URL)]);
            r
        }

        fn assert_ended(&self) {
            assert_eq!(self.phase(), Phase::Idle);
            assert_eq!(self.fx.streaming(), Some(false));
            assert_eq!(self.fx.duck(), Some(false));
            assert_eq!(self.fx.armed(Timer::Watchdog), None);
            assert_eq!(self.fx.armed(Timer::FollowupWait), None);
        }
    }

    #[test]
    fn run_binding_table() {
        #[derive(Clone, Copy)]
        enum Step {
            Start,
            Ev(Va, bool),
            Reset,
        }
        use Step::*;
        use Va::*;
        let b = |awaiting_run_start, run_active, stale_run_end| RunBinding {
            awaiting_run_start,
            run_active,
            stale_run_end,
        };
        let scenarios: &[(&str, &[(Step, RunBinding)])] = &[
            (
                "normal run",
                &[
                    (Start, b(true, false, false)),
                    (Ev(RunStart, true), b(false, true, false)),
                    (Ev(SttEnd, true), b(false, true, false)),
                    (Ev(TtsEnd, true), b(false, true, false)),
                    (Ev(RunEnd, true), b(false, false, false)),
                ],
            ),
            (
                "barge-in: the replaced run's events and RUN_END come first",
                &[
                    (Start, b(true, false, false)),
                    (Ev(RunStart, true), b(false, true, false)),
                    (Start, b(true, true, true)),
                    (Ev(TtsEnd, false), b(true, true, true)),
                    (Ev(Other, false), b(true, true, true)),
                    (Ev(RunEnd, false), b(true, false, false)),
                    (Ev(RunStart, true), b(false, true, false)),
                    (Ev(SttEnd, true), b(false, true, false)),
                    (Ev(RunEnd, true), b(false, false, false)),
                ],
            ),
            (
                "HA ends the request without a run",
                &[
                    (Start, b(true, false, false)),
                    (Ev(RunEnd, true), b(false, false, false)),
                ],
            ),
            (
                "voice link lost",
                &[
                    (Start, b(true, false, false)),
                    (Ev(RunStart, true), b(false, true, false)),
                    (Start, b(true, true, true)),
                    (Reset, b(false, false, false)),
                    (Ev(RunEnd, true), b(false, false, false)),
                ],
            ),
            (
                "events with no dialog pass (the phase check drops them)",
                &[
                    (Ev(SttEnd, true), b(false, false, false)),
                    (Ev(RunStart, true), b(false, true, false)),
                ],
            ),
        ];
        for (name, steps) in scenarios {
            let mut rb = RunBinding::default();
            for (i, (step, want)) in steps.iter().enumerate() {
                match *step {
                    Start => rb.start(),
                    Reset => rb = RunBinding::default(),
                    Ev(ev, accepted) => assert_eq!(rb.accept(ev), accepted, "{name}: step {i}"),
                }
                assert_eq!(rb, *want, "{name}: step {i}");
            }
        }
    }

    #[test]
    fn followup_needs_ha_request_success_and_nonzero_duration() {
        let mut f = Followup::default();
        assert!(!f.should_continue(true, 10));
        f.on_event(Va::IntentEnd, true);
        assert!(f.should_continue(true, 10));
        assert!(!f.should_continue(true, 0));
        assert!(!f.should_continue(false, 10));
        f.on_event(Va::IntentEnd, false);
        assert!(!f.should_continue(true, 10));
    }

    #[test]
    fn followup_late_intent_end_does_not_undo_an_error() {
        let mut f = Followup::default();
        f.on_event(Va::IntentEnd, true);
        f.on_event(Va::Error, false);
        assert!(!f.should_continue(true, 10));
        f.on_event(Va::IntentEnd, true);
        assert!(!f.should_continue(true, 10));
        f.on_event(Va::RunStart, false);
        f.on_event(Va::IntentEnd, true);
        assert!(f.should_continue(true, 10));
    }

    #[test]
    fn followup_window_needs_vad_events_of_the_previous_run() {
        assert_eq!(Followup::default().window_ms(7), None);

        let mut f = Followup::default();
        for ev in [Va::RunStart, Va::SttStart, Va::SttVadStart, Va::IntentEnd] {
            f.on_event(ev, true);
        }
        let mut f = f.next_turn();
        f.on_event(Va::RunStart, false);
        assert_eq!(f.window_ms(7), Some(8_000));
        assert_eq!(f.window_ms(0), None);
        f.on_event(Va::SttVadStart, false);
        assert_eq!(f.window_ms(7), None);
        assert!(!f.awaiting_speech);

        let mut f = Followup::default();
        for ev in [Va::RunStart, Va::SttStart, Va::IntentEnd] {
            f.on_event(ev, true);
        }
        let mut f = f.next_turn();
        f.on_event(Va::RunStart, false);
        assert_eq!(f.window_ms(7), None);
        assert!(f.awaiting_speech);
    }

    #[test]
    fn start_needs_voice_subscription() {
        let mut r = Rig::new(5);
        r.fx.subscribed = false;
        assert!(!r.start("alice"));
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.calls.is_empty());
    }

    #[test]
    fn start_needs_enabled_mic() {
        let mut r = Rig::new(5);
        r.d.set_mic_enabled(false, &mut r.fx);
        assert!(!r.start("alice"));
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.runs_started().is_empty());
        r.d.set_mic_enabled(true, &mut r.fx);
        assert!(r.start("alice"));
        assert_eq!(r.phase(), Phase::Listening);
    }

    #[test]
    fn start_listens_streams_ducks_and_arms_the_listen_watchdog() {
        let mut r = Rig::new(5);
        assert!(r.start("alice"));
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.runs_started(), vec!["alice"]);
        assert_eq!(r.fx.streaming(), Some(true));
        assert_eq!(r.fx.duck(), Some(true));
        assert_eq!(r.fx.armed(Timer::Watchdog), Some(LISTEN_TIMEOUT_MS));
        assert!(r.fx.answers().is_empty());
    }

    #[test]
    fn owed_answer_report_goes_out_before_the_next_start() {
        let (mut r, tts) = Rig::speaking(5, false);
        r.clear();
        assert!(r.start("alice"));
        assert_eq!(r.fx.answers(), vec![false]);
        assert!(
            r.fx.index(&Call::AnswerFinished(false)) < r.fx.index(&Call::StartRun("alice".into()))
        );
        r.done(tts, false);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.answers(), vec![false]);
    }

    #[test]
    fn events_before_run_start_are_dropped() {
        let mut r = Rig::new(5);
        r.start("");
        r.clear();
        r.ev(Va::SttEnd, &[]);
        r.ev(Va::TtsEnd, &[("url", URL)]);
        assert_eq!(r.phase(), Phase::Listening);
        assert!(r.fx.calls.is_empty());
        r.ev(Va::RunStart, &[("url", URL)]);
        r.ev(Va::SttEnd, &[]);
        assert_eq!(r.phase(), Phase::Processing);
        assert_eq!(r.fx.streaming(), Some(false));
        assert_eq!(r.fx.armed(Timer::Watchdog), Some(PROCESS_TIMEOUT_MS));
    }

    #[test]
    fn start_while_speaking_cuts_the_answer_short() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.clear();
        assert!(r.start("alice"));
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 1);
        assert!(
            r.fx.index(&Call::StopPlay(Channel::Announce))
                < r.fx.index(&Call::StartRun("alice".into()))
        );
        assert_eq!(r.fx.duck(), Some(true));
        r.done(tts, true);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.runs_started(), vec!["alice"]);
        assert_eq!(r.fx.answers(), vec![false]);
    }

    #[test]
    fn start_during_an_announcement_ends_it() {
        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.clear();
        assert!(r.start("alice"));
        assert_eq!(r.phase(), Phase::Listening);
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 1);
        assert!(
            r.fx.index(&Call::StopPlay(Channel::Announce))
                < r.fx.index(&Call::StartRun("alice".into()))
        );
        assert_eq!(r.fx.duck(), Some(true));
        r.done(ann, true);
        assert_eq!(r.fx.acks(), vec![true]);
        assert_eq!(r.fx.runs_started(), vec!["alice"]);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.duck(), Some(true));
        assert_eq!(r.fx.shown_announcing(), Some(false));
    }

    #[test]
    fn refused_start_leaves_the_answer_playing() {
        let (mut r, _) = Rig::speaking(5, false);
        r.fx.subscribed = false;
        r.clear();
        assert!(!r.start("alice"));
        assert!(r.fx.calls.is_empty());
        assert_eq!(r.phase(), Phase::Speaking);
    }

    #[test]
    fn run_end_of_a_replaced_run_does_not_end_the_new_dialog() {
        let (mut r, _) = Rig::speaking(5, false);
        assert!(r.start("again"));
        r.ev(Va::RunEnd, &[]);
        assert_eq!(r.phase(), Phase::Listening);
        r.ev(Va::RunStart, &[]);
        r.ev(Va::RunEnd, &[]);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn run_end_without_run_start_ends_a_request_ha_did_not_run() {
        let mut r = Rig::new(5);
        r.start("");
        r.ev(Va::RunEnd, &[]);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn idle_drops_events_of_the_stopped_run_but_reports_its_tts_start() {
        let mut r = Rig::listening(5);
        r.button();
        assert_eq!(r.fx.stops(), 1);
        r.clear();
        r.ev(Va::SttEnd, &[]);
        r.ev(Va::TtsEnd, &[("url", URL)]);
        assert!(r.fx.calls.is_empty());
        r.ev(Va::TtsStart, &[]);
        assert_eq!(r.fx.calls, vec![Call::AnswerFinished(false)]);
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn idle_tts_start_is_not_reported_while_an_announcement_is_pending() {
        let mut r = Rig::listening(5);
        r.button();
        let ann = r.announce(false);
        r.ev(Va::TtsStart, &[]);
        assert!(r.fx.answers().is_empty());
        r.done(ann, true);
        assert_eq!(r.fx.acks(), vec![true]);
        assert!(r.fx.answers().is_empty());
    }

    #[test]
    fn speech_end_events_stop_streaming() {
        for ev in [Va::SttVadEnd, Va::SttEnd, Va::IntentStart] {
            let mut r = Rig::listening(5);
            r.ev(ev, &[]);
            assert_eq!(r.phase(), Phase::Processing, "{ev:?}");
            assert_eq!(r.fx.streaming(), Some(false), "{ev:?}");
            assert_eq!(r.fx.armed(Timer::Watchdog), Some(PROCESS_TIMEOUT_MS));
        }
    }

    #[test]
    fn intent_start_in_processing_only_rearms_the_watchdog() {
        let mut r = Rig::processing();
        r.clear();
        r.ev(Va::IntentStart, &[]);
        assert_eq!(r.phase(), Phase::Processing);
        assert_eq!(
            r.fx.calls,
            vec![Call::Arm(Timer::Watchdog, PROCESS_TIMEOUT_MS)]
        );
    }

    #[test]
    fn other_events_only_rearm_the_watchdog() {
        for ev in [
            Va::IntentEnd,
            Va::TtsStart,
            Va::WakeWordStart,
            Va::WakeWordEnd,
            Va::TtsStreamStart,
            Va::TtsStreamEnd,
            Va::Other,
        ] {
            let mut r = Rig::processing();
            r.clear();
            r.ev(ev, &[]);
            assert_eq!(r.phase(), Phase::Processing, "{ev:?}");
            assert_eq!(
                r.fx.calls,
                vec![Call::Arm(Timer::Watchdog, PROCESS_TIMEOUT_MS)],
                "{ev:?}"
            );
        }
    }

    #[test]
    fn tts_end_url_overrides_the_run_start_url() {
        let mut r = Rig::processing();
        r.ev(Va::TtsEnd, &[("url", "http://ha.local/tts/late.flac")]);
        assert_eq!(r.phase(), Phase::Speaking);
        assert_eq!(
            r.fx.plays(),
            vec![(
                Channel::Announce,
                vec!["http://ha.local/tts/late.flac".into()]
            )]
        );
        assert_eq!(r.fx.armed(Timer::Watchdog), None);
    }

    #[test]
    fn streamed_tts_plays_once_per_dialog() {
        let mut r = Rig::processing();
        r.ev(Va::IntentProgress, &[("tts_start_streaming", "1")]);
        assert_eq!(r.phase(), Phase::Speaking);
        r.ev(Va::IntentProgress, &[("tts_start_streaming", "1")]);
        r.ev(Va::TtsEnd, &[("url", "http://ha.local/tts/late.flac")]);
        assert_eq!(r.fx.plays(), vec![(Channel::Announce, vec![URL.into()])]);
    }

    #[test]
    fn intent_progress_without_streaming_flag_waits_for_tts_end() {
        let mut r = Rig::processing();
        r.ev(Va::IntentProgress, &[("tts_start_streaming", "0")]);
        r.ev(Va::IntentProgress, &[]);
        assert_eq!(r.phase(), Phase::Processing);
        assert!(r.fx.plays().is_empty());
        r.ev(Va::TtsEnd, &[]);
        assert_eq!(r.fx.plays(), vec![(Channel::Announce, vec![URL.into()])]);
    }

    #[test]
    fn run_without_tts_url_ends_at_run_end() {
        let mut r = Rig::new(5);
        r.start("");
        r.ev(Va::RunStart, &[]);
        r.ev(Va::SttEnd, &[]);
        r.ev(Va::TtsEnd, &[]);
        assert_eq!(r.phase(), Phase::Processing);
        assert!(r.fx.plays().is_empty());
        r.ev(Va::RunEnd, &[]);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn answer_without_speech_events_stops_streaming() {
        let mut r = Rig::listening(5);
        r.ev(Va::TtsEnd, &[]);
        assert_eq!(r.phase(), Phase::Speaking);
        assert_eq!(r.fx.streaming(), Some(false));
    }

    #[test]
    fn answer_end_reports_once_and_ends_the_dialog() {
        let (mut r, tts) = Rig::speaking(5, false);
        assert_eq!(r.fx.armed(Timer::Watchdog), None);
        r.ev(Va::RunEnd, &[]);
        assert_eq!(r.phase(), Phase::Speaking);
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.answers(), vec![true]);
        assert_eq!(r.fx.stops(), 0);
        r.done(tts, true);
        assert_eq!(r.fx.answers(), vec![true]);
    }

    #[test]
    fn answer_ending_before_run_end_stops_the_run_once() {
        let (mut r, tts) = Rig::speaking(5, false);
        r.done(tts, true);
        assert_eq!(r.fx.stops(), 1);
        r.announce(false);
        r.button();
        assert_eq!(r.fx.stops(), 1);
        assert_eq!(r.fx.answers(), vec![true]);
    }

    #[test]
    fn answer_report_is_dropped_while_an_announcement_is_pending() {
        let (mut r, tts) = Rig::speaking(5, false);
        let ann = r.announce(false);
        r.done(tts, false);
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.answers().is_empty());
        r.done(ann, true);
        assert_eq!(r.fx.acks(), vec![true]);
        assert!(r.fx.answers().is_empty());
        assert_eq!(r.fx.duck(), Some(false));
    }

    #[test]
    fn watchdog_follows_the_phase() {
        let mut r = Rig::new(5);
        r.start("");
        r.ev(Va::RunStart, &[("url", URL)]);
        assert_eq!(
            r.fx.count(&Call::Arm(Timer::Watchdog, LISTEN_TIMEOUT_MS)),
            2
        );
        r.ev(Va::SttEnd, &[]);
        assert_eq!(r.fx.armed(Timer::Watchdog), Some(PROCESS_TIMEOUT_MS));
        r.ev(Va::TtsEnd, &[]);
        assert_eq!(r.fx.armed(Timer::Watchdog), None);
        r.timer(Timer::Watchdog);
        assert_eq!(r.phase(), Phase::Speaking);
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn watchdog_aborts_a_dialog_waiting_for_its_run() {
        let mut r = Rig::new(5);
        r.start("");
        r.timer(Timer::Watchdog);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 1);
        r.ev(Va::RunStart, &[]);
        r.ev(Va::SttEnd, &[]);
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn watchdog_aborts_a_processing_dialog() {
        let mut r = Rig::processing();
        r.timer(Timer::Watchdog);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 1);
    }

    #[test]
    fn watchdog_fire_while_idle_is_ignored() {
        let mut r = Rig::new(5);
        r.timer(Timer::Watchdog);
        assert!(r.fx.calls.is_empty());
    }

    #[test]
    fn pipeline_errors_stop_streaming_and_wait_for_run_end() {
        for code in [
            "duplicate_wake_up_detected",
            "stt-no-text-recognized",
            "stt-stream-failed",
        ] {
            let mut r = Rig::listening(5);
            r.ev(Va::Error, &[("code", code)]);
            assert_eq!(r.phase(), Phase::Processing, "{code}");
            assert_eq!(r.fx.streaming(), Some(false), "{code}");
            assert_eq!(r.fx.armed(Timer::Watchdog), Some(PROCESS_TIMEOUT_MS));
            r.ev(Va::RunEnd, &[]);
            r.assert_ended();
            assert_eq!(r.fx.stops(), 0, "{code}");
        }
    }

    #[test]
    fn requested_follow_up_listens_again_without_phrase() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.clear();
        r.done(tts, true);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.answers(), vec![true]);
        assert_eq!(r.fx.runs_started(), vec![""]);
        assert!(r.fx.index(&Call::AnswerFinished(true)) < r.fx.index(&Call::StartRun("".into())));
        assert_eq!(r.fx.stops(), 0);
        assert_eq!(r.fx.streaming(), Some(true));
        assert_eq!(r.fx.duck(), Some(true));
        assert!(r.fx.plays().is_empty());

        r.ev(Va::RunEnd, &[]);
        assert_eq!(r.phase(), Phase::Listening);
        r.ev(Va::RunStart, &[("url", URL)]);
        r.ev(Va::SttStart, &[]);
        assert_eq!(r.fx.armed(Timer::FollowupWait), Some(6_000));
        r.ev(Va::SttVadStart, &[]);
        assert_eq!(r.fx.armed(Timer::FollowupWait), None);
        r.timer(Timer::FollowupWait);
        assert_eq!(r.phase(), Phase::Listening);
    }

    #[test]
    fn follow_up_leaves_an_announcement_that_began_after_the_answer_playing() {
        let (mut r, tts) = Rig::speaking(5, true);
        let ann = r.announce(false);
        r.done(tts, true);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 0);
        assert!(r.d.announcing());
        r.done(ann, true);
        assert_eq!(r.fx.acks(), vec![true]);
        assert!(!r.d.announcing());
        assert_eq!(r.phase(), Phase::Listening);
    }

    #[test]
    fn silent_follow_up_ends_when_the_wait_runs_out() {
        let mut r = Rig::followup_turn();
        r.ev(Va::SttStart, &[]);
        assert_eq!(r.fx.armed(Timer::FollowupWait), Some(6_000));
        r.timer(Timer::FollowupWait);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 1);
        assert_eq!(r.fx.answers(), vec![true]);
    }

    #[test]
    fn follow_up_wait_needs_vad_events_in_the_previous_run() {
        let mut r = Rig::new(5);
        r.start("");
        let tts = r.answer(false, true);
        r.done(tts, true);
        r.ev(Va::RunStart, &[]);
        r.ev(Va::SttStart, &[]);
        assert_eq!(r.phase(), Phase::Listening);
        assert!(
            !r.fx
                .calls
                .iter()
                .any(|c| matches!(c, Call::Arm(Timer::FollowupWait, _)))
        );
    }

    #[test]
    fn follow_up_window_needs_listening() {
        let mut r = Rig::followup_turn();
        r.ev(Va::SttEnd, &[]);
        r.ev(Va::SttStart, &[]);
        assert_eq!(r.fx.armed(Timer::FollowupWait), None);
        r.timer(Timer::FollowupWait);
        assert_eq!(r.phase(), Phase::Processing);
    }

    #[test]
    fn follow_up_silence_error_waits_for_run_end() {
        let mut r = Rig::followup_turn();
        r.ev(Va::SttStart, &[]);
        r.ev(Va::Error, &[("code", "stt-no-text-recognized")]);
        assert_eq!(r.phase(), Phase::Processing);
        assert_eq!(r.fx.armed(Timer::FollowupWait), None);
        r.ev(Va::RunEnd, &[]);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn follow_up_duration_is_capped() {
        let mut r = Rig::new(60);
        r.start("");
        let tts = r.answer(true, true);
        r.done(tts, true);
        r.ev(Va::RunStart, &[]);
        r.d.set_followup_duration(99, &mut r.fx);
        r.ev(Va::SttStart, &[]);
        assert_eq!(
            r.fx.armed(Timer::FollowupWait),
            Some(u64::from(FOLLOWUP_DURATION_MAX_S) * 1000 + FOLLOWUP_VAD_GRACE_MS)
        );
        assert_eq!(r.phase(), Phase::Listening);
    }

    #[test]
    fn follow_up_is_not_continued_after_a_failed_answer() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.done(tts, false);
        r.assert_ended();
        assert_eq!(r.fx.answers(), vec![false]);
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn follow_up_is_not_continued_unless_ha_asks() {
        let (mut r, tts) = Rig::speaking(5, false);
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn follow_up_is_not_continued_with_zero_duration() {
        let (mut r, tts) = Rig::speaking(0, true);
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn follow_up_is_not_continued_after_a_pipeline_error() {
        let mut r = Rig::listening(5);
        r.ev(Va::Error, &[("code", "intent-failed")]);
        r.ev(Va::IntentEnd, &[("continue_conversation", "1")]);
        r.ev(Va::TtsEnd, &[]);
        let tts = r.fx.last_play();
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn follow_up_is_not_continued_with_the_mic_disabled() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.d.set_mic_enabled(false, &mut r.fx);
        assert_eq!(r.phase(), Phase::Speaking);
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.answers(), vec![true]);
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn follow_up_refused_for_lost_subscription_ends_the_dialog() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.fx.subscribed = false;
        r.done(tts, true);
        r.assert_ended();
        assert_eq!(r.fx.runs_started().len(), 1);
        assert_eq!(r.fx.answers(), vec![true]);
    }

    #[test]
    fn turning_follow_up_off_ends_a_turn_waiting_for_speech() {
        let mut r = Rig::followup_turn();
        r.d.set_followup_duration(0, &mut r.fx);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 1);
    }

    #[test]
    fn turning_follow_up_off_leaves_other_dialogs_alone() {
        let mut r = Rig::listening(5);
        r.d.set_followup_duration(0, &mut r.fx);
        assert_eq!(r.phase(), Phase::Listening);

        let mut r = Rig::followup_turn();
        r.ev(Va::SttVadStart, &[]);
        r.d.set_followup_duration(0, &mut r.fx);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn start_failure_of_the_current_run_ends_the_dialog_without_stop() {
        let mut r = Rig::new(5);
        r.start("");
        r.d.on_start_failed(2, &mut r.fx);
        assert_eq!(r.phase(), Phase::Listening);
        r.d.on_start_failed(1, &mut r.fx);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn start_failure_in_processing_ends_the_dialog() {
        let mut r = Rig::processing();
        r.d.on_start_failed(1, &mut r.fx);
        r.assert_ended();
    }

    #[test]
    fn start_failure_after_the_dialog_ended_is_ignored() {
        let mut r = Rig::new(5);
        r.start("");
        r.ev(Va::RunEnd, &[]);
        r.clear();
        r.d.on_start_failed(1, &mut r.fx);
        assert!(r.fx.calls.is_empty());
    }

    #[test]
    fn audio_overflow_ends_the_dialog_without_a_second_stop() {
        let mut r = Rig::listening(5);
        r.d.on_audio_overflow(2, &mut r.fx);
        assert_eq!(r.phase(), Phase::Listening);
        r.d.on_audio_overflow(1, &mut r.fx);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn audio_overflow_of_a_replaced_run_is_ignored() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.done(tts, true);
        r.d.on_audio_overflow(1, &mut r.fx);
        assert_eq!(r.phase(), Phase::Listening);
        r.d.on_audio_overflow(2, &mut r.fx);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn audio_overflow_while_speaking_is_ignored() {
        let (mut r, _) = Rig::speaking(5, false);
        r.d.on_audio_overflow(1, &mut r.fx);
        assert_eq!(r.phase(), Phase::Speaking);
    }

    #[test]
    fn link_lost_ends_listening_and_processing_without_stop() {
        for mut r in [Rig::listening(5), Rig::processing()] {
            r.d.on_link_lost(&mut r.fx);
            r.assert_ended();
            assert_eq!(r.fx.stops(), 0);
        }
    }

    #[test]
    fn link_lost_lets_a_playing_answer_finish_without_report() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.d.on_link_lost(&mut r.fx);
        assert_eq!(r.phase(), Phase::Speaking);
        r.done(tts, true);
        r.assert_ended();
        assert!(r.fx.answers().is_empty());
        assert_eq!(r.fx.stops(), 0);
        assert_eq!(r.fx.runs_started().len(), 1);
    }

    #[test]
    fn link_lost_announcement_is_neither_acked_nor_continued() {
        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.d.on_link_lost(&mut r.fx);
        assert!(r.d.announcing());
        r.done(ann, true);
        assert!(r.fx.acks().is_empty());
        assert!(r.fx.runs_started().is_empty());
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.duck(), Some(false));
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn link_lost_announcement_does_not_hold_back_the_next_answer_report() {
        let mut r = Rig::new(5);
        let ann = r.announce(false);
        r.d.on_link_lost(&mut r.fx);
        assert!(r.start(""));
        let tts = r.answer(true, false);
        r.done(tts, true);
        assert_eq!(r.fx.answers(), vec![true]);
        r.done(ann, false);
        assert!(r.fx.acks().is_empty());
    }

    #[test]
    fn link_lost_while_idle_changes_nothing() {
        let mut r = Rig::new(5);
        r.d.on_link_lost(&mut r.fx);
        assert!(r.fx.calls.is_empty());
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn button_in_idle_starts_a_dialog_without_phrase() {
        let mut r = Rig::new(5);
        r.button();
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.runs_started(), vec![""]);
    }

    #[test]
    fn button_without_subscription_does_nothing() {
        let mut r = Rig::new(5);
        r.fx.subscribed = false;
        r.button();
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.calls.is_empty());
    }

    #[test]
    fn button_cancels_listening_and_processing() {
        for mut r in [Rig::listening(5), Rig::processing()] {
            r.button();
            r.assert_ended();
            assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 1);
            assert_eq!(r.fx.stops(), 1);
        }
    }

    #[test]
    fn button_cuts_the_answer_short() {
        let (mut r, tts) = Rig::speaking(5, true);
        r.button();
        r.assert_ended();
        assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 1);
        assert_eq!(r.fx.stops(), 1);
        assert_eq!(r.fx.answers(), vec![false]);
        r.done(tts, false);
        assert_eq!(r.fx.answers(), vec![false]);
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn button_stops_an_announcement_without_starting_a_dialog() {
        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.button();
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.count(&Call::StopPlay(Channel::Announce)), 1);
        r.assert_ended();
        r.done(ann, false);
        assert_eq!(r.fx.acks(), vec![false]);
        assert!(r.fx.runs_started().is_empty());
        assert_eq!(r.phase(), Phase::Idle);
    }

    #[test]
    fn disabling_the_mic_ends_only_a_listening_dialog() {
        let mut r = Rig::listening(5);
        r.d.set_mic_enabled(false, &mut r.fx);
        r.assert_ended();
        assert_eq!(r.fx.stops(), 1);

        let mut r = Rig::processing();
        r.d.set_mic_enabled(false, &mut r.fx);
        assert_eq!(r.phase(), Phase::Processing);
        assert_eq!(r.fx.stops(), 0);
    }

    #[test]
    fn announcement_plays_ducked_and_is_acked_once() {
        let mut r = Rig::new(5);
        let ann = r.announce(false);
        assert!(r.d.announcing());
        assert_eq!(r.phase(), Phase::Idle);
        assert_eq!(
            r.fx.plays(),
            vec![(Channel::Announce, vec!["pre".into(), "media".into()])]
        );
        assert_eq!(r.fx.shown_announcing(), Some(true));
        assert_eq!(r.fx.duck(), Some(true));
        r.done(ann, true);
        assert_eq!(r.fx.acks(), vec![true]);
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.duck(), Some(false));
        assert!(r.fx.runs_started().is_empty());
    }

    #[test]
    fn announcement_with_start_conversation_listens_after_it() {
        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.done(ann, true);
        assert_eq!(r.phase(), Phase::Listening);
        assert_eq!(r.fx.runs_started(), vec![""]);
        assert_eq!(r.fx.acks(), vec![true]);
        assert!(r.fx.index(&Call::AnnounceFinished(true)) < r.fx.index(&Call::StartRun("".into())));
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.duck(), Some(true));
    }

    #[test]
    fn announcement_conversation_needs_success_idle_and_mic() {
        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.done(ann, false);
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.runs_started().is_empty());

        let mut r = Rig::new(5);
        let ann = r.announce(true);
        r.d.set_mic_enabled(false, &mut r.fx);
        r.done(ann, true);
        assert_eq!(r.phase(), Phase::Idle);
        assert!(r.fx.runs_started().is_empty());

        let mut r = Rig::processing();
        let ann = r.announce(true);
        r.done(ann, true);
        assert_eq!(r.phase(), Phase::Processing);
        assert_eq!(r.fx.runs_started().len(), 1);
        assert_eq!(r.fx.duck(), Some(true));
    }

    #[test]
    fn superseded_announcement_is_acked_once_and_keeps_announcing() {
        let mut r = Rig::new(5);
        let first = r.announce(true);
        let second = r.announce(false);
        r.done(first, false);
        assert_eq!(r.fx.acks(), vec![false]);
        assert!(r.d.announcing());
        assert_eq!(r.fx.duck(), Some(true));
        r.done(second, true);
        assert_eq!(r.fx.acks(), vec![false, true]);
        assert!(!r.d.announcing());
        assert!(r.fx.runs_started().is_empty());
    }

    #[test]
    fn answer_superseding_an_announcement_ends_it() {
        let mut r = Rig::processing();
        let ann = r.announce(false);
        r.ev(Va::TtsEnd, &[]);
        r.done(ann, false);
        assert_eq!(r.fx.acks(), vec![false]);
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.phase(), Phase::Speaking);
        assert_eq!(r.fx.duck(), Some(true));
    }

    #[test]
    fn media_announcement_is_not_acked() {
        let mut r = Rig::new(5);
        r.d.on_media_announce("http://ha.local/doorbell.mp3".into(), &mut r.fx);
        let id = r.fx.last_play();
        assert!(r.d.announcing());
        assert_eq!(
            r.fx.plays(),
            vec![(
                Channel::Announce,
                vec!["http://ha.local/doorbell.mp3".into()]
            )]
        );
        assert_eq!(r.fx.shown_announcing(), Some(true));
        assert_eq!(r.fx.duck(), Some(true));
        r.done(id, true);
        assert!(r.fx.acks().is_empty());
        assert!(!r.d.announcing());
        assert_eq!(r.fx.shown_announcing(), Some(false));
        assert_eq!(r.fx.duck(), Some(false));
    }

    #[test]
    fn superseded_media_announcement_keeps_the_newer_one_announcing() {
        let mut r = Rig::new(5);
        r.d.on_media_announce("a".into(), &mut r.fx);
        let first = r.fx.last_play();
        r.d.on_media_announce("b".into(), &mut r.fx);
        let second = r.fx.last_play();
        r.done(first, false);
        assert!(r.d.announcing());
        r.done(second, true);
        assert!(!r.d.announcing());
        assert!(r.fx.acks().is_empty());
    }
}
