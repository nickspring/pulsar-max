use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{info, warn};
use max_core::screen::{FRAME_BYTES, Frame, ScreenOwner, Source};
use max_core::spectrogram::{Analyzer, Spectrogram, Spectrum, Style};
use max_core::spectrum_sync::{MusicChunk, MusicSource, TapFormat};

use crate::aaudio::monotonic_ns;

const FB_NAME: &str = "YNDX LED screen";
/// ~0.6 s of the capture's 20 ms chunks.
const QUEUE_CHUNKS: usize = 32;
const SPEAKER_RATE: u32 = 48_000;
const POLL: Duration = Duration::from_millis(100);
const GUARD_POLL: Duration = Duration::from_millis(1);

#[derive(Debug)]
enum Cmd {
    Show {
        style: Style,
        restart: bool,
        source: Source,
    },
    Hide,
    ForeignDraw,
}

enum Audio {
    Speaker(Vec<i16>),
    Music(MusicChunk),
}

pub struct ScreenHandle {
    tx: Option<Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
}

pub struct ReferenceFeed {
    tx: SyncSender<Audio>,
    active: Arc<AtomicBool>,
}

impl ReferenceFeed {
    pub fn wanted(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    pub fn send(&self, stereo: Vec<i16>) {
        let _ = self.tx.try_send(Audio::Speaker(stereo));
    }
}

/// Called on the audio thread: never waits.
pub struct MusicTap {
    tx: SyncSender<Audio>,
    active: Arc<AtomicBool>,
    format: Mutex<TapFormat>,
}

impl MusicTap {
    pub fn configure(&self, rate: u32, channels: u8, bits: u8) {
        let mut f = self
            .format
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *f = f.next(rate, channels, bits);
    }

    pub fn format(&self) -> Option<TapFormat> {
        if !self.active.load(Ordering::Relaxed) {
            return None;
        }
        self.format.try_lock().ok().map(|f| *f)
    }

    pub fn send(&self, before: TapFormat, taken: &[u8], finish_in_us: i64) {
        // A configure during the write: the bytes may belong to either stream.
        if self.format() != Some(before) {
            return;
        }
        if let Some(chunk) = before.chunk(taken, finish_in_us, monotonic_ns()) {
            let _ = self.tx.try_send(Audio::Music(chunk));
        }
    }
}

pub struct Inputs {
    rx: Receiver<Audio>,
    speaker: Arc<AtomicBool>,
    music: Arc<AtomicBool>,
}

pub fn feeds() -> (ReferenceFeed, MusicTap, Inputs) {
    let (tx, rx) = mpsc::sync_channel(QUEUE_CHUNKS);
    let speaker = Arc::new(AtomicBool::new(false));
    let music = Arc::new(AtomicBool::new(false));
    let reference = ReferenceFeed {
        tx: tx.clone(),
        active: speaker.clone(),
    };
    let tap = MusicTap {
        tx,
        active: music.clone(),
        format: Mutex::new(TapFormat::default()),
    };
    (reference, tap, Inputs { rx, speaker, music })
}

pub fn spawn(inputs: Inputs) -> Option<ScreenHandle> {
    let path = crate::ring::find_framebuffer(FB_NAME)?;
    let fb = match OpenOptions::new().read(true).write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            warn!("screen: {}: {e}", path.display());
            return None;
        }
    };
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("screen".into())
        .spawn(move || run(&fb, &rx, &inputs))
        .map_err(|e| warn!("screen: thread: {e}"))
        .ok()?;
    info!("screen: {}", path.display());
    Some(ScreenHandle {
        tx: Some(tx),
        thread: Some(thread),
    })
}

impl ScreenHandle {
    pub fn show(&self, style: Style, restart: bool, source: Source) {
        self.send(Cmd::Show {
            style,
            restart,
            source,
        });
    }

    pub fn hide(&self) {
        self.send(Cmd::Hide);
    }

    pub fn foreign_draw(&self) {
        self.send(Cmd::ForeignDraw);
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(cmd);
        }
    }
}

impl Drop for ScreenHandle {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn now_ms(ns: i64) -> i64 {
    ns / 1_000_000
}

struct Screen<'a> {
    fb: &'a File,
    inputs: &'a Inputs,
    owner: ScreenOwner,
    /// Kept over a hide: a resume continues the picture.
    spectrogram: Option<Spectrogram>,
    source: Option<Source>,
    speaker: Analyzer,
    music: MusicSource,
    spectra: Vec<(u64, Spectrum)>,
    error_logged: bool,
    overflow_logged: bool,
}

impl Screen<'_> {
    fn read(&self) -> Frame {
        let mut f = [0; FRAME_BYTES];
        if let Err(e) = self.fb.read_exact_at(&mut f, 0) {
            warn!("screen: read: {e}");
        }
        f
    }

    fn write(&mut self, frame: &Frame) {
        match self.fb.write_at(frame, 0) {
            Ok(n) if n == frame.len() => self.error_logged = false,
            result => {
                if !self.error_logged {
                    warn!("screen: write: {result:?}");
                    self.error_logged = true;
                }
            }
        }
    }

    fn set_source(&mut self, source: Option<Source>) {
        if self.source == source {
            return;
        }
        self.source = source;
        self.inputs
            .speaker
            .store(source == Some(Source::Speaker), Ordering::Relaxed);
        self.inputs
            .music
            .store(source == Some(Source::Sendspin), Ordering::Relaxed);
        self.speaker.reset();
        self.music.stop();
        if source == Some(Source::Sendspin) {
            self.music.start(monotonic_ns());
        }
    }

    fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Show {
                style,
                restart,
                source,
            } => {
                if !self.owner.showing() {
                    let current = self.read();
                    self.owner.start(&current);
                }
                let s = self
                    .spectrogram
                    .get_or_insert_with(|| Spectrogram::new(style));
                s.set_style(style);
                if restart {
                    s.reset();
                    self.speaker.reset();
                }
                self.set_source(Some(source));
            }
            Cmd::Hide => {
                self.set_source(None);
                if let Some(clock) = self.owner.stop(now_ms(monotonic_ns())) {
                    self.write(&clock);
                }
            }
            Cmd::ForeignDraw => self.owner.foreign_draw(now_ms(monotonic_ns())),
        }
    }

    /// io.sdk's clock rewrite is undone within a poll, not at our next frame 40 ms later.
    fn guard_until(&mut self, end_ms: i64, tick_ns: Option<i64>) {
        loop {
            let current = self.read();
            let ns = monotonic_ns();
            if let Some(f) = self.owner.guard(now_ms(ns), &current) {
                self.write(&f);
                return;
            }
            if now_ms(ns) >= end_ms || tick_ns.is_some_and(|t| ns >= t) {
                return;
            }
            std::thread::sleep(GUARD_POLL);
        }
    }

    fn draw(&mut self, spectrum: Option<&Spectrum>) {
        let Some(s) = self.spectrogram.as_mut() else {
            return;
        };
        let next = s.render(spectrum);
        let current = self.read();
        if let Some(f) = self.owner.frame(now_ms(monotonic_ns()), &current, &next) {
            self.write(&f);
        }
    }

    fn audio(&mut self, audio: Audio) {
        match (audio, self.source) {
            (Audio::Speaker(chunk), Some(Source::Speaker)) => {
                let mut spectra = std::mem::take(&mut self.spectra);
                spectra.clear();
                self.speaker.push(&chunk, &mut spectra);
                for (_, s) in &spectra {
                    self.draw(Some(s));
                }
                self.spectra = spectra;
            }
            (Audio::Music(chunk), Some(Source::Sendspin)) => {
                let kept = self.music.push(&chunk);
                if !kept && !self.overflow_logged {
                    warn!("screen: spectrum queue overflow");
                }
                self.overflow_logged = !kept;
            }
            _ => {}
        }
    }

    fn tick(&mut self, now_ns: i64) {
        let spectrum = self.music.tick(now_ns);
        self.draw(spectrum.as_ref());
    }
}

fn run(fb: &File, cmds: &Receiver<Cmd>, inputs: &Inputs) {
    let mut screen = Screen {
        fb,
        inputs,
        owner: ScreenOwner::default(),
        spectrogram: None,
        source: None,
        speaker: Analyzer::new(SPEAKER_RATE),
        music: MusicSource::default(),
        spectra: Vec::new(),
        error_logged: false,
        overflow_logged: false,
    };
    let mut audio_open = true;
    loop {
        loop {
            match cmds.try_recv() {
                Ok(cmd) => screen.command(cmd),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    screen.command(Cmd::Hide);
                    return;
                }
            }
        }
        let ns = monotonic_ns();
        let tick = screen.music.next_tick_ns();
        if tick.is_some_and(|t| t <= ns) {
            screen.tick(ns);
            continue;
        }
        let ms = now_ms(ns);
        let watch = screen.owner.watch(ms);
        if let Some(w) = watch.as_ref().filter(|w| w.contains(&ms)) {
            screen.guard_until(w.end, tick);
            continue;
        }
        let until_watch = watch
            .filter(|w| w.start > ms)
            .map(|w| Duration::from_millis(u64::try_from(w.start - ms).unwrap_or(0)));
        let until_tick = tick.map(|t| Duration::from_nanos(u64::try_from(t - ns).unwrap_or(0)));
        let wait = [until_watch, until_tick]
            .into_iter()
            .flatten()
            .fold(POLL, Duration::min);
        if audio_open {
            match inputs.rx.recv_timeout(wait) {
                Ok(audio) => screen.audio(audio),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => audio_open = false,
            }
            continue;
        }
        match cmds.recv_timeout(wait) {
            Ok(cmd) => screen.command(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                screen.command(Cmd::Hide);
                return;
            }
        }
    }
}
