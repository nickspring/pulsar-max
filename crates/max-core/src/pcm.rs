/// A sample split between two pipe reads is kept for the next one.
#[derive(Debug)]
pub struct S16Framer {
    odd_byte: Option<u8>,
    skip: usize,
}

impl S16Framer {
    pub fn new(skip: usize) -> Self {
        Self {
            odd_byte: None,
            skip,
        }
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<i16>) {
        let start = out.len();
        let mut rest = bytes;
        if let Some(lo) = self.odd_byte {
            let Some((&hi, tail)) = rest.split_first() else {
                return;
            };
            self.odd_byte = None;
            out.push(i16::from_le_bytes([lo, hi]));
            rest = tail;
        }
        let (pairs, remainder) = rest.as_chunks::<2>();
        out.extend(pairs.iter().map(|&p| i16::from_le_bytes(p)));
        self.odd_byte = remainder.first().copied();

        let dropped = self.skip.min(out.len() - start);
        self.skip -= dropped;
        out.drain(start..start + dropped);
    }
}

/// A frame cut by a pipe read waits for the rest, so channels never slip against each other.
#[derive(Debug)]
pub struct Deinterleaver {
    partial: Vec<i16>,
    channels: usize,
}

impl Deinterleaver {
    pub fn new(channels: usize) -> Self {
        assert!(channels > 0);
        Self {
            partial: Vec::with_capacity(channels),
            channels,
        }
    }

    pub fn push(&mut self, samples: &[i16], out: &mut [Vec<i16>]) {
        assert_eq!(out.len(), self.channels);
        let mut rest = samples;
        if !self.partial.is_empty() {
            let take = (self.channels - self.partial.len()).min(rest.len());
            self.partial.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.partial.len() < self.channels {
                return;
            }
            for (ch, &s) in out.iter_mut().zip(&self.partial) {
                ch.push(s);
            }
            self.partial.clear();
        }
        let whole = rest.len() - rest.len() % self.channels;
        for frame in rest[..whole].chunks_exact(self.channels) {
            for (ch, &s) in out.iter_mut().zip(frame) {
                ch.push(s);
            }
        }
        self.partial.extend_from_slice(&rest[whole..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn any_split_of_the_stream_gives_the_same_samples() {
        let samples: Vec<i16> = (0..50).map(|i| (i * 1311 - 30000) as i16).collect();
        let raw = bytes(&samples);
        for split in [1, 2, 3, 7, 64] {
            let mut f = S16Framer::new(0);
            let mut out = Vec::new();
            for chunk in raw.chunks(split) {
                f.push(chunk, &mut out);
            }
            assert_eq!(out, samples, "split {split}");
        }
    }

    #[test]
    fn skip_spans_reads_and_keeps_existing_output() {
        let raw = bytes(&[1, 2, 3, 4, 5]);
        let mut f = S16Framer::new(3);
        let mut out = vec![-7];
        f.push(&raw[..3], &mut out);
        assert_eq!(out, [-7]);
        f.push(&raw[3..], &mut out);
        assert_eq!(out, [-7, 4, 5]);
    }

    #[test]
    fn empty_read_keeps_a_pending_half_sample() {
        let mut f = S16Framer::new(0);
        let mut out = Vec::new();
        f.push(&[0x34], &mut out);
        f.push(&[], &mut out);
        f.push(&[0x12], &mut out);
        assert_eq!(out, [0x1234]);
    }

    fn split(d: &mut Deinterleaver, samples: &[i16], channels: usize) -> Vec<Vec<i16>> {
        let mut out = vec![Vec::new(); channels];
        d.push(samples, &mut out);
        out
    }

    #[test]
    fn splits_interleaved_frames_by_channel() {
        let mut d = Deinterleaver::new(3);
        assert_eq!(
            split(&mut d, &[1, 2, 3, 4, 5, 6], 3),
            [vec![1, 4], vec![2, 5], vec![3, 6]]
        );
    }

    #[test]
    fn any_split_of_the_stream_keeps_channels_aligned() {
        let samples: Vec<i16> = (0..60).collect();
        for size in [1, 2, 4, 5, 7, 60] {
            let mut d = Deinterleaver::new(3);
            let mut out = vec![Vec::new(); 3];
            for chunk in samples.chunks(size) {
                d.push(chunk, &mut out);
            }
            for (c, ch) in out.iter().enumerate() {
                let expected: Vec<i16> = (0..20).map(|f| f * 3 + c as i16).collect();
                assert_eq!(*ch, expected, "chunk size {size}, channel {c}");
            }
        }
    }

    #[test]
    fn an_incomplete_frame_waits_across_several_reads() {
        let mut d = Deinterleaver::new(4);
        assert_eq!(split(&mut d, &[1], 4), vec![Vec::<i16>::new(); 4]);
        assert_eq!(split(&mut d, &[2], 4), vec![Vec::<i16>::new(); 4]);
        assert_eq!(split(&mut d, &[], 4), vec![Vec::<i16>::new(); 4]);
        assert_eq!(
            split(&mut d, &[3, 4, 5], 4),
            [vec![1], vec![2], vec![3], vec![4]]
        );
        assert_eq!(
            split(&mut d, &[6, 7, 8], 4),
            [vec![5], vec![6], vec![7], vec![8]]
        );
    }

    #[test]
    fn one_channel_passes_through() {
        let mut d = Deinterleaver::new(1);
        assert_eq!(split(&mut d, &[5, -6, 7], 1), [vec![5, -6, 7]]);
    }
}
