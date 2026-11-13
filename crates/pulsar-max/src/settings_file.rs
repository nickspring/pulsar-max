use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use log::warn;
use max_core::settings::{self, Settings, Store};

const _: () = {
    assert!(settings::MIC_GAIN_MAX_DB == pulsar_esphome::MIC_GAIN_MAX_DB);
    assert!(max_core::dialog::FOLLOWUP_DURATION_MAX_S == pulsar_esphome::FOLLOWUP_DURATION_MAX_S);
    assert!(settings::COMMAND_OFFSET_MIN_MS == pulsar_esphome::COMMAND_OFFSET_MIN_MS);
    assert!(settings::COMMAND_OFFSET_MAX_MS == pulsar_esphome::COMMAND_OFFSET_MAX_MS);
};

pub struct SettingsFile {
    path: PathBuf,
    store: Store,
}

impl SettingsFile {
    pub fn load(path: PathBuf) -> Self {
        let (s, err) = Settings::from_read(std::fs::read_to_string(&path));
        if let Some(e) = err {
            warn!("{}: {e}, default settings", path.display());
        }
        Self {
            path,
            store: Store::new(s),
        }
    }

    pub fn stored(&self) -> Settings {
        self.store.saved()
    }

    pub fn flush(&mut self, s: &Settings) {
        if let Some(e) = self.store.flush(s, |text| write_atomic(&self.path, text)) {
            warn!("{}: {e}, settings are not kept", self.path.display());
        }
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let written = File::create(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    if let Some(dir) = path.parent() {
        let _ = File::open(dir).and_then(|d| d.sync_all());
    }
    Ok(())
}
