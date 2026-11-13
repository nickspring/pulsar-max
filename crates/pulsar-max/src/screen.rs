use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{info, warn};
use max_core::screen::{FRAME_BYTES, Frame, ScreenOwner, Source};
use max_core::spectrogram::{Analyzer, Spectrogram, Spectrum, Style};
use max_core::spectrum_sync::{MusicChunk, MusicSource, TapFormat};

use crate::aaudio::monotonic_ns;
use crate::sys::FbMap;

const FB_NAME: &str = "YNDX LED screen";
const SETTLE_NS: i64 = 200_000;
/// Two holder pans: they push our frame while the guard goes on watching.
const PUSH_NS: i64 = 3_500_000;
/// ~0.6 s of the capture's 20 ms chunks.
const QUEUE_CHUNKS: usize = 32;
const SPEAKER_RATE: u32 = 48_000;
const POLL: Duration = Duration::from_millis(100);
/// A clock hunt lasts seconds; between its slices the run loop serves the audio feeds.
const GUARD_SLICE_NS: i64 = 10_000_000;

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
    let fb = match FbMap::open(&path, FRAME_BYTES) {
        Ok(f) => f,
        Err(e) => {
            warn!("screen: {}: {e}", path.display());
            return None;
        }
    };
    let holder = LockHolder::spawn(&path);
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("screen".into())
        .spawn(move || run(fb, holder, &rx, &inputs))
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

/// Pans back to back while io.sdk's clock is due, so its pan waits until our frame is back in memory.
/// Two threads: the pan takes console_lock, a semaphore handed straight to a queued waiter.
struct LockHolder {
    until_ns: Arc<AtomicI64>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl LockHolder {
    fn spawn(path: &std::path::Path) -> Option<Self> {
        let until_ns = Arc::new(AtomicI64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for i in 0..2 {
            let fb = FbMap::open(path, FRAME_BYTES)
                .map_err(|e| warn!("screen: lock holder: {e}"))
                .ok()?;
            let (until, stopped) = (until_ns.clone(), stop.clone());
            let t = std::thread::Builder::new()
                .name(format!("screen-lock{i}"))
                .spawn(move || {
                    if let Err(e) = crate::sys::realtime_lowest() {
                        warn!("screen: lock holder priority: {e}");
                    }
                    while !stopped.load(Ordering::Relaxed) {
                        if monotonic_ns() < until.load(Ordering::Relaxed) {
                            let _ = fb.pan();
                        } else {
                            std::thread::park_timeout(POLL);
                        }
                    }
                })
                .map_err(|e| warn!("screen: lock holder: {e}"))
                .ok()?;
            threads.push(t);
        }
        Some(Self {
            until_ns,
            stop,
            threads,
        })
    }

    fn hold_until(&self, ns: i64) {
        self.until_ns.store(ns, Ordering::Relaxed);
        for t in &self.threads {
            t.thread().unpark();
        }
    }

    fn release(&self) {
        self.until_ns.store(0, Ordering::Relaxed);
    }

    fn holding(&self) -> bool {
        monotonic_ns() < self.until_ns.load(Ordering::Relaxed)
    }
}

impl Drop for LockHolder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

fn now_ms(ns: i64) -> i64 {
    ns / 1_000_000
}

struct Screen<'a> {
    fb: FbMap,
    inputs: &'a Inputs,
    owner: ScreenOwner,
    /// Kept over a hide: a resume continues the picture.
    spectrogram: Option<Spectrogram>,
    source: Option<Source>,
    speaker: Analyzer,
    music: MusicSource,
    spectra: Vec<(u64, Spectrum)>,
    error_logged: bool,
    in_memory: Frame,
    last_drawn: Frame,
    holder: Option<LockHolder>,
    overflow_logged: bool,
}

impl Screen<'_> {
    fn read(&mut self) -> Frame {
        let mut f = [0; FRAME_BYTES];
        self.fb.snapshot(&mut f);
        self.in_memory = f;
        f
    }

    fn write(&mut self, frame: &Frame) {
        self.in_memory = *frame;
        // Our own pan would queue behind the holders and leave io.sdk's memmove unwatched; theirs push it.
        if self.holder.as_ref().is_some_and(LockHolder::holding) {
            self.fb.store(frame);
            return;
        }
        match self.fb.show(frame) {
            Ok(()) => self.error_logged = false,
            Err(e) => {
                if !self.error_logged {
                    warn!("screen: pan: {e}");
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
                self.release_lock();
                self.set_source(None);
                if let Some(clock) = self.owner.stop(now_ms(monotonic_ns())) {
                    self.write(&clock);
                }
            }
            Cmd::ForeignDraw => self.owner.foreign_draw(now_ms(monotonic_ns())),
        }
    }

    fn release_lock(&self) {
        if let Some(h) = &self.holder {
            h.release();
        }
    }

    fn watching(&self, ms: i64) -> bool {
        self.owner.watch(ms).is_some_and(|w| w.contains(&ms))
    }

    /// io.sdk's memmove may still be landing when we see its first bytes.
    fn settle(&mut self, ours: &Frame) {
        let end = monotonic_ns() + SETTLE_NS;
        while monotonic_ns() < end {
            if self.fb.differs(ours) {
                self.fb.store(ours);
            }
            std::hint::spin_loop();
        }
    }

    /// Spins: our frame back in memory before io.sdk's pan copies it means the clock never shows.
    fn guard_until(&mut self, end_ns: i64, tick_ns: Option<i64>) {
        if let Some(h) = &self.holder {
            h.hold_until(end_ns);
        }
        loop {
            if self.fb.differs(&self.in_memory) {
                let mut current = [0; FRAME_BYTES];
                let took = self.in_memory == self.last_drawn;
                if took {
                    let ours = self.last_drawn;
                    self.fb.take_over(&mut current, &ours);
                } else {
                    self.fb.snapshot(&mut current);
                }
                let ms = now_ms(monotonic_ns());
                match self.owner.guard(ms, &current) {
                    Some(f) => {
                        self.settle(&f);
                        if let Some(h) = &self.holder {
                            h.hold_until(monotonic_ns() + PUSH_NS);
                        }
                        self.write(&f);
                        return;
                    }
                    None if took => self.write(&current),
                    None => self.in_memory = current,
                }
                if !self.watching(ms) {
                    self.release_lock();
                    return;
                }
            }
            let ns = monotonic_ns();
            if ns >= end_ns || tick_ns.is_some_and(|t| ns >= t) {
                return;
            }
            std::hint::spin_loop();
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
            self.last_drawn = f;
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

fn run(fb: FbMap, holder: Option<LockHolder>, cmds: &Receiver<Cmd>, inputs: &Inputs) {
    // A preempted guard lets the lock holder's next pan push io.sdk's clock itself.
    if let Err(e) = crate::sys::realtime_lowest() {
        warn!("screen: priority: {e}");
    }
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
        in_memory: [0; FRAME_BYTES],
        last_drawn: [0; FRAME_BYTES],
        holder,
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
            let end_ns = w.end * 1_000_000;
            // Audio waits out a known window: a draw there could meet the tick unguarded.
            if !screen.owner.hunting(ms) {
                screen.guard_until(end_ns, tick);
                continue;
            }
            if audio_open {
                match inputs.rx.try_recv() {
                    Ok(audio) => {
                        screen.audio(audio);
                        continue;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => audio_open = false,
                }
            }
            screen.guard_until(end_ns.min(ns + GUARD_SLICE_NS), tick);
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
