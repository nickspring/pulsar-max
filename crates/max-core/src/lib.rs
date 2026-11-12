//! No pulsar crates here: `pulsar-lan` uses netlink and does not build outside Linux, while this
//! logic has to be tested on the host.
#![forbid(unsafe_code)]

pub mod identity;
