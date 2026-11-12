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
}
