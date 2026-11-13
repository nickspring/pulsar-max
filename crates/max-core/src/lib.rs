//! No pulsar crates here: `pulsar-lan` uses netlink and does not build outside Linux, while this
//! logic has to be tested on the host.
#![forbid(unsafe_code)]

pub mod audio;
pub mod backoff;
pub mod commands;
pub mod dialog;
pub mod identity;
pub mod inflight;
pub mod input;
pub mod ledpattern;
pub mod media;
pub mod mic;
pub mod music_out;
pub mod package;
pub mod pcm;
pub mod player_proto;
pub mod preroll;
pub mod ring;
pub mod screen;
pub mod settings;
pub mod spectrogram;
pub mod spectrum_sync;
