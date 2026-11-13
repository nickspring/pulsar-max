use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use std::sync::mpsc::SyncSender;

use log::{info, warn};
use max_core::audio::Decimator;
use max_core::pcm::{Deinterleaver, S16Framer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::helper::{self, Restarts};
use crate::screen::ReferenceFeed;

const CHANNEL_MASK: &str = "ffff";
/// `vqe_0`, `vqe_bnr` and the first raw mic (docs/max.md, "Звук"): the spotter's omni, bnr and
/// frequency filter inputs; HA hears `vqe_0`.
pub const CHANNELS: [usize; 3] = [1, 2, 3];
const CHANNEL_COUNT: usize = CHANNELS.len();
/// The digital speaker reference L, R: everything that plays, for the screen spectrogram.
const REFERENCE: [usize; 2] = [13, 14];
const PIPE_CHANNELS: usize = CHANNEL_COUNT + REFERENCE.len();
const INPUT_RATE: usize = 48_000;
/// The HAL clicks up to 0 dBFS for about 0.5 s after every capture start.
pub const SKIP_AFTER_START_FRAMES: usize = INPUT_RATE / 2;
const SKIP_AFTER_START: usize = SKIP_AFTER_START_FRAMES * PIPE_CHANNELS;
const READ_BYTES: usize = INPUT_RATE / 50 * 2 * PIPE_CHANNELS;
/// A restart jumps the stream position by a second, so consumers see the lost audio as a gap.
const RESTART_GAP: u64 = 16_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    /// Position of the first sample in the 16 kHz stream.
    pub pos: u64,
    pub samples: Vec<i16>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpotterAudio {
    pub pos: u64,
    pub omni: Vec<i16>,
    pub bnr: Vec<i16>,
    pub raw: Vec<i16>,
}

pub struct CaptureHandle {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    stop: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Shared {
    /// Same lock-ordering contract as the player's `adopt`.
    fn adopt(&self, mut child: Child) -> Option<ChildStdout> {
        let stdout = child.stdout.take();
        let Ok(mut guard) = self.child.lock() else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        if self.stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        *guard = Some(child);
        stdout
    }
}

pub fn spawn(
    dex: PathBuf,
    wake: Option<SyncSender<SpotterAudio>>,
    screen: Option<ReferenceFeed>,
) -> (CaptureHandle, UnboundedReceiver<AudioChunk>) {
    let (tx, rx) = unbounded_channel();
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        child: Mutex::new(None),
    });
    let s = shared.clone();
    let thread = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || supervise(&s, &dex, &tx, wake.as_ref(), screen.as_ref()))
        .map_err(|e| warn!("capture: thread: {e}"))
        .ok();
    (CaptureHandle { shared, thread }, rx)
}

fn supervise(
    shared: &Shared,
    dex: &Path,
    tx: &UnboundedSender<AudioChunk>,
    wake: Option<&SyncSender<SpotterAudio>>,
    screen: Option<&ReferenceFeed>,
) {
    let mut restarts = Restarts::new();
    let mut pos = 0u64;
    while !shared.stop.load(Ordering::Relaxed) {
        restarts.attempt();
        let channels = CHANNELS
            .iter()
            .chain(&REFERENCE)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let spawned = helper::command(dex, &["capture", CHANNEL_MASK, &channels])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => {
                let Some(out) = shared.adopt(child) else {
                    break;
                };
                info!("capture: helper started");
                pump(out, &mut pos, tx, wake, screen);
                pos += RESTART_GAP;
                let status = shared
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
                    .map(|mut c| c.wait());
                if !shared.stop.load(Ordering::Relaxed) {
                    warn!("capture: helper exited: {status:?}");
                }
            }
            Err(e) => warn!("capture: cannot start helper: {e}"),
        }
        if !restarts.wait_unless_stopped(&shared.stop) {
            break;
        }
    }
}

fn pump(
    mut out: impl Read,
    pos: &mut u64,
    tx: &UnboundedSender<AudioChunk>,
    wake: Option<&SyncSender<SpotterAudio>>,
    screen: Option<&ReferenceFeed>,
) {
    let mut buf = vec![0u8; READ_BYTES];
    let mut framer = S16Framer::new(SKIP_AFTER_START);
    let mut split = Deinterleaver::new(PIPE_CHANNELS);
    let mut decimators: [Decimator; CHANNEL_COUNT] = std::array::from_fn(|_| Decimator::new());
    let mut samples = Vec::with_capacity(READ_BYTES / 2 + 1);
    let mut channels: [Vec<i16>; PIPE_CHANNELS] = Default::default();
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        samples.clear();
        framer.push(&buf[..n], &mut samples);
        channels.iter_mut().for_each(Vec::clear);
        split.push(&samples, &mut channels);
        if let Some(feed) = screen
            && feed.wanted()
        {
            let [l, r] = [&channels[CHANNEL_COUNT], &channels[CHANNEL_COUNT + 1]];
            feed.send(l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect());
        }
        let [omni, bnr, raw] = std::array::from_fn(|c| {
            let mut out16 = Vec::with_capacity(channels[c].len() / 3 + 1);
            decimators[c].process(&channels[c], &mut out16);
            out16
        });
        if omni.is_empty() {
            continue;
        }
        let at = *pos;
        *pos += omni.len() as u64;
        if let Some(w) = wake {
            // Never block capture: a wake thread that falls behind loses frames, not the stream.
            let _ = w.try_send(SpotterAudio {
                pos: at,
                omni: omni.clone(),
                bnr,
                raw,
            });
        }
        if tx
            .send(AudioChunk {
                pos: at,
                samples: omni,
            })
            .is_err()
        {
            return;
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.shared.child.lock()
            && let Some(child) = g.as_mut()
        {
            let _ = child.kill();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
