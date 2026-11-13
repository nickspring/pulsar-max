//! pulsar-max: Home Assistant voice satellite for Yandex Station Max.
//! usage: pulsar-max [--version | --spot <capture.raw> | data_dir]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[allow(unsafe_code)]
mod aaudio;
mod capture;
mod controller;
mod helper;
mod input;
mod player;
mod replay;
mod ring;
mod screen;
mod sendspin;
mod settings_file;
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
use max_core::player_proto::PlayerEvent;
use pulsar_core::actors;
use pulsar_esphome::{ApiConfig, DeviceIdentity, WakeWordInfo};
use pulsar_lan::mdns::{self, MdnsService};
use pulsar_sendspin::SendspinConfig;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc::UnboundedReceiver;

use controller::{Controller, Flow, Tick};
use max_core::dialog::Sounds;
use max_core::screen::Viz;
use max_wake::command::CommandSpotter;
use max_wake::spotter::Spotter;
use settings_file::SettingsFile;
use wake::SpotEvent;

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
const SENDSPIN_PORT: u16 = 8928;
const SENDSPIN_SERVICE: &str = "_sendspin._tcp";
const ALERT_FILE: &str = "timer_alert.wav";
const SETTINGS_FILE: &str = "settings.conf";
const WAKE_WORD_ID: &str = "alisa";
pub(crate) const WAKE_PHRASE: &str = "Алиса";
const SPOTTER_CATALOG: &str = "/data/data/com.yandex.io.sdk/files/data/data/spotter.json";
const SPOTTER_KIND: &str = "activation/alisa";
const COMMAND_KIND: &str = "command";
const STOCK_SOUNDS: &str = "/data/data/com.yandex.io.sdk/files/system/sounds/common";
const TEARDOWN_SECS: u64 = 5;
const FIRST_VOLUME_WAIT: Duration = Duration::from_secs(3);

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
        let spotter = load_spotter().map_err(|e| warn!("wake word off: {e}")).ok();
        let commands = load_commands()
            .map_err(|e| warn!("voice commands off: {e}"))
            .ok();
        let spotted = replay::spot_file(Path::new(&file), spotter, commands);
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
    pulsar_core::add_own_target("max_core");
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
    let (reference_feed, music_tap, screen_inputs) = screen::feeds();
    let ss_cfg = SendspinConfig {
        name: id.friendly_name.clone(),
        product_name: id.model.clone(),
        manufacturer: id.manufacturer.clone(),
        software_version: id.project_version.clone(),
        mac: id.mac.to_ascii_lowercase(),
        state_dir: data_dir.join("sendspin"),
        output: sendspin::output(music_tap),
        port: SENDSPIN_PORT,
    };
    let ss_txt = vec![
        "path=/sendspin".to_owned(),
        format!("name={}", id.friendly_name),
    ];
    let spotter = load_spotter().map_err(|e| warn!("wake word off: {e}")).ok();
    let commands = load_commands()
        .map_err(|e| warn!("voice commands off: {e}"))
        .ok();
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
        music_styles: Viz::all().map(|v| v.label().to_owned()).collect(),
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
    // A server gets the volume at connect: start with STREAM_MUSIC, not a placeholder.
    let mut early_player = Vec::new();
    let volume = first_volume(&mut player_events, &mut early_player).await;
    if volume.is_none() {
        warn!("STREAM_MUSIC volume unknown, Sendspin starts at 0");
    }
    let (sendspin, mut ss_events, _no_sendspin) =
        match pulsar_sendspin::spawn(ss_cfg, volume.unwrap_or(0.0), false, 1.0) {
            Ok((h, rx)) => (Some(h), rx, None),
            Err(e) => {
                error!("Sendspin player unavailable: {e}");
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                (None, rx, Some(tx))
            }
        };
    if let (Some(m), Some(_)) = (&mdns, &sendspin) {
        m.add_service(MdnsService {
            service: SENDSPIN_SERVICE.into(),
            instance: name.clone(),
            port: SENDSPIN_PORT,
            txt: ss_txt,
        });
    }
    let mic_taken = yandex::take_mic();
    let (wake, wake_audio, mut spot_events, _no_spotter) = match wake::spawn(spotter, commands) {
        Some((h, audio_tx, events)) => (Some(h), Some(audio_tx), events, None),
        None => {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            (None, None, rx, Some(tx))
        }
    };
    let screen = screen::spawn(screen_inputs);
    let screen_feed = screen.as_ref().map(|_| reference_feed);
    let (capture, mut audio) = capture::spawn(dex, wake_audio, screen_feed);
    let (_input, mut keys) = input::spawn();
    let ring = ring::spawn();
    let settings = SettingsFile::load(data_dir.join(SETTINGS_FILE));
    let mut ctl = Controller::new(api, player, mdns, sounds, wake, ring, sendspin, settings);
    ctl.set_screen(screen);
    for ev in early_player {
        ctl.on_player(ev);
    }
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
            Some(ev) = spot_events.recv() => match ev {
                SpotEvent::Wake { end } => ctl.on_wake(end),
                SpotEvent::Command { phrase } => ctl.on_command(&phrase),
            },
            Some(ev) = ss_events.recv() => ctl.on_sendspin(ev),
            _ = term.recv() => break,
            _ = int.recv() => break,
            _ = hup.recv() => break,
            _ = quit.recv() => break,
        }
    }
    ctl.flush_settings();
    let restored = Arc::new(AtomicBool::new(!mic_taken));
    arm_teardown_watchdog(restored.clone());
    drop(capture);
    if mic_taken {
        yandex::give_back_mic();
        restored.store(true, Ordering::Relaxed);
    }
    0
}

async fn first_volume(
    events: &mut UnboundedReceiver<PlayerEvent>,
    early: &mut Vec<PlayerEvent>,
) -> Option<f32> {
    let wait = async {
        while let Some(ev) = events.recv().await {
            let volume = match ev {
                PlayerEvent::Volume { index, max } => {
                    Some(max_core::media::volume_from_index(index, max))
                }
                _ => None,
            };
            early.push(ev);
            if volume.is_some() {
                return volume;
            }
        }
        None
    };
    tokio::time::timeout(FIRST_VOLUME_WAIT, wait)
        .await
        .ok()
        .flatten()
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

fn load_commands() -> Result<CommandSpotter, String> {
    let catalog =
        std::fs::read_to_string(SPOTTER_CATALOG).map_err(|e| format!("{SPOTTER_CATALOG}: {e}"))?;
    let dir = PathBuf::from(max_wake::catalog::model_dir(&catalog, COMMAND_KIND)?);
    let read = |name: &str| {
        let path = dir.join(name);
        std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    let text = |name: &str| String::from_utf8(read(name)?).map_err(|e| format!("{name}: {e}"));
    let spotter = CommandSpotter::new(
        &text("flags.txt")?,
        &text("words.txt")?,
        &read("acoustic_model.nnet")?,
    )?;
    info!(
        "commands: model loaded, {} phrases",
        spotter.phrases().len()
    );
    Ok(spotter)
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
        volume: stock("volume_change.wav"),
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
