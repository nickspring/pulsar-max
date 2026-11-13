use std::process::Command;

use log::{info, warn};
use max_core::package;

use crate::sys;

const PACKAGE: &str = "com.yandex.io.sdk";
const SYSTEM_APK: &str = "/system/priv-app/ServicesIosdk/ServicesIosdk.apk";
const TFLITE_IN_APK: &str = "!/lib/arm64-v8a/libtflite.so";

/// io.sdk can be neither disabled nor stopped (docs/max.md); a restart applies the denial.
pub fn take_mic() -> bool {
    if !set_record_audio("ignore") {
        return false;
    }
    restart_io_sdk();
    info!("yandex: microphone taken from {PACKAGE}");
    true
}

pub fn give_back_mic() {
    if set_record_audio("allow") {
        restart_io_sdk();
        info!("yandex: microphone given back to {PACKAGE}");
    }
}

/// bionic loads a library straight from an APK when it is stored uncompressed and page-aligned,
/// as io.sdk's is; an update moves the APK, so ask the package manager first.
pub fn tflite_library() -> String {
    let apk = Command::new("/system/bin/cmd")
        .args(["package", "path", PACKAGE])
        .output()
        .ok()
        .and_then(|o| package::apk_path(&String::from_utf8_lossy(&o.stdout)).map(str::to_owned))
        .unwrap_or_else(|| SYSTEM_APK.to_owned());
    format!("{apk}{TFLITE_IN_APK}")
}

fn set_record_audio(mode: &str) -> bool {
    match Command::new("/system/bin/cmd")
        .args(["appops", "set", PACKAGE, "RECORD_AUDIO", mode])
        .status()
    {
        Ok(s) if s.success() => true,
        Ok(s) => {
            warn!("yandex: appops RECORD_AUDIO {mode}: {s}");
            false
        }
        Err(e) => {
            warn!("yandex: appops RECORD_AUDIO {mode}: {e}");
            false
        }
    }
}

fn restart_io_sdk() {
    for pid in pids_of(PACKAGE) {
        if let Err(e) = sys::kill(pid) {
            warn!("yandex: kill {pid}: {e}");
        }
    }
}

fn pids_of(process: &str) -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| c.split(|&b| b == 0).next() == Some(process.as_bytes()))
        })
        .collect()
}
