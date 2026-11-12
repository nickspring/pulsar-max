use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use log::{info, warn};
use max_core::audio::Decimator;
use max_core::pcm::S16Framer;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::helper::{self, Restarts};

const CHANNEL_MASK: &str = "ffff";
/// `vqe_0`, see docs/max.md, "Звук".
const VQE_CHANNEL: &str = "1";
const INPUT_RATE: usize = 48_000;
/// The HAL clicks up to 0 dBFS for about 0.5 s after every capture start.
const SKIP_AFTER_START: usize = INPUT_RATE / 2;
const READ_BYTES: usize = INPUT_RATE / 50 * 2;

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

pub fn spawn(dex: PathBuf) -> (CaptureHandle, UnboundedReceiver<Vec<i16>>) {
    let (tx, rx) = unbounded_channel();
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        child: Mutex::new(None),
    });
    let s = shared.clone();
    let thread = std::thread::Builder::new()
        .name("capture".into())
        .spawn(move || supervise(&s, &dex, &tx))
        .map_err(|e| warn!("capture: thread: {e}"))
        .ok();
    (CaptureHandle { shared, thread }, rx)
}

fn supervise(shared: &Shared, dex: &Path, tx: &UnboundedSender<Vec<i16>>) {
    let mut restarts = Restarts::new();
    let mut decimator = Decimator::new();
    while !shared.stop.load(Ordering::Relaxed) {
        restarts.attempt();
        let spawned = helper::command(dex, &["capture", CHANNEL_MASK, VQE_CHANNEL])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => {
                let Some(out) = shared.adopt(child) else {
                    break;
                };
                info!("capture: helper started");
                decimator.reset();
                pump(out, &mut decimator, tx);
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

fn pump(mut out: impl Read, decimator: &mut Decimator, tx: &UnboundedSender<Vec<i16>>) {
    let mut buf = vec![0u8; READ_BYTES];
    let mut framer = S16Framer::new(SKIP_AFTER_START);
    let mut samples = Vec::with_capacity(READ_BYTES / 2 + 1);
    loop {
        let n = match out.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        samples.clear();
        framer.push(&buf[..n], &mut samples);
        let mut out16 = Vec::with_capacity(samples.len() / 3 + 1);
        decimator.process(&samples, &mut out16);
        if !out16.is_empty() && tx.send(out16).is_err() {
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
