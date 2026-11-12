use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use max_core::backoff::Backoff;

pub fn dex_path() -> PathBuf {
    std::env::current_exe()
        .map(|exe| exe.with_file_name("helper.dex"))
        .unwrap_or_else(|_| PathBuf::from("helper.dex"))
}

pub fn command(dex: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("/system/bin/app_process");
    c.env("CLASSPATH", dex)
        .arg("/system/bin")
        .arg("MaxHelper")
        .args(args);
    c
}

pub struct Restarts {
    backoff: Backoff,
    started: Instant,
}

impl Restarts {
    pub fn new() -> Self {
        Self {
            backoff: Backoff::default(),
            started: Instant::now(),
        }
    }

    /// Called before every spawn, so a spawn that fails counts as a run of zero length.
    pub fn attempt(&mut self) {
        self.started = Instant::now();
    }

    pub fn wait_unless_stopped(&mut self, stop: &AtomicBool) -> bool {
        let deadline = Instant::now() + self.backoff.next_delay(self.started.elapsed());
        while Instant::now() < deadline {
            if stop.load(Ordering::Relaxed) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        !stop.load(Ordering::Relaxed)
    }
}
