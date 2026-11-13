//! io.sdk computes spectra before playback and draws each when its audio plays.

use std::collections::VecDeque;

use crate::spectrogram::{Analyzer, Spectrum};

pub const TICK_NS: i64 = 40_000_000;
const LATE_NS: i64 = 80_000_000;
/// io.sdk's "spectrum buffer overflow" limit.
const CAPACITY: usize = 126;
const SECOND_NS: i64 = 1_000_000_000;

#[derive(Debug)]
pub struct TimedQueue<T> {
    items: VecDeque<(i64, T)>,
    next_tick_ns: Option<i64>,
}

impl<T> Default for TimedQueue<T> {
    fn default() -> Self {
        TimedQueue {
            items: VecDeque::new(),
            next_tick_ns: None,
        }
    }
}

impl<T> TimedQueue<T> {
    pub fn push(&mut self, at_ns: i64, item: T) -> bool {
        if self.items.len() >= CAPACITY {
            return false;
        }
        self.items.push_back((at_ns, item));
        true
    }

    pub fn start(&mut self, now_ns: i64) {
        self.next_tick_ns = Some(now_ns);
    }

    pub fn stop(&mut self) {
        self.items.clear();
        self.next_tick_ns = None;
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn next_tick_ns(&self) -> Option<i64> {
        self.next_tick_ns
    }

    /// io.sdk resyncs its tick to the drawn item, so steady playback is drawn on time.
    pub fn tick(&mut self, now_ns: i64) -> Option<T> {
        while self
            .items
            .front()
            .is_some_and(|&(at, _)| at < now_ns - LATE_NS)
        {
            self.items.pop_front();
        }
        let due = self
            .items
            .front()
            .is_some_and(|&(at, _)| at <= now_ns + TICK_NS);
        let item = if due { self.items.pop_front() } else { None };
        self.next_tick_ns = Some(item.as_ref().map_or(now_ns, |&(at, _)| at) + TICK_NS);
        item.map(|(_, t)| t)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MusicChunk {
    pub stream: u32,
    pub rate: u32,
    /// CLOCK_MONOTONIC.
    pub play_at_ns: i64,
    pub stereo: Vec<i16>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TapFormat {
    pub stream: u32,
    pub rate: u32,
    pub channels: u8,
    pub bits: u8,
}

impl TapFormat {
    pub fn next(self, rate: u32, channels: u8, bits: u8) -> TapFormat {
        TapFormat {
            stream: self.stream.wrapping_add(1),
            rate,
            channels,
            bits,
        }
    }

    /// `finish_in_us` 0: a write that played nothing, so it cannot be timed.
    pub fn chunk(&self, taken: &[u8], finish_in_us: i64, now_ns: i64) -> Option<MusicChunk> {
        if self.bits != 16 || !matches!(self.channels, 1 | 2) || self.rate == 0 || finish_in_us <= 0
        {
            return None;
        }
        let bpf = 2 * usize::from(self.channels);
        let frames = taken.len() / bpf;
        if frames == 0 {
            return None;
        }
        let samples = taken[..frames * bpf]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b));
        let stereo = if self.channels == 2 {
            samples.collect()
        } else {
            samples.flat_map(|s| [s, s]).collect()
        };
        let duration_ns = i64::try_from(frames).ok()? * SECOND_NS / i64::from(self.rate);
        Some(MusicChunk {
            stream: self.stream,
            rate: self.rate,
            play_at_ns: now_ns + finish_in_us * 1_000 - duration_ns,
            stereo,
        })
    }
}

#[derive(Default)]
pub struct MusicSource {
    analysis: Option<(u32, Analyzer)>,
    queue: TimedQueue<Spectrum>,
    spectra: Vec<(u64, Spectrum)>,
}

impl MusicSource {
    pub fn start(&mut self, now_ns: i64) {
        self.queue.start(now_ns);
    }

    pub fn stop(&mut self) {
        self.queue.stop();
        self.analysis = None;
    }

    pub fn next_tick_ns(&self) -> Option<i64> {
        self.queue.next_tick_ns()
    }

    pub fn push(&mut self, chunk: &MusicChunk) -> bool {
        let same = self
            .analysis
            .as_ref()
            .is_some_and(|(stream, a)| *stream == chunk.stream && a.rate() == chunk.rate);
        if !same {
            self.analysis = Some((chunk.stream, Analyzer::new(chunk.rate)));
            self.queue.clear();
        }
        let Some((_, analyzer)) = self.analysis.as_mut() else {
            return true;
        };
        let start = analyzer.position();
        self.spectra.clear();
        analyzer.push(&chunk.stereo, &mut self.spectra);
        let rate = i64::from(chunk.rate);
        let mut kept = true;
        for (pos, spectrum) in self.spectra.drain(..) {
            let frames =
                i64::try_from(pos).unwrap_or(i64::MAX) - i64::try_from(start).unwrap_or(i64::MAX);
            kept &= self
                .queue
                .push(chunk.play_at_ns + frames * SECOND_NS / rate, spectrum);
        }
        kept
    }

    pub fn tick(&mut self, now_ns: i64) -> Option<Spectrum> {
        self.queue.tick(now_ns)
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: i64 = 1_000_000;

    #[test]
    fn a_tick_draws_the_first_item_due_within_the_next_40_ms() {
        let mut q = TimedQueue::default();
        q.start(0);
        assert_eq!(q.next_tick_ns(), Some(0));
        q.push(100 * MS, 'a');
        q.push(140 * MS, 'b');
        assert_eq!(q.tick(0), None, "100 ms ahead: not due");
        assert_eq!(q.next_tick_ns(), Some(40 * MS));
        assert_eq!(q.tick(59 * MS), None);
        assert_eq!(q.tick(60 * MS), Some('a'), "due 40 ms early");
        assert_eq!(q.next_tick_ns(), Some(140 * MS), "resynced to the item");
        assert_eq!(q.tick(140 * MS), Some('b'));
        assert_eq!(q.next_tick_ns(), Some(180 * MS));
        assert_eq!(q.tick(180 * MS), None);
        assert_eq!(q.next_tick_ns(), Some(220 * MS));
    }

    #[test]
    fn items_older_than_80_ms_are_skipped_and_a_late_one_catches_up_at_once() {
        let mut q = TimedQueue::default();
        q.start(0);
        for (i, c) in ['a', 'b', 'c', 'd'].into_iter().enumerate() {
            q.push(i as i64 * 40 * MS, c);
        }
        assert_eq!(q.tick(81 * MS), Some('b'), "a is 81 ms old, b 41 ms");
        assert_eq!(
            q.next_tick_ns(),
            Some(80 * MS),
            "behind: the next tick is due now"
        );
        assert_eq!(q.tick(81 * MS), Some('c'));
        assert_eq!(q.tick(120 * MS), Some('d'));
        assert!(q.is_empty());
        q.push(0, 'e');
        assert_eq!(q.tick(81 * MS), None);
        assert!(q.is_empty());
    }

    #[test]
    fn the_queue_holds_126_items_and_stops_clear_it() {
        let mut q = TimedQueue::default();
        for i in 0..126 {
            assert!(q.push(i, i));
        }
        assert!(!q.push(126, 126));
        assert_eq!(q.len(), 126);
        q.start(5);
        q.stop();
        assert!(q.is_empty());
        assert_eq!(q.next_tick_ns(), None);
    }

    fn stereo_bytes(frames: &[[i16; 2]]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|f| f.iter().flat_map(|s| s.to_le_bytes()))
            .collect()
    }

    #[test]
    fn a_chunk_is_timed_by_its_first_frame() {
        let f = TapFormat::default().next(48_000, 2, 16);
        assert_eq!(f.stream, 1);
        let bytes = stereo_bytes(&[[1, -2], [3, -4], [5, -6]]);
        let c = f.chunk(&bytes, 100_000, 7 * MS).unwrap();
        assert_eq!(c.stereo, [1, -2, 3, -4, 5, -6]);
        assert_eq!(c.play_at_ns, 7 * MS + 100 * MS - 62_500);
        assert_eq!((c.stream, c.rate), (1, 48_000));
        let half = f.chunk(&bytes[..9], 100_000, 0).unwrap();
        assert_eq!(half.stereo, [1, -2, 3, -4], "whole frames only");
    }

    #[test]
    fn mono_is_doubled_and_untimed_or_unknown_formats_are_skipped() {
        let mono = TapFormat::default().next(44_100, 1, 16);
        let c = mono.chunk(&[1, 0, 0xff, 0xff], 1, 0).unwrap();
        assert_eq!(c.stereo, [1, 1, -1, -1]);
        let stereo = TapFormat::default().next(48_000, 2, 16);
        assert_eq!(stereo.chunk(&[0; 8], 0, 0), None, "no finish time");
        assert_eq!(stereo.chunk(&[0; 3], 10, 0), None, "no whole frame");
        assert_eq!(
            TapFormat::default()
                .next(48_000, 2, 24)
                .chunk(&[0; 12], 10, 0),
            None
        );
        assert_eq!(
            TapFormat::default()
                .next(48_000, 6, 16)
                .chunk(&[0; 12], 10, 0),
            None
        );
        assert_eq!(TapFormat::default().chunk(&[0; 4], 10, 0), None);
    }

    fn chunk(stream: u32, rate: u32, play_at_ns: i64, frames: usize) -> MusicChunk {
        MusicChunk {
            stream,
            rate,
            play_at_ns,
            stereo: vec![100; 2 * frames],
        }
    }

    fn drain(m: &mut MusicSource, until_ns: i64) -> Vec<(i64, bool)> {
        let mut drawn = Vec::new();
        while let Some(at) = m.next_tick_ns().filter(|&t| t <= until_ns) {
            drawn.push((at, m.tick(at).is_some()));
        }
        drawn
    }

    #[test]
    fn spectra_are_drawn_when_their_interval_starts_playing() {
        let mut m = MusicSource::default();
        m.start(0);
        m.push(&chunk(1, 44_100, 150 * MS, 441));
        assert_eq!(m.queued(), 0, "the first 1024 frames are not in yet");
        m.push(&chunk(1, 44_100, 160 * MS, 1_764 * 2));
        assert_eq!(m.queued(), 2);
        let drawn = drain(&mut m, 300 * MS);
        let shown: Vec<_> = drawn.iter().filter(|d| d.1).map(|d| d.0).collect();
        assert_eq!(shown, [120 * MS, 150 * MS + 40 * MS]);
        assert!(drawn.iter().take_while(|d| !d.1).all(|d| d.0 < 120 * MS));
    }

    #[test]
    fn a_new_stream_restarts_the_analysis_and_drops_what_was_queued() {
        let mut m = MusicSource::default();
        m.start(0);
        m.push(&chunk(1, 48_000, 500 * MS, 1_920));
        assert_eq!(m.queued(), 1);
        m.push(&chunk(2, 48_000, 200 * MS, 1_000));
        assert_eq!(m.queued(), 0);
        m.push(&chunk(2, 48_000, 200 * MS + 1_000 * MS / 48, 24));
        assert_eq!(m.queued(), 1);
        assert_eq!(m.tick(160 * MS).map(|_| ()), Some(()), "due at 200 ms");
        m.push(&chunk(2, 44_100, 300 * MS, 1_024));
        assert_eq!(m.queued(), 1, "a new rate is a new stream too");
        m.stop();
        assert_eq!(m.queued(), 0);
        assert_eq!(m.next_tick_ns(), None);
    }
}
