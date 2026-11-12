use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use log::{info, warn};
use max_core::dialog::{Channel, PlayId};
use max_core::inflight::InFlight;
use max_core::player_proto::{
    self, PlayState, PlayerEvent, gain_line, play_line, simple_line, volume_line,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::helper::{self, Restarts};

#[derive(Clone)]
pub struct PlayerHandle {
    inner: Rc<Inner>,
}

struct Inner {
    cmds: RefCell<Option<mpsc::Sender<String>>>,
    next_id: Cell<u64>,
    shared: Arc<Shared>,
    threads: RefCell<Vec<JoinHandle<()>>>,
}

struct Running {
    child: Child,
    stdin: Option<ChildStdin>,
}

struct Shared {
    stop: AtomicBool,
    running: Mutex<Option<Running>>,
    in_flight: Mutex<InFlight>,
    gains: Mutex<[f32; 2]>,
    events: UnboundedSender<PlayerEvent>,
}

impl Shared {
    fn emit(&self, ev: Option<PlayerEvent>) {
        if let Some(ev) = ev {
            let _ = self.events.send(ev);
        }
    }

    fn finish(&self, id: PlayId, ok: bool) {
        let ev = self
            .in_flight
            .lock()
            .ok()
            .and_then(|mut f| f.finish(id, ok));
        self.emit(ev);
    }

    fn fail_unsent(&self, line: &str) {
        let ev = self
            .in_flight
            .lock()
            .ok()
            .and_then(|mut f| f.fail_unsent(line));
        self.emit(ev);
    }

    fn fail_all(&self) {
        let evs = self
            .in_flight
            .lock()
            .map(|mut f| f.fail_all())
            .unwrap_or_default();
        for ev in evs {
            let _ = self.events.send(ev);
        }
    }

    fn write_line(&self, line: &str) -> bool {
        let Ok(mut guard) = self.running.lock() else {
            return false;
        };
        let Some(stdin) = guard.as_mut().and_then(|r| r.stdin.as_mut()) else {
            return false;
        };
        let ok = writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .is_ok();
        if !ok && let Some(r) = guard.as_mut() {
            r.stdin = None;
        }
        ok
    }

    /// Publishes the child unless shutdown already began; Drop raises `stop` before it takes
    /// the same lock, so either it sees this child or the check here sees `stop`.
    fn adopt(&self, mut child: Child) -> Option<ChildStdout> {
        let stdout = child.stdout.take();
        let stdin = child.stdin.take();
        let Ok(mut guard) = self.running.lock() else {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        if self.stop.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        *guard = Some(Running { child, stdin });
        stdout
    }
}

fn gain_index(ch: Channel) -> usize {
    match ch {
        Channel::Music => 0,
        Channel::Announce => 1,
    }
}

pub fn spawn(dex: PathBuf) -> (PlayerHandle, UnboundedReceiver<PlayerEvent>) {
    let (events, rx) = unbounded_channel();
    let (cmds, cmd_rx) = mpsc::channel::<String>();
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        running: Mutex::new(None),
        in_flight: Mutex::new(InFlight::default()),
        gains: Mutex::new([1.0, 1.0]),
        events,
    });
    let mut threads = Vec::new();
    let s = shared.clone();
    match std::thread::Builder::new()
        .name("player-writer".into())
        .spawn(move || writer(&s, &cmd_rx))
    {
        Ok(t) => threads.push(t),
        Err(e) => warn!("player: writer thread: {e}"),
    }
    let s = shared.clone();
    match std::thread::Builder::new()
        .name("player".into())
        .spawn(move || supervise(&s, &dex))
    {
        Ok(t) => threads.push(t),
        Err(e) => warn!("player: supervisor thread: {e}"),
    }
    let handle = PlayerHandle {
        inner: Rc::new(Inner {
            cmds: RefCell::new(Some(cmds)),
            next_id: Cell::new(1),
            shared,
            threads: RefCell::new(threads),
        }),
    };
    (handle, rx)
}

fn writer(shared: &Shared, cmds: &mpsc::Receiver<String>) {
    while let Ok(line) = cmds.recv() {
        if !shared.write_line(&line) {
            shared.fail_unsent(&line);
        }
    }
}

fn supervise(shared: &Shared, dex: &Path) {
    let mut restarts = Restarts::new();
    while !shared.stop.load(Ordering::Relaxed) {
        restarts.attempt();
        let spawned = helper::command(dex, &["player"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
        match spawned {
            Ok(child) => {
                let Some(stdout) = shared.adopt(child) else {
                    break;
                };
                let gains = shared.gains.lock().map(|g| *g).unwrap_or([1.0, 1.0]);
                shared.write_line(&gain_line(Channel::Music, gains[0]));
                shared.write_line(&gain_line(Channel::Announce, gains[1]));
                read_events(shared, stdout);
                let status = shared
                    .running
                    .lock()
                    .ok()
                    .and_then(|mut g| g.take())
                    .map(|mut r| r.child.wait());
                if !shared.stop.load(Ordering::Relaxed) {
                    warn!("player: helper exited: {status:?}");
                }
            }
            Err(e) => warn!("player: cannot start helper: {e}"),
        }
        shared.fail_all();
        for ch in [Channel::Music, Channel::Announce] {
            let _ = shared.events.send(PlayerEvent::State(ch, PlayState::Idle));
        }
        if !restarts.wait_unless_stopped(&shared.stop) {
            break;
        }
    }
    shared.fail_all();
}

fn read_events(shared: &Shared, out: ChildStdout) {
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        match player_proto::parse_event(&line) {
            Some(PlayerEvent::Done { id, ok }) => shared.finish(id, ok),
            Some(PlayerEvent::Ready) => info!("player: helper ready"),
            Some(ev) => {
                let _ = shared.events.send(ev);
            }
            None => warn!("player: unexpected line from helper: {line:?}"),
        }
    }
}

impl PlayerHandle {
    fn send(&self, line: String) {
        let unsent = match self.inner.cmds.borrow().as_ref() {
            Some(tx) => tx.send(line).err().map(|e| e.0),
            None => Some(line),
        };
        if let Some(line) = unsent {
            self.inner.shared.fail_unsent(&line);
        }
    }

    pub fn play(&self, ch: Channel, urls: Vec<String>) -> PlayId {
        let id = PlayId(self.inner.next_id.get());
        self.inner.next_id.set(id.0 + 1);
        if let Ok(mut f) = self.inner.shared.in_flight.lock() {
            f.start(id);
        }
        match play_line(id, ch, &urls) {
            Some(line) => self.send(line),
            None => {
                warn!("player: unplayable url list ({} urls)", urls.len());
                self.inner.shared.finish(id, false);
            }
        }
        id
    }

    pub fn pause(&self, ch: Channel) {
        self.send(simple_line("pause", ch));
    }

    pub fn resume(&self, ch: Channel) {
        self.send(simple_line("resume", ch));
    }

    pub fn stop(&self, ch: Channel) {
        self.send(simple_line("stop", ch));
    }

    pub fn set_gain(&self, ch: Channel, gain: f32) {
        if let Ok(mut g) = self.inner.shared.gains.lock() {
            g[gain_index(ch)] = gain;
        }
        self.send(gain_line(ch, gain));
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(volume_line(volume));
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.cmds.borrow_mut().take();
        if let Ok(mut g) = self.shared.running.lock()
            && let Some(r) = g.as_mut()
        {
            r.stdin = None;
            let _ = r.child.kill();
        }
        for t in self.threads.borrow_mut().drain(..) {
            let _ = t.join();
        }
    }
}
