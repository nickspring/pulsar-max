use crate::chen14_multi::Chen14Multi;
use crate::command_config::parse_command_config;
use crate::fbank::{FRAME_LEN, FRAME_SHIFT, MEL_BINS, MonoFbank, RATE};
use crate::nnet::Nnet;

// pulsar's per-spotter debounce: io.sdk refires on one utterance once the post-hit interval ends.
const DEBOUNCE_SAMPLES: u64 = 1200 * RATE as u64 / 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct CommandHit {
    pub command: usize,
    pub phrase: String,
    /// Samples since `new`/`reset` up to the end of the hit frame.
    pub end: u64,
    pub confidence: f32,
}

pub struct CommandSpotter {
    fbank: MonoFbank,
    net: Nnet,
    decoder: Chen14Multi,
    phrases: Vec<String>,
    batch_frames: usize,
    features: Vec<f32>,
    posteriors: Vec<f32>,
    last_hit_end: Option<u64>,
}

impl CommandSpotter {
    pub fn new(flags: &str, words: &str, nnet: &[u8]) -> Result<Self, String> {
        let cfg = parse_command_config(flags, words)?;
        let net = Nnet::read(nnet)?;
        if net.input_dim() != MEL_BINS {
            return Err(format!(
                "nnet3: input dim {}, the fbank gives {MEL_BINS}",
                net.input_dim()
            ));
        }
        if net.output_dim() != cfg.outputs {
            return Err(format!(
                "nnet3: {} outputs, words.txt has {}",
                net.output_dim(),
                cfg.outputs
            ));
        }
        Ok(Self {
            fbank: MonoFbank::new(),
            net,
            decoder: Chen14Multi::new(&cfg.decoder),
            phrases: cfg.phrases,
            batch_frames: cfg.batch_frames,
            features: Vec::new(),
            posteriors: Vec::new(),
            last_hit_end: None,
        })
    }

    pub fn phrases(&self) -> &[String] {
        &self.phrases
    }

    pub fn push(&mut self, vqe: &[i16]) -> Vec<CommandHit> {
        let mut hits = Vec::new();
        self.fbank.push(vqe, &mut self.features);
        for batch in self.features.chunks(self.batch_frames * MEL_BINS) {
            self.posteriors.clear();
            self.net.forward(batch, &mut self.posteriors);
            for frame in self.posteriors.chunks_exact(self.net.output_dim()) {
                let Some(hit) = self.decoder.push(frame) else {
                    continue;
                };
                let end = hit.frame * FRAME_SHIFT as u64 + FRAME_LEN as u64;
                if self
                    .last_hit_end
                    .is_some_and(|last| end - last < DEBOUNCE_SAMPLES)
                {
                    continue;
                }
                self.last_hit_end = Some(end);
                hits.push(CommandHit {
                    command: hit.command,
                    phrase: self.phrases[hit.command].clone(),
                    end,
                    confidence: hit.confidence,
                });
            }
        }
        self.features.clear();
        hits
    }

    pub fn reset(&mut self) {
        self.fbank.reset();
        self.net.reset();
        self.decoder.reset();
        self.features.clear();
        self.last_hit_end = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::command_signal;
    use crate::fixtures::nnet3::{affine, model, softmax_component};

    const FLAGS: &str = include_str!("../tests/fixtures/command_flags.txt");
    const WORDS: &str = include_str!("../tests/fixtures/command_words.txt");
    const TINY: &[u8] = include_bytes!("../tests/fixtures/command_tiny.nnet");

    fn tiny() -> CommandSpotter {
        CommandSpotter::new(FLAGS, WORDS, TINY).unwrap()
    }

    /// command_hits.txt of golden_command.py: (frame, command) before the debounce.
    fn reference_hits() -> Vec<(u64, usize)> {
        include_str!("../tests/fixtures/command_hits.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let (frame, command) = l.split_once(' ').unwrap();
                (frame.parse().unwrap(), command.parse().unwrap())
            })
            .collect()
    }

    fn end(frame: u64) -> u64 {
        frame * FRAME_SHIFT as u64 + FRAME_LEN as u64
    }

    fn feed(s: &mut CommandSpotter, audio: &[i16], chunk: usize) -> Vec<(u64, usize)> {
        audio
            .chunks(chunk)
            .flat_map(|c| s.push(c))
            .map(|h| (h.end, h.command))
            .collect()
    }

    #[test]
    fn the_pipeline_agrees_with_the_python_reference() {
        let cfg = parse_command_config(FLAGS, WORDS).unwrap();
        let mut fbank = MonoFbank::new();
        let mut net = Nnet::read(TINY).unwrap();
        let mut decoder = Chen14Multi::new(&cfg.decoder);
        let (mut features, mut posteriors) = (Vec::new(), Vec::new());
        fbank.push(&command_signal(), &mut features);
        net.forward(&features, &mut posteriors);
        let got: Vec<(u64, usize)> = posteriors
            .chunks_exact(net.output_dim())
            .filter_map(|p| decoder.push(p))
            .map(|h| (h.frame, h.command))
            .collect();
        assert_eq!(got, reference_hits());
    }

    #[test]
    fn reports_one_hit_per_1200_ms() {
        let mut kept: Vec<(u64, usize)> = Vec::new();
        for (frame, command) in reference_hits() {
            if kept
                .last()
                .is_none_or(|&(last, _)| end(frame) - last >= DEBOUNCE_SAMPLES)
            {
                kept.push((end(frame), command));
            }
        }
        assert!(kept.len() >= 2 && kept.len() < reference_hits().len());

        let mut s = tiny();
        let hits = s.push(&command_signal());
        let got: Vec<(u64, usize)> = hits.iter().map(|h| (h.end, h.command)).collect();
        assert_eq!(got, kept);
        assert_eq!(hits[0].phrase, s.phrases()[hits[0].command]);
        assert!(
            hits.iter()
                .all(|h| h.confidence > 0.0 && h.confidence <= 1.0)
        );
    }

    #[test]
    fn chunking_and_reset_do_not_change_hits() {
        let signal = command_signal();
        let whole = feed(&mut tiny(), &signal, signal.len());
        let mut s = tiny();
        for chunk in [1, 160, 333, 4000] {
            s.reset();
            assert_eq!(feed(&mut s, &signal, chunk), whole, "chunk {chunk}");
        }
        s.push(&signal[..12_345]);
        s.reset();
        assert_eq!(feed(&mut s, &signal, 320), whole);
    }

    // p("да") rises with the summed log-mel energy: loud frames hit, near-silent ones do not.
    fn loudness_spotter() -> CommandSpotter {
        let config = "input-node name=input dim=40
            component-node name=head component=head input=input
            component-node name=sm component=sm input=head
            output-node name=output input=sm";
        let w: Vec<i16> = [0; MEL_BINS].into_iter().chain([1; MEL_BINS]).collect();
        let net = model(
            config,
            &[
                affine("head", 0.1, 2, &w, &[0.0, -30.0]),
                softmax_component("sm", 2),
            ],
        );
        let flags = "spotter-type chen14\nam-type nnet\nfeat-frontend log-mel-fbank\n\
                     sample-rate 16000\nmel-num-bins 40\nfeat-pipeline\nneed-priors false\n\
                     use-vad false\nhit-at-peak subhit\ncommands 1\nw-smooth 1\nw-max 1\n\
                     thresholds 0.5\nsubthresholds 0.5";
        CommandSpotter::new(flags, "SIL 0\nда 1", &net).unwrap()
    }

    #[test]
    fn the_debounce_window_starts_at_the_last_reported_hit() {
        const BURST: usize = 4800;
        let second = 8000 + DEBOUNCE_SAMPLES as usize;
        let starts = [
            8000,
            second,
            second + 9600,
            second + DEBOUNCE_SAMPLES as usize,
            second + 2 * DEBOUNCE_SAMPLES as usize - FRAME_SHIFT,
        ];
        let mut audio = vec![0i16; starts[4] + 2 * BURST];
        for &start in &starts {
            for (i, s) in audio[start..start + BURST].iter_mut().enumerate() {
                *s = if (i / 10) % 2 == 0 { 8000 } else { -8000 };
            }
        }
        let ends: Vec<u64> = feed(&mut loudness_spotter(), &audio, 320)
            .into_iter()
            .map(|(end, _)| end)
            .collect();
        assert_eq!(ends.len(), 3, "{ends:?}");
        assert!((8000..8000 + BURST as u64).contains(&ends[0]), "{ends:?}");
        assert_eq!(
            ends[1..],
            [ends[0] + DEBOUNCE_SAMPLES, ends[0] + 2 * DEBOUNCE_SAMPLES]
        );
    }

    #[test]
    fn rejects_a_model_that_does_not_fit_the_files() {
        let config = "input-node name=input dim=2
            component-node name=head component=head input=input
            component-node name=sm component=sm input=head
            output-node name=output input=sm";
        let net = model(
            config,
            &[
                affine("head", 1.0, 2, &[1, 0, 0, 1], &[0.0; 2]),
                softmax_component("sm", 2),
            ],
        );
        let e = CommandSpotter::new(FLAGS, WORDS, &net).err().unwrap();
        assert!(e.contains("input dim 2"), "{e}");

        let words = format!("{WORDS}SIL 8\n");
        let e = CommandSpotter::new(FLAGS, &words, TINY).err().unwrap();
        assert!(e.contains("8 outputs, words.txt has 9"), "{e}");

        assert!(CommandSpotter::new(FLAGS, WORDS, &TINY[..100]).is_err());
        assert!(CommandSpotter::new("", WORDS, TINY).is_err());
    }
}
