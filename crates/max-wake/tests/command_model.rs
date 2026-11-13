use std::path::{Path, PathBuf};
use std::time::Instant;

use max_wake::command::CommandSpotter;
use max_wake::fbank::MonoFbank;
use max_wake::nnet::Nnet;

const CHUNK: usize = 320;

/// MAX_CMD_MODEL=<copy of the station's command spotter_model dir> MAX_CMD_RAW=<16 kHz s16 mono>
/// cargo test --release -p max-wake --test command_model -- --ignored --nocapture
#[test]
#[ignore = "needs a local copy of the station's command model and a recording"]
fn command_model_on_a_recording() {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let dir = PathBuf::from(var("MAX_CMD_MODEL"));
    let read = |file: &str| std::fs::read(dir.join(file)).expect(file);
    let text = |file: &str| String::from_utf8(read(file)).expect(file);
    let pcm: Vec<i16> = std::fs::read(var("MAX_CMD_RAW"))
        .expect("MAX_CMD_RAW")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b))
        .collect();

    let mut spotter = CommandSpotter::new(
        &text("flags.txt"),
        &text("words.txt"),
        &read("acoustic_model.nnet"),
    )
    .expect("model");
    let started = Instant::now();
    for chunk in pcm.chunks(CHUNK) {
        for hit in spotter.push(chunk) {
            let frame = (hit.end - 400) / 160;
            println!(
                "{} {} (end {}, confidence {:.3})",
                hit.phrase,
                frame * 10,
                hit.end,
                hit.confidence
            );
        }
    }
    let elapsed = started.elapsed();
    let seconds = pcm.len() as f64 / 16_000.0;
    println!(
        "{seconds:.1} s of audio in {:.3} s: {:.2} ms per second of audio",
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / seconds
    );
    if let Ok(out) = std::env::var("MAX_CMD_DUMP") {
        dump(Path::new(&out), &pcm, &read("acoustic_model.nnet"));
    }
}

fn dump(out: &Path, pcm: &[i16], nnet: &[u8]) {
    let mut features = Vec::new();
    MonoFbank::new().push(pcm, &mut features);
    let mut posteriors = Vec::new();
    Nnet::read(nnet)
        .expect("model")
        .forward(&features, &mut posteriors);
    let write = |file: &str, values: &[f32]| {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(out.join(file), bytes).expect(file);
    };
    write("features.f32le", &features);
    write("posteriors.f32le", &posteriors);
}
