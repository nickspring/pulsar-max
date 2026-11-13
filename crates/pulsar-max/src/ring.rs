use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use max_core::dialog::Ui;
use max_core::ledpattern::{LEDS, Pattern};
use max_core::ring::{FILES, Output, Patterns, Ring};

const STOCK_PATTERNS: &str = "/data/data/com.yandex.io.sdk/files/system/ledpatterns";
const FB_NAME: &str = "YNDX LED ring";
const IDLE_WAIT: Duration = Duration::from_secs(1);

#[derive(Debug)]
enum Cmd {
    Show(Ui),
    Muted(bool),
    VoiceIndication(bool),
    ForeignDraw,
}

pub struct RingHandle {
    tx: Option<Sender<Cmd>>,
    thread: Option<JoinHandle<()>>,
}

pub fn spawn() -> Option<RingHandle> {
    let path = find_framebuffer()?;
    let fb = match OpenOptions::new().write(true).open(&path) {
        Ok(f) => f,
        Err(e) => {
            warn!("ring: {}: {e}", path.display());
            return None;
        }
    };
    let ring = Ring::new(load_patterns(Path::new(STOCK_PATTERNS)));
    let (tx, rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("ring".into())
        .spawn(move || run(ring, &fb, &rx))
        .map_err(|e| warn!("ring: thread: {e}"))
        .ok()?;
    info!("ring: {}", path.display());
    Some(RingHandle {
        tx: Some(tx),
        thread: Some(thread),
    })
}

impl RingHandle {
    pub fn show(&self, ui: Ui) {
        self.send(Cmd::Show(ui));
    }

    pub fn set_muted(&self, muted: bool) {
        self.send(Cmd::Muted(muted));
    }

    pub fn set_voice_indication(&self, on: bool) {
        self.send(Cmd::VoiceIndication(on));
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

impl Drop for RingHandle {
    fn drop(&mut self) {
        drop(self.tx.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn find_framebuffer() -> Option<PathBuf> {
    let dir = std::fs::read_dir("/sys/class/graphics").ok()?;
    let found = dir.filter_map(Result::ok).find(|e| {
        std::fs::read_to_string(e.path().join("name")).is_ok_and(|n| n.trim() == FB_NAME)
    });
    match found {
        Some(e) => Some(Path::new("/dev/graphics").join(e.file_name())),
        None => {
            warn!("ring: no '{FB_NAME}' framebuffer");
            None
        }
    }
}

fn load_patterns(dir: &Path) -> Patterns {
    let mut patterns = Patterns::default();
    for (role, file) in FILES {
        let path = dir.join(file);
        let parsed = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|text| Pattern::parse(&text));
        match parsed {
            Ok(p) => patterns.set(role, p),
            Err(e) => warn!("ring: {}: {e}", path.display()),
        }
    }
    patterns
}

fn run(mut ring: Ring, fb: &File, rx: &Receiver<Cmd>) {
    let epoch = Instant::now();
    let now = || u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut output = Output::default();
    let mut error_logged = false;
    loop {
        let step = output.step(&mut ring, now());
        if let Some(frame) = step.write {
            match fb.write_at(&frame, 0) {
                Ok(_) => error_logged = false,
                Err(e) if !error_logged => {
                    warn!("ring: write: {e}");
                    error_logged = true;
                }
                Err(_) => {}
            }
        }
        let wait = step.wake.map_or(IDLE_WAIT, |w| {
            Duration::from_millis(w.saturating_sub(now()))
        });
        let cmd = match rx.recv_timeout(wait) {
            Ok(cmd) => cmd,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let t = now();
        debug!("ring: {cmd:?} at {t}");
        match cmd {
            Cmd::Show(ui) => ring.show(ui, t),
            Cmd::Muted(m) => ring.set_muted(m, t),
            Cmd::VoiceIndication(on) => ring.set_voice_indication(on, t),
            Cmd::ForeignDraw => ring.foreign_draw(t),
        }
    }
    let _ = fb.write_at(&[0; 3 * LEDS], 0);
}
