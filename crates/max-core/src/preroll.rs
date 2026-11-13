use std::collections::VecDeque;

/// The wake thread scores behind the live stream: this holds what followed the detected frame.
#[derive(Debug)]
pub struct Preroll {
    buf: VecDeque<i16>,
    end: u64,
    cap: usize,
}

impl Preroll {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(cap),
            end: 0,
            cap,
        }
    }

    pub fn push(&mut self, pos: u64, samples: &[i16]) {
        if pos != self.end {
            self.buf.clear();
        }
        self.buf.extend(samples);
        self.end = pos + samples.len() as u64;
        let excess = self.buf.len().saturating_sub(self.cap);
        self.buf.drain(..excess);
    }

    pub fn since(&self, pos: u64) -> Vec<i16> {
        let start = self.end - self.buf.len() as u64;
        let skip = usize::try_from(pos.saturating_sub(start)).unwrap_or(usize::MAX);
        self.buf.iter().skip(skip).copied().collect()
    }
}

const SAMPLES_PER_MS: i64 = 16;

/// pulsar's "Command start offset": positive moves the start later, toward the command.
pub fn command_start(wake_end: u64, offset_ms: i32) -> u64 {
    let shift = i64::from(offset_ms) * SAMPLES_PER_MS;
    let pos = i64::try_from(wake_end)
        .unwrap_or(i64::MAX)
        .saturating_add(shift);
    u64::try_from(pos).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(from: i16, n: usize) -> Vec<i16> {
        (0..n).map(|i| from + i as i16).collect()
    }

    #[test]
    fn returns_the_samples_after_a_position() {
        let mut p = Preroll::new(100);
        p.push(0, &ramp(0, 10));
        p.push(10, &ramp(10, 10));
        assert_eq!(p.since(15), ramp(15, 5));
        assert_eq!(p.since(20), Vec::<i16>::new());
        assert_eq!(p.since(99), Vec::<i16>::new());
    }

    #[test]
    fn a_position_older_than_the_ring_gives_everything_held() {
        let mut p = Preroll::new(8);
        p.push(0, &ramp(0, 10));
        p.push(10, &ramp(10, 4));
        assert_eq!(p.since(0), ramp(6, 8));
    }

    #[test]
    fn a_gap_drops_the_audio_before_it() {
        let mut p = Preroll::new(100);
        p.push(0, &ramp(0, 10));
        p.push(25, &ramp(25, 5));
        assert_eq!(p.since(0), ramp(25, 5));
        assert_eq!(p.since(27), ramp(27, 3));
    }

    #[test]
    fn empty_ring_returns_nothing() {
        let p = Preroll::new(4);
        assert!(p.since(0).is_empty());
    }

    #[test]
    fn command_start_shifts_by_16_samples_per_ms() {
        assert_eq!(command_start(16_000, 0), 16_000);
        assert_eq!(command_start(16_000, 200), 19_200);
        assert_eq!(command_start(16_000, -100), 14_400);
        assert_eq!(command_start(800, -100), 0);
    }
}
