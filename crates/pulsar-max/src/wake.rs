use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{error, info};
use max_wake::spotter::{AcousticModel, Detection, Spotter};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::capture::SpotterAudio;

/// ~0.6 s: rides out a slow inference without firing on stale audio.
const QUEUE_CHUNKS: usize = 32;
const STOP_POLL: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub struct WakeEvent {
    /// End of the hit frame's window, the stock `spotter_get_unhandled_nbytes` anchor: still
    /// inside the tail of the word.
    pub end: u64,
}

pub struct WakeHandle {
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub fn spawn<M>(
    spotter: Spotter<M>,
) -> (
    WakeHandle,
    SyncSender<SpotterAudio>,
    UnboundedReceiver<WakeEvent>,
)
where
    M: AcousticModel + Send + 'static,
    M::Error: Display,
{
    let (audio_tx, audio_rx) = mpsc::sync_channel(QUEUE_CHUNKS);
    let (events, rx) = unbounded_channel();
    let enabled = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(AtomicBool::new(false));
    let (e, s) = (enabled.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("wake".into())
        .spawn(move || run(spotter, &e, &s, &audio_rx, &events))
        .map_err(|e| error!("wake: thread: {e}"))
        .ok();
    let handle = WakeHandle {
        enabled,
        stop,
        thread,
    };
    (handle, audio_tx, rx)
}

fn run<M>(
    mut spotter: Spotter<M>,
    enabled: &AtomicBool,
    stop: &AtomicBool,
    audio: &Receiver<SpotterAudio>,
    events: &UnboundedSender<WakeEvent>,
) where
    M: AcousticModel,
    M::Error: Display,
{
    let mut detections = Vec::new();
    let mut failures = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let chunk = match audio.recv_timeout(STOP_POLL) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // A skipped chunk is a gap in positions: the spotter starts over on the next one.
        if !enabled.load(Ordering::Relaxed) {
            continue;
        }
        detections.clear();
        let pushed = spotter.push(
            chunk.pos,
            &chunk.omni,
            &chunk.bnr,
            &chunk.raw,
            &mut detections,
        );
        if let Err(e) = pushed {
            failures += 1;
            if failures.is_power_of_two() {
                error!("wake: model: {e} ({failures} failures)");
            }
        }
        for d in &detections {
            match *d {
                Detection::Wake { end, confidence } => {
                    info!("wake: confidence {confidence:.3}");
                    let _ = events.send(WakeEvent { end });
                }
                Detection::Blocked {
                    confidence, filter, ..
                } => info!(
                    "wake: confidence {confidence:.3} blocked by the frequency filter ({filter:.2})"
                ),
            }
        }
    }
}

impl WakeHandle {
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }
}

impl Drop for WakeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
