/// `ro.yndx.quasar.platform`; the Zigbee revision reports the same value.
pub const PLATFORM: &str = "yandexstation_2";
pub const MODEL_NAME: &str = "Yandex Station Max";
pub const FRIENDLY_NAME: &str = "Яндекс Станция Макс";
pub const MANUFACTURER: &str = "Yandex";
const HOST_PREFIX: &str = "yandex-max-";

#[derive(Debug, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub mac: String,
}

/// Same naming as pulsar: the prefix and the last 6 hex digits of the `wlan0` MAC.
pub fn identity(mac_line: &str) -> Identity {
    let mac = mac_line.trim().to_ascii_uppercase();
    let plain: String = mac
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let suffix = &plain[plain.len().saturating_sub(6)..];
    Identity {
        name: format!("{HOST_PREFIX}{suffix}"),
        mac,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_from_sysfs_line() {
        let id = identity("aa:bb:cc:dd:ee:0f\n");
        assert_eq!(id.name, "yandex-max-ddee0f");
        assert_eq!(id.mac, "AA:BB:CC:DD:EE:0F");
    }

    #[test]
    fn short_or_empty_mac_keeps_what_there_is() {
        assert_eq!(identity("ab:c").name, "yandex-max-abc");
        assert_eq!(identity("").name, "yandex-max-");
        assert_eq!(identity("").mac, "");
    }

    #[test]
    fn name_fits_esphome_limit() {
        assert!(identity("aa:bb:cc:dd:ee:ff").name.len() <= 31);
    }
}
