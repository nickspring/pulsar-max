//! The stock Yandex "Алиса" and command spotters reimplemented from io.sdk 3.367.25 (docs/max.md,
//! "Ключевое слово"); only the models themselves come from the station.
#![forbid(unsafe_code)]
pub mod catalog;
pub mod chen14;
pub mod chen14_multi;
pub mod command;
pub mod command_config;
pub mod config;
pub mod fbank;
#[cfg(test)]
mod fixtures;
pub mod freqfilter;
pub mod model_meta;
pub mod nnet;
pub mod spectrum;
pub mod spotter;
