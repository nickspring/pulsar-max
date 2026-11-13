use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{error, info};
use max_wake::command::CommandSpotter;
use max_wake::spotter::{AcousticModel, Detection, Spotter};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::capture::SpotterAudio;

/// ~0.6 s: rides out a slow inference without firing on stale audio.
const QUEUE_CHUNKS: usize = 32;
const STOP_POLL: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub enum SpotEvent {
    Wake {
        /// End of the hit frame's window, the stock `spotter_get_unhandled_nbytes` anchor: still
        /// inside the tail of the word.
        end: u64,
    },
    Command {
        phrase: String,
    },
}

pub struct WakeHandle {
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub type Spawned = (
    WakeHandle,
    SyncSender<SpotterAudio>,
    UnboundedReceiver<SpotEvent>,
);

pub fn spawn<M>(spotter: Option<Spotter<M>>, commands: Option<CommandSpotter>) -> Option<Spawned>
where
    M: AcousticModel + Send + 'static,
    M::Error: Display,
{
    if spotter.is_none() && commands.is_none() {
        return None;
    }
    let (audio_tx, audio_rx) = mpsc::sync_channel(QUEUE_CHUNKS);
    let (events, rx) = unbounded_channel();
    let enabled = Arc::new(AtomicBool::new(true));
    let stop = Arc::new(AtomicBool::new(false));
    let (e, s) = (enabled.clone(), stop.clone());
    let thread = std::thread::Builder::new()
        .name("wake".into())
        .spawn(move || run(spotter, commands, &e, &s, &audio_rx, &events))
        .map_err(|e| error!("wake: thread: {e}"))
        .ok();
    let handle = WakeHandle {
        enabled,
        stop,
        thread,
    };
    Some((handle, audio_tx, rx))
}

fn run<M>(
    mut spotter: Option<Spotter<M>>,
    mut commands: Option<CommandSpotter>,
    enabled: &AtomicBool,
    stop: &AtomicBool,
    audio: &Receiver<SpotterAudio>,
    events: &UnboundedSender<SpotEvent>,
) where
    M: AcousticModel,
    M::Error: Display,
{
    let mut detections = Vec::new();
    let mut failures = 0u64;
    let mut next_pos = None;
    while !stop.load(Ordering::Relaxed) {
        let chunk = match audio.recv_timeout(STOP_POLL) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        // A skipped chunk is a gap in positions: the spotters start over on the next one.
        if !enabled.load(Ordering::Relaxed) {
            continue;
        }
        // Wake hits go out first: a command word after "Алиса" is judged against the dialog it opens.
        if let Some(spotter) = spotter.as_mut() {
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
                        let _ = events.send(SpotEvent::Wake { end });
                    }
                    Detection::Blocked {
                        confidence, filter, ..
                    } => info!(
                        "wake: confidence {confidence:.3} blocked by the frequency filter ({filter:.2})"
                    ),
                }
            }
        }
        if let Some(commands) = commands.as_mut() {
            if next_pos.is_some_and(|p| p != chunk.pos) {
                commands.reset();
            }
            next_pos = Some(chunk.pos + chunk.omni.len() as u64);
            for hit in commands.push(&chunk.omni) {
                info!("command: '{}' confidence {:.3}", hit.phrase, hit.confidence);
                let _ = events.send(SpotEvent::Command { phrase: hit.phrase });
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
