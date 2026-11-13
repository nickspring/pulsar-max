use crate::chen14_multi::Chen14MultiConfig;
use crate::config::{Flags, flag, number, positive, window};

#[derive(Clone, Debug, PartialEq)]
pub struct CommandConfig {
    pub decoder: Chen14MultiConfig,
    pub phrases: Vec<String>,
    pub outputs: usize,
    pub batch_frames: usize,
    pub version: String,
}

/// Rejects the settings known to change detections that this crate does not implement, as
/// `config::parse_flags` does; other keys are ignored.
pub fn parse_command_config(flags: &str, words: &str) -> Result<CommandConfig, String> {
    let words = parse_words(words)?;
    let flags = Flags::parse(flags);
    flags.expect("spotter-type", None, "chen14", Some)?;
    flags.expect("am-type", None, "nnet", Some)?;
    flags.expect("feat-frontend", Some("mfcc"), "log-mel-fbank", Some)?;
    flags.expect("sample-rate", Some(8000.0), 16000.0, number)?;
    flags.expect("frame-size", Some(25.0), 25.0, number)?;
    flags.expect("frame-shift", Some(10.0), 10.0, number)?;
    flags.expect("dither", Some(0.1), 0.1, number)?;
    flags.expect("preemph-coeff", Some(0.97), 0.97, number)?;
    flags.expect("remove-dc-offset", Some(true), true, flag)?;
    flags.expect("mel-num-bins", Some(23.0), 40.0, number)?;
    flags.expect("mel-low-freq", Some(20.0), 20.0, number)?;
    flags.expect("mel-high-freq", Some(0.0), 0.0, number)?;
    let pipeline = flags.get("feat-pipeline", Some("cmn:lda"), Some)?;
    if !pipeline.split(':').all(str::is_empty) {
        return Err(format!("feat-pipeline: {pipeline} unsupported, only empty"));
    }
    flags.expect("skip-rate", Some(1.0), 1.0, number)?;
    flags.expect("need-priors", Some(true), false, flag)?;
    flags.expect("omni-channel", Some(true), true, flag)?;
    flags.expect("bnr-channel", Some(false), false, flag)?;
    flags.expect("use-vad", Some(true), false, flag)?;
    flags.expect("verifier-enabled", Some(false), false, flag)?;
    flags.expect("rms-blocker-enabled", Some(false), false, flag)?;
    flags.expect("freq-filter-enabled", Some(false), false, flag)?;
    flags.expect("metric", Some("geometric"), "geometric", Some)?;
    flags.expect("hit-at-peak", Some("off"), "subhit", Some)?;
    flags.expect("dynamic-averaging", Some(true), true, flag)?;
    flags.expect(
        "counteractivation-features-enabled",
        Some(false),
        false,
        flag,
    )?;
    for key in [
        "fire-at-subthreshold-phrases",
        "fire-at-two-subthresholds-phrases",
    ] {
        if !flags.get(key, Some(""), Some)?.is_empty() {
            return Err(format!("{key}: unsupported"));
        }
    }
    for key in [
        "dynamic-threshold-value",
        "dynamic-threshold-last-frame",
        "score-with-hmm-model",
    ] {
        if flags.values.contains_key(key) {
            return Err(format!("{key}: unsupported"));
        }
    }

    let commands: Vec<Vec<usize>> = flags.get("commands", None, |v| {
        v.split(':')
            .map(|c| {
                c.split(',')
                    .map(|u| u.parse().ok().filter(|&u: &usize| u < words.len()))
                    .collect()
            })
            .collect()
    })?;
    let per_command = |v: &str, parse: fn(&str) -> Option<f32>| {
        let list: Option<Vec<f32>> = v.split(':').map(parse).collect();
        list.filter(|l| l.len() == commands.len())
    };
    let thresholds = flags.get("thresholds", None, |v| per_command(v, positive))?;
    flags.get("subthresholds", None, |v| per_command(v, number))?;
    let decoder = Chen14MultiConfig {
        thresholds,
        w_smooth: flags.get("w-smooth", None, window)?,
        w_max: flags.get("w-max", Some(100), window)?,
        interval_after_hit: flags.get("interval-after-last-activation", Some(0), |v| {
            v.parse().ok()
        })?,
        zero_after_hit: flags.get("zeroing-confidence-buffer-after-hit", Some(false), flag)?,
        commands,
    };
    let phrases = decoder
        .commands
        .iter()
        .map(|units| {
            units
                .iter()
                .map(|&u| words[u])
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    Ok(CommandConfig {
        decoder,
        phrases,
        outputs: words.len(),
        batch_frames: flags.get("am-batch-size", Some(1), window)?,
        version: flags.get("version", Some(""), Some)?.to_string(),
    })
}

fn parse_words(text: &str) -> Result<Vec<&str>, String> {
    let words: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .enumerate()
        .map(
            |(i, line)| match line.split_whitespace().collect::<Vec<_>>()[..] {
                [word, index] if index.parse() == Ok(i) => Ok(word),
                _ => Err(format!("words.txt: line '{line}' is not '<word> {i}'")),
            },
        )
        .collect::<Result<_, _>>()?;
    if words.is_empty() {
        return Err("words.txt: empty".into());
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAGS: &str = include_str!("../tests/fixtures/command_flags.txt");
    const WORDS: &str = include_str!("../tests/fixtures/command_words.txt");

    const MINIMAL: &str = "spotter-type chen14
am-type nnet
feat-frontend log-mel-fbank
sample-rate 16000
mel-num-bins 40
feat-pipeline
need-priors false
use-vad false
hit-at-peak subhit
commands 1:2,1
w-smooth 5
thresholds 0.5:0.25
subthresholds 0.5:0.125";

    fn with(text: &str, key: &str, line: Option<&str>) -> String {
        text.lines()
            .filter(|l| l.split([' ', '=']).next() != Some(key))
            .chain(line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn parses_the_synthetic_station_layout() {
        let want = CommandConfig {
            decoder: Chen14MultiConfig {
                commands: vec![
                    vec![1],
                    vec![2],
                    vec![3, 4],
                    vec![6, 4],
                    vec![7],
                    vec![1, 2, 7],
                ],
                thresholds: vec![0.714, 0.091, 0.306, 0.522, 0.285, 0.207],
                w_smooth: 6,
                w_max: 30,
                interval_after_hit: 12,
                zero_after_hit: true,
            },
            phrases: [
                "один",
                "два",
                "три четыре",
                "пять четыре",
                "шесть",
                "один два шесть",
            ]
            .map(String::from)
            .to_vec(),
            outputs: 8,
            batch_frames: 4,
            version: "synthetic-commands".into(),
        };
        assert_eq!(parse_command_config(FLAGS, WORDS), Ok(want));
    }

    #[test]
    fn missing_optional_keys_take_io_sdk_defaults() {
        let cfg = parse_command_config(MINIMAL, "SIL 0\nда 1\nнет 2\n").unwrap();
        assert_eq!(cfg.decoder.w_max, 100);
        assert_eq!(cfg.decoder.interval_after_hit, 0);
        assert!(!cfg.decoder.zero_after_hit);
        assert_eq!((cfg.batch_frames, cfg.outputs), (1, 3));
        assert_eq!(cfg.phrases, ["да", "нет да"]);
        assert_eq!(cfg.version, "");
    }

    #[test]
    fn accepts_the_defaults_written_out() {
        let text = format!(
            "{MINIMAL}\nfeat-pipeline :\nmetric geometric\nbnr-channel false\nomni-channel true\n\
             dynamic-averaging true\nfreq-filter-enabled false\nfire-at-subthreshold-phrases\n\
             skip-rate 1\nsome-unmodelled-key 1"
        );
        let words = "SIL 0\nда 1\nнет 2";
        assert_eq!(
            parse_command_config(&text, words),
            parse_command_config(MINIMAL, words)
        );
    }

    #[test]
    fn rejects_what_this_crate_cannot_reproduce() {
        let cases = [
            ("spotter-type", None),
            ("spotter-type", Some("spotter-type kws")),
            ("am-type", None),
            ("am-type", Some("am-type tflite")),
            ("feat-frontend", Some("feat-frontend mfcc")),
            ("sample-rate", Some("sample-rate 8000")),
            ("frame-size", Some("frame-size 20")),
            ("frame-shift", Some("frame-shift 15")),
            ("dither", Some("dither 0")),
            ("preemph-coeff", Some("preemph-coeff 0.9")),
            ("remove-dc-offset", Some("remove-dc-offset false")),
            ("mel-num-bins", Some("mel-num-bins 80")),
            ("mel-low-freq", Some("mel-low-freq 40")),
            ("mel-high-freq", Some("mel-high-freq 7600")),
            ("feat-pipeline", None),
            ("feat-pipeline", Some("feat-pipeline cmn")),
            ("skip-rate", Some("skip-rate 2")),
            ("need-priors", Some("need-priors true")),
            ("omni-channel", Some("omni-channel false")),
            ("bnr-channel", Some("bnr-channel true")),
            ("use-vad", Some("use-vad true")),
            ("verifier-enabled", Some("verifier-enabled true")),
            ("rms-blocker-enabled", Some("rms-blocker-enabled true")),
            ("freq-filter-enabled", Some("freq-filter-enabled true")),
            ("metric", Some("metric minimum")),
            ("hit-at-peak", None),
            ("hit-at-peak", Some("hit-at-peak hit")),
            ("dynamic-averaging", Some("dynamic-averaging false")),
            (
                "counteractivation-features-enabled",
                Some("counteractivation-features-enabled true"),
            ),
            (
                "fire-at-subthreshold-phrases",
                Some("fire-at-subthreshold-phrases один"),
            ),
            (
                "fire-at-two-subthresholds-phrases",
                Some("fire-at-two-subthresholds-phrases один"),
            ),
            (
                "dynamic-threshold-value",
                Some("dynamic-threshold-value 0.5"),
            ),
            (
                "dynamic-threshold-last-frame",
                Some("dynamic-threshold-last-frame 9"),
            ),
            ("score-with-hmm-model", Some("score-with-hmm-model true")),
            ("commands", None),
            ("commands", Some("commands 1:8")),
            ("commands", Some("commands 1::2")),
            ("commands", Some("commands 1,:2")),
            ("thresholds", None),
            ("thresholds", Some("thresholds 0.5:0.5")),
            ("thresholds", Some("thresholds 0.5:0.5:0.5:0.5:0.5:0")),
            ("subthresholds", None),
            ("subthresholds", Some("subthresholds 0.5")),
            (
                "subthresholds",
                Some("subthresholds 0.5:0.5:0.5:0.5:0.5:NaN"),
            ),
            ("w-smooth", None),
            ("w-smooth", Some("w-smooth 0")),
            ("w-max", Some("w-max 0")),
            (
                "interval-after-last-activation",
                Some("interval-after-last-activation -1"),
            ),
            (
                "zeroing-confidence-buffer-after-hit",
                Some("zeroing-confidence-buffer-after-hit yes"),
            ),
            ("am-batch-size", Some("am-batch-size 0")),
        ];
        for (key, line) in cases {
            let e = parse_command_config(&with(FLAGS, key, line), WORDS)
                .expect_err(&format!("{key}: {line:?}"));
            assert!(e.starts_with(&format!("{key}: ")), "{key}: {line:?} -> {e}");
        }
    }

    #[test]
    fn rejects_bad_words() {
        for words in ["", "SIL 0\nда 2", "SIL 0\nда", "SIL 0\nда 1 x", "SIL x"] {
            let e = parse_command_config(MINIMAL, words).expect_err(words);
            assert!(e.starts_with("words.txt: "), "{words:?} -> {e}");
        }
        let e = parse_command_config(MINIMAL, "SIL 0\nда 1").unwrap_err();
        assert!(e.starts_with("commands: "), "{e}");
    }
}
