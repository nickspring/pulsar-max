use std::fs::File;
use std::io::Read;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use log::{info, warn};
use max_core::input::{EVENT_SIZE, RawEvent, decode};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::sys;

const KEYPAD_NAME: &str = "gpio_keypad";
const KNOB_NAME: &str = "rotary-volume";
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_DROPPED: u16 = 3;
const SW_MUTE_DEVICE: u16 = 14;
const POLL_MS: i32 = 500;
const REOPEN: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    /// The device was (re)opened: nothing held before can still be released.
    Opened,
    /// `at_ms` is taken right after the read, so queueing does not stretch a press.
    Key {
        ev: RawEvent,
        at_ms: i64,
    },
    MicMutedState(bool),
    /// The volume knob; io.sdk keeps handling it and draws the volume on the ring.
    Knob(RawEvent),
}

pub struct InputHandle {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

pub fn spawn() -> (InputHandle, UnboundedReceiver<InputEvent>) {
    let (tx, rx) = unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (s, knob_stop, knob_tx) = (stop.clone(), stop.clone(), tx.clone());
    let threads = [
        std::thread::Builder::new()
            .name("input".into())
            .spawn(move || run(&s, &tx)),
        std::thread::Builder::new()
            .name("knob".into())
            .spawn(move || run_knob(&knob_stop, &knob_tx)),
    ]
    .into_iter()
    .filter_map(|t| t.map_err(|e| warn!("input: thread: {e}")).ok())
    .collect();
    (InputHandle { stop, threads }, rx)
}

fn run_knob(stop: &AtomicBool, tx: &UnboundedSender<InputEvent>) {
    let mut failure_logged = false;
    while !stop.load(Ordering::Relaxed) {
        match find_device(KNOB_NAME).map(File::open) {
            Some(Ok(mut file)) => {
                failure_logged = false;
                let mut buf = [0u8; EVENT_SIZE];
                while !stop.load(Ordering::Relaxed) {
                    match sys::wait_readable(file.as_fd(), POLL_MS) {
                        Ok(false) => continue,
                        Ok(true) => {}
                        Err(_) => break,
                    }
                    if file.read_exact(&mut buf).is_err() {
                        break;
                    }
                    let ev = decode(&buf);
                    if ev.kind == EV_KEY && tx.send(InputEvent::Knob(ev)).is_err() {
                        return;
                    }
                }
            }
            Some(Err(e)) if !failure_logged => {
                warn!("input: {KNOB_NAME}: {e}");
                failure_logged = true;
            }
            None if !failure_logged => {
                warn!("input: no {KNOB_NAME} device");
                failure_logged = true;
            }
            _ => {}
        }
        wait_reopen(stop);
    }
}

fn wait_reopen(stop: &AtomicBool) {
    let mut waited = Duration::ZERO;
    while waited < REOPEN && !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(100));
        waited += Duration::from_millis(100);
    }
}

fn run(stop: &AtomicBool, tx: &UnboundedSender<InputEvent>) {
    let mut failure_logged = false;
    while !stop.load(Ordering::Relaxed) {
        match find_device(KEYPAD_NAME).map(|path| (File::open(&path), path)) {
            Some((Ok(file), path)) => {
                failure_logged = false;
                // io.sdk answers the action button even without the mic (docs/max.md).
                match sys::evdev_grab(file.as_fd()) {
                    Ok(()) => info!("input: {} grabbed", path.display()),
                    Err(e) => warn!("input: grab {}: {e}", path.display()),
                }
                if tx.send(InputEvent::Opened).is_err() {
                    return;
                }
                send_mute_state(&file, tx);
                read_events(file, stop, tx);
            }
            Some((Err(e), path)) if !failure_logged => {
                warn!("input: {}: {e}", path.display());
                failure_logged = true;
            }
            None if !failure_logged => {
                warn!("input: no {KEYPAD_NAME} device");
                failure_logged = true;
            }
            _ => {}
        }
        wait_reopen(stop);
    }
}

fn read_events(mut file: File, stop: &AtomicBool, tx: &UnboundedSender<InputEvent>) {
    let mut buf = [0u8; EVENT_SIZE];
    while !stop.load(Ordering::Relaxed) {
        match sys::wait_readable(file.as_fd(), POLL_MS) {
            Ok(false) => continue,
            Ok(true) => {}
            Err(e) => {
                warn!("input: poll: {e}");
                return;
            }
        }
        if let Err(e) = file.read_exact(&mut buf) {
            warn!("input: read: {e}");
            return;
        }
        let at_ms = pulsar_core::now_ms();
        let ev = decode(&buf);
        if tx.send(InputEvent::Key { ev, at_ms }).is_err() {
            return;
        }
        // The dropped events may have held the switch change; EVIOCGSW reads the current state.
        if ev.kind == EV_SYN && ev.code == SYN_DROPPED {
            send_mute_state(&file, tx);
        }
    }
}

fn send_mute_state(file: &File, tx: &UnboundedSender<InputEvent>) {
    match sys::evdev_switch_on(file.as_fd(), SW_MUTE_DEVICE) {
        Ok(muted) => {
            let _ = tx.send(InputEvent::MicMutedState(muted));
        }
        Err(e) => warn!("input: switch state: {e}"),
    }
}

fn find_device(name: &str) -> Option<PathBuf> {
    let dir = std::fs::read_dir("/sys/class/input").ok()?;
    dir.filter_map(Result::ok)
        .map(|e| e.file_name())
        .filter_map(|n| n.to_str().map(str::to_owned))
        .filter(|n| n.starts_with("event"))
        .find(|n| device_name(Path::new("/sys/class/input").join(n)).as_deref() == Some(name))
        .map(|n| Path::new("/dev/input").join(n))
}

fn device_name(sys_dir: PathBuf) -> Option<String> {
    std::fs::read_to_string(sys_dir.join("device/name"))
        .ok()
        .map(|s| s.trim().to_owned())
}

impl Drop for InputHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}
