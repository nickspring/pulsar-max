use std::fs::File;
use std::io::{Read, Write as _};
use std::path::Path;

use max_core::audio::Decimator;
use max_core::pcm::{Deinterleaver, S16Framer};
use max_wake::command::CommandSpotter;
use max_wake::fbank::RATE;
use max_wake::spotter::{AcousticModel, Detection, Spotter};

use crate::capture::{CHANNELS, SKIP_AFTER_START_FRAMES};

/// MaxProbe's `record ... ffff` layout.
const RECORDED_CHANNELS: usize = 16;
const READ_BYTES: usize = 64 * 1024;

/// `--spot <capture.raw>`: the live spotting path, fed from a recording instead of the helper.
pub fn spot_file<M>(
    path: &Path,
    mut spotter: Option<Spotter<M>>,
    mut commands: Option<CommandSpotter>,
) -> Result<(), String>
where
    M: AcousticModel,
    M::Error: std::fmt::Display,
{
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut framer = S16Framer::new(SKIP_AFTER_START_FRAMES * RECORDED_CHANNELS);
    let mut split = Deinterleaver::new(RECORDED_CHANNELS);
    let mut decimators = CHANNELS.map(|_| Decimator::new());
    let mut buf = vec![0u8; READ_BYTES];
    let mut samples = Vec::new();
    let mut all = vec![Vec::new(); RECORDED_CHANNELS];
    if spotter.is_none() && commands.is_none() {
        return Err("no wake word and no command model".into());
    }
    let mut detections = Vec::new();
    let mut hits = Vec::new();
    let mut pos = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        samples.clear();
        framer.push(&buf[..n], &mut samples);
        all.iter_mut().for_each(Vec::clear);
        split.push(&samples, &mut all);
        let [omni, bnr, raw] = std::array::from_fn(|i| {
            let mut out = Vec::new();
            decimators[i].process(&all[CHANNELS[i]], &mut out);
            out
        });
        if let Some(s) = spotter.as_mut() {
            s.push(pos, &omni, &bnr, &raw, &mut detections)
                .map_err(|e| format!("model: {e}"))?;
        }
        if let Some(c) = commands.as_mut() {
            hits.extend(c.push(&omni));
        }
        pos += omni.len() as u64;
    }
    let seconds = |at: u64| at as f64 / f64::from(RATE);
    let mut out = std::io::stdout().lock();
    for d in &detections {
        let line = match *d {
            Detection::Wake { end, confidence } => {
                format!("wake {:.2} s confidence {confidence:.3}", seconds(end))
            }
            Detection::Blocked {
                end,
                confidence,
                filter,
            } => format!(
                "blocked {:.2} s confidence {confidence:.3} filter {filter:.2}",
                seconds(end)
            ),
        };
        let _ = writeln!(out, "{line}");
    }
    for h in &hits {
        let _ = writeln!(
            out,
            "command '{}' {:.2} s confidence {:.3}",
            h.phrase,
            seconds(h.end),
            h.confidence
        );
    }
    let _ = writeln!(
        out,
        "{} detections, {} commands in {:.1} s",
        detections.len(),
        hits.len(),
        seconds(pos)
    );
    Ok(())
}
