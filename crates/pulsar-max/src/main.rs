//! pulsar-max: Home Assistant voice satellite for Yandex Station Max.
//! usage: pulsar-max [--version | --spot <capture.raw> | data_dir]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

mod capture;
mod controller;
mod helper;
mod input;
mod player;
mod replay;
mod ring;
#[allow(unsafe_code)]
mod sys;
#[allow(unsafe_code)]
mod tflite;
mod wake;
mod yandex;

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use log::{error, info, warn};
use max_core::identity::{self, FRIENDLY_NAME, MANUFACTURER, MODEL_NAME, PLATFORM};
use pulsar_core::actors;
use pulsar_esphome::{ApiConfig, DeviceIdentity, WakeWordInfo};
use pulsar_lan::mdns::{self, MdnsService};
use tokio::signal::unix::{Signal, SignalKind, signal};

use controller::{Controller, Flow, MUSIC_STYLES, Tick};
use max_core::dialog::Sounds;
use max_wake::spotter::Spotter;

pub(crate) const VERSION: &str = match option_env!("PULSAR_MAX_VERSION") {
    Some(v) => v,
    None => "unknown",
};
const BUILD_TIME: &str = match option_env!("PULSAR_MAX_BUILD_TIME") {
    Some(v) => v,
    None => "unknown",
};
const DEFAULT_DATA_DIR: &str = "/data/pulsar-max";
const API_PORT: u16 = 6053;
pub(crate) const ESPHOME_SERVICE: &str = "_esphomelib._tcp";
const ALERT_FILE: &str = "timer_alert.wav";
const WAKE_WORD_ID: &str = "alisa";
pub(crate) const WAKE_PHRASE: &str = "Алиса";
const SPOTTER_CATALOG: &str = "/data/data/com.yandex.io.sdk/files/data/data/spotter.json";
const SPOTTER_KIND: &str = "activation/alisa";
const STOCK_SOUNDS: &str = "/data/data/com.yandex.io.sdk/files/system/sounds/common";
const TEARDOWN_SECS: u64 = 5;

fn main() -> ExitCode {
    let arg = std::env::args_os().nth(1);
    if arg.as_deref() == Some(OsStr::new("--version")) {
        let _ = writeln!(std::io::stdout(), "pulsar-max {VERSION}");
        return ExitCode::SUCCESS;
    }
    if arg.as_deref() == Some(OsStr::new("--spot")) {
        pulsar_core::init_logger();
        let Some(file) = std::env::args_os().nth(2) else {
            error!("usage: pulsar-max --spot <capture.raw>");
            return ExitCode::from(2);
        };
        let spotted = load_spotter().and_then(|s| replay::spot_file(Path::new(&file), s));
        return match spotted {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                error!("{e}");
                ExitCode::from(1)
            }
        };
    }
    let data_dir = arg.map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from);

    pulsar_core::init_logger();
    let platform = sys::system_property(c"ro.yndx.quasar.platform");
    if platform != PLATFORM {
        error!("not a Station Max: ro.yndx.quasar.platform={platform:?}");
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        error!("{}: {e}", data_dir.display());
        return ExitCode::from(1);
    }

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    let local = tokio::task::LocalSet::new();
    let status = local.block_on(&rt, run(&data_dir));
    local.block_on(&rt, actors::join_all(Duration::from_secs(3)));
    {
        let _rt = rt.enter();
        drop(local);
    }
    rt.shutdown_timeout(Duration::from_secs(1));
    ExitCode::from(status)
}

fn device_identity() -> DeviceIdentity {
    let mac = std::fs::read_to_string("/sys/class/net/wlan0/address").unwrap_or_else(|e| {
        warn!("wlan0 MAC: {e}");
        String::new()
    });
    let id = identity::identity(&mac);
    DeviceIdentity {
        name: id.name,
        friendly_name: FRIENDLY_NAME.into(),
        mac: id.mac,
        model: MODEL_NAME.into(),
        board: PLATFORM.into(),
        manufacturer: MANUFACTURER.into(),
        project_version: VERSION.into(),
        compilation_time: BUILD_TIME.into(),
    }
}

async fn run(data_dir: &Path) -> u8 {
    let id = device_identity();
    let name = id.name.clone();
    let spotter = load_spotter().map_err(|e| warn!("wake word off: {e}")).ok();
    let wake_ids: Vec<String> = spotter.iter().map(|_| WAKE_WORD_ID.to_owned()).collect();
    let cfg = ApiConfig {
        port: API_PORT,
        wake_words: wake_ids
            .iter()
            .map(|id| WakeWordInfo {
                id: id.clone(),
                phrase: WAKE_PHRASE.into(),
                languages: vec!["ru".into()],
            })
            .collect(),
        active_wake_words: wake_ids.clone(),
        music_styles: MUSIC_STYLES.iter().map(|s| (*s).to_owned()).collect(),
        has_clock: false,
        has_lux: false,
        has_hw_mic_mute: true,
        has_buttons: true,
        has_auto_brightness: false,
        key_path: Some(data_dir.join("api.key")),
        ..ApiConfig::default()
    };
    let Ok((api, mut api_events)) = pulsar_esphome::spawn(id, cfg) else {
        return 1;
    };
    let mdns = match mdns::spawn(name.clone()) {
        Ok(m) => {
            m.add_service(MdnsService {
                service: ESPHOME_SERVICE.into(),
                instance: name.clone(),
                port: API_PORT,
                txt: api.mdns_txt(),
            });
            Some(m)
        }
        Err(_) => {
            warn!("mDNS advertising unavailable");
            None
        }
    };
    let mut signals = Vec::new();
    for kind in [
        SignalKind::terminate(),
        SignalKind::interrupt(),
        SignalKind::hangup(),
        SignalKind::quit(),
    ] {
        match signal(kind) {
            Ok(s) => signals.push(s),
            Err(e) => {
                error!("signal handler: {e}");
                return 1;
            }
        }
    }
    let Ok([mut term, mut int, mut hup, mut quit]) = <[Signal; 4]>::try_from(signals) else {
        return 1;
    };

    let dex = helper::dex_path();
    let sounds = sounds(data_dir);
    let (player, mut player_events) = player::spawn(dex.clone());
    let mic_taken = yandex::take_mic();
    let (wake, wake_audio, mut wake_events, _no_wake) = match spotter {
        Some(s) => {
            let (h, audio_tx, events) = wake::spawn(s);
            (Some(h), Some(audio_tx), events, None)
        }
        None => {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (None, None, rx, Some(tx))
        }
    };
    let (capture, mut audio) = capture::spawn(dex, wake_audio);
    let (_input, mut keys) = input::spawn();
    let ring = ring::spawn();
    let mut ctl = Controller::new(api, player, mdns, sounds, wake, ring);
    ctl.publish_all();
    info!("pulsar-max {VERSION} running as {name}");

    loop {
        tokio::select! {
            biased;
            t = ctl.timers().next() => {
                // Keypad contract: a release already read goes before its press's long-press tick.
                if t == Tick::Keypad {
                    while let Ok(ev) = keys.try_recv() {
                        ctl.on_input(ev);
                    }
                }
                ctl.on_timer(t);
            }
            Some(ev) = keys.recv() => ctl.on_input(ev),
            Some(ev) = api_events.recv() => {
                if let Flow::Exit = ctl.on_api(ev) {
                    break;
                }
            }
            Some(ev) = player_events.recv() => ctl.on_player(ev),
            Some(chunk) = audio.recv() => ctl.on_audio(chunk),
            Some(ev) = wake_events.recv() => ctl.on_wake(ev),
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = hup.recv() => break,
            _ = quit.recv() => break,
        }
    }
    let restored = Arc::new(AtomicBool::new(!mic_taken));
    arm_teardown_watchdog(restored.clone());
    drop(capture);
    if mic_taken {
        yandex::give_back_mic();
        restored.store(true, Ordering::Relaxed);
    }
    0
}

/// io.sdk's own model, read in place: its files may not leave the station (docs/max.md).
fn load_spotter() -> Result<Spotter<tflite::Model>, String> {
    let catalog =
        std::fs::read_to_string(SPOTTER_CATALOG).map_err(|e| format!("{SPOTTER_CATALOG}: {e}"))?;
    let dir = PathBuf::from(max_wake::catalog::model_dir(&catalog, SPOTTER_KIND)?);
    let flags_path = dir.join("flags.txt");
    let flags = std::fs::read_to_string(&flags_path)
        .map_err(|e| format!("{}: {e}", flags_path.display()))?;
    let cfg = max_wake::config::parse_flags(&flags)?;
    let model = tflite::Model::load(&yandex::tflite_library(), &dir.join("model.tflite"))?;
    info!(
        "wake: '{}' model {} loaded, frequency filter {}",
        cfg.phrase,
        cfg.version,
        if cfg.freq_filter.is_some() {
            "on"
        } else {
            "off"
        }
    );
    Ok(Spotter::new(&cfg, model))
}

/// io.sdk's own sound files, read in place (docs/max.md).
fn sounds(data_dir: &Path) -> Sounds {
    let stock = |name: &str| {
        let path = Path::new(STOCK_SOUNDS).join(name);
        path.is_file().then(|| path.to_string_lossy().into_owned())
    };
    let timer = stock("timer.wav").or_else(|| {
        let alert = data_dir.join(ALERT_FILE);
        match std::fs::write(&alert, max_core::media::alert_wav()) {
            Ok(()) => Some(alert.to_string_lossy().into_owned()),
            Err(e) => {
                warn!("{}: {e}", alert.display());
                None
            }
        }
    });
    Sounds {
        start: stock("spotter_start.ogg"),
        cancel: stock("spotter_cancel.ogg"),
        error: stock("error.wav"),
        timer,
    }
}

/// pulsar's teardown watchdog; here it also returns the microphone to io.sdk, which a hung
/// teardown would otherwise leave deaf.
fn arm_teardown_watchdog(mic_restored: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new()
        .name("teardown-wd".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(TEARDOWN_SECS));
            error!("teardown did not finish in {TEARDOWN_SECS} s, exiting");
            if !mic_restored.load(Ordering::Relaxed) {
                yandex::give_back_mic();
            }
            sys::exit_now(0);
        });
    if let Err(e) = spawned {
        error!("teardown watchdog: {e}");
    }
}
