//! The stock Yandex "Алиса" spotter reimplemented from io.sdk 3.367.25 (docs/max.md, "Ключевое
//! слово"); only the TFLite model itself comes from the station.
#![forbid(unsafe_code)]
pub mod catalog;
pub mod chen14;
pub mod config;
pub mod fbank;
pub mod freqfilter;
pub mod model_meta;
pub mod spectrum;
pub mod spotter;
