//! No pulsar crates here: `pulsar-lan` uses netlink and does not build outside Linux, while this
//! logic has to be tested on the host.
#![forbid(unsafe_code)]

pub mod audio;
pub mod backoff;
pub mod dialog;
pub mod identity;
pub mod inflight;
pub mod input;
pub mod media;
pub mod mic;
pub mod pcm;
pub mod player_proto;
