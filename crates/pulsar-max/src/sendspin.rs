use std::sync::Arc;
use std::time::Duration;

use log::info;
use max_core::music_out::{self, MusicOut};
use pulsar_sendspin::{AudioOutput, Written};

use crate::aaudio;

pub fn output() -> Arc<dyn AudioOutput> {
    Arc::new(Output(MusicOut::new(AAudio)))
}

struct AAudio;

impl music_out::Device for AAudio {
    type Stream = aaudio::Stream;

    fn open(&self, rate: u32, channels: u8) -> Result<aaudio::Stream, String> {
        let mut s = aaudio::Stream::open_output(rate, channels)?;
        let (burst, buffer) = s.burst_and_buffer();
        info!("sendspin: AAudio burst {burst}, buffer {buffer} frames");
        Ok(s)
    }

    fn now_ns(&self) -> i64 {
        aaudio::monotonic_ns()
    }

    fn idle(&self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}

impl music_out::Stream for aaudio::Stream {
    fn write(&mut self, data: &[u8], frames: usize, timeout_ms: u32) -> Result<usize, String> {
        aaudio::Stream::write(self, data, frames, timeout_ms)
    }

    fn timestamp(&mut self) -> Option<(i64, i64)> {
        aaudio::Stream::timestamp(self)
    }

    fn frames_written(&mut self) -> i64 {
        aaudio::Stream::frames_written(self)
    }

    fn frames_read(&mut self) -> i64 {
        aaudio::Stream::frames_read(self)
    }

    fn xruns(&mut self) -> i32 {
        aaudio::Stream::xruns(self)
    }
}

struct Output(MusicOut<AAudio>);

impl AudioOutput for Output {
    fn configure(&self, rate: u32, channels: u8, bits: u8) {
        self.0.configure(rate, channels, bits);
    }

    fn release(&self) {
        self.0.release();
    }

    fn set_gain(&self, g: f32) {
        self.0.set_gain(g);
    }

    fn write(&self, data: &[u8], timeout_ms: u32) -> Written {
        let w = self.0.write(data, timeout_ms);
        Written {
            bytes: w.bytes,
            played: w.played,
            finish_in_us: w.finish_in_us,
        }
    }
}
