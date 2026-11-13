use std::collections::HashMap;
use std::fmt::Display;

use crate::chen14::Chen14Config;
use crate::freqfilter::FreqFilterConfig;
use crate::spotter::BATCH_FRAMES;

#[derive(Clone, Debug, PartialEq)]
pub struct SpotterConfig {
    pub chen14: Chen14Config,
    pub freq_filter: Option<FreqFilterConfig>,
    pub phrase: String,
    pub version: String,
}

/// Rejects the flags.txt settings known to change detections that this crate does not implement.
/// The last of repeated keys wins; other keys are ignored, `use-tts-blocker` among them
/// (docs/max.md, "Ключевое слово").
pub fn parse_flags(text: &str) -> Result<SpotterConfig, String> {
    let flags = Flags::parse(text);
    flags.expect("spotter-type", None, "chen14", Some)?;
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
    flags.expect("bnr-channel", None, true, flag)?;
    flags.expect("use-vad", Some(true), false, flag)?;
    flags.expect("spotter-npu-use-streaming", Some(false), true, flag)?;
    flags.expect("spotter-npu-use-sigmoid", Some(false), false, flag)?;
    flags.expect("verifier-enabled", Some(false), false, flag)?;
    flags.expect("rms-blocker-enabled", Some(false), false, flag)?;
    flags.expect("commands", None, 1.0, number)?;
    flags.expect("metric", Some("geometric"), "geometric", Some)?;
    flags.expect("hit-at-peak", Some("off"), "subhit", Some)?;
    flags.expect("dynamic-averaging", Some(true), true, flag)?;
    flags.expect(
        "zeroing-confidence-buffer-after-hit",
        Some(false),
        false,
        flag,
    )?;
    flags.expect("interval-after-last-activation", Some(0.0), 0.0, number)?;
    flags.expect(
        "spotter-tflite-activation-function",
        Some("softmax"),
        "softmax",
        Some,
    )?;
    let stride = flags.get("spotter-npu-stride", Some(-1.0), number)?;
    if stride != -1.0 && stride != BATCH_FRAMES as f32 {
        return Err(format!(
            "spotter-npu-stride: {stride} unsupported, only {BATCH_FRAMES}"
        ));
    }
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
    for key in ["dynamic-threshold-value", "dynamic-threshold-last-frame"] {
        if flags.values.contains_key(key) {
            return Err(format!("{key}: unsupported"));
        }
    }
    let chen14 = Chen14Config {
        w_smooth: flags.get("w-smooth", None, window)?,
        w_max: flags.get("w-max", Some(100), window)?,
        threshold: flags.get("thresholds", None, positive)?,
        subthreshold: flags.get("subthresholds", None, number)?,
    };
    let freq_filter = if flags.get("freq-filter-enabled", Some(false), flag)? {
        Some(freq_filter(&flags)?)
    } else {
        None
    };
    let phrase = flags.get("string-representations", None, |v| {
        Some(v).filter(|v| !v.is_empty())
    })?;
    Ok(SpotterConfig {
        chen14,
        freq_filter,
        phrase: phrase.to_string(),
        version: flags.get("version", Some(""), Some)?.to_string(),
    })
}

fn freq_filter(flags: &Flags) -> Result<FreqFilterConfig, String> {
    flags.expect("freq-filter-audio-channel", Some("omni"), "raw-mic", Some)?;
    let inside = "freq-filter-aggregation-func-inside";
    flags.expect(inside, Some("mean"), "mean", Some)?;
    let outside = "freq-filter-aggregation-func-outside";
    flags.expect(outside, Some("mean"), "median", Some)?;
    flags.expect("freq-filter-reduce-features-first", Some(true), true, flag)?;
    flags.expect("freq-filter-react-as-subhits", Some(true), true, flag)?;
    Ok(FreqFilterConfig {
        frequencies_hz: flags.get("freq-filter-frequencies-hz", None, |v| {
            v.split(':')
                .map(|f| number(f).filter(|f| (10.0..8000.0).contains(f)))
                .collect()
        })?,
        threshold: flags.get("freq-filter-threshold", None, number)?,
        frame_ms: flags.get("freq-filter-frame-size", Some(512.0), positive)?,
        shift_ms: flags.get("freq-filter-frame-shift", Some(128.0), positive)?,
        left_context_ms: flags.get("freq-filter-left-context", Some(0.0), number)?,
        buffer_ms: flags.get("freq-filter-buffer-size", Some(3000.0), positive)?,
        preemph: flags.get("freq-filter-preemph-coeff", Some(0.97), |v| {
            number(v).filter(|c| (0.0..=1.0).contains(c))
        })?,
        dither: flags.get("dither", Some(0.1), number)?,
        inside_smooth: flags.get("freq-filter-inside-smooth", Some(2), |v| v.parse().ok())?,
        outside_smooth: flags.get("freq-filter-outside-smooth", Some(10), |v| v.parse().ok())?,
        lower_ratio: flags.get("freq-filter-lower-freq-ratio", Some(0.9), number)?,
        higher_ratio: flags.get("freq-filter-higher-freq-ratio", Some(1.1), number)?,
        dyn_smooth: flags.get("freq-filter-use-dyn-freq-smooth", Some(false), flag)?,
    })
}

pub(crate) fn number(v: &str) -> Option<f32> {
    v.parse().ok().filter(|x: &f32| x.is_finite())
}

pub(crate) fn positive(v: &str) -> Option<f32> {
    number(v).filter(|&x| x > 0.0)
}

pub(crate) fn window(v: &str) -> Option<usize> {
    v.parse().ok().filter(|&frames| frames > 0)
}

pub(crate) fn flag(v: &str) -> Option<bool> {
    match v {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

pub(crate) struct Flags<'a> {
    pub(crate) values: HashMap<&'a str, &'a str>,
}

impl<'a> Flags<'a> {
    pub(crate) fn parse(text: &'a str) -> Self {
        let values = text
            .split(['\n', '\r'])
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| match line.split_once([' ', '=']) {
                Some((key, value)) => (key, value.trim()),
                None => (line, ""),
            })
            .collect();
        Self { values }
    }

    pub(crate) fn get<T>(
        &self,
        key: &str,
        default: Option<T>,
        parse: impl FnOnce(&'a str) -> Option<T>,
    ) -> Result<T, String> {
        match self.values.get(key) {
            Some(&value) => parse(value).ok_or_else(|| format!("{key}: bad value '{value}'")),
            None => default.ok_or_else(|| format!("{key}: missing")),
        }
    }

    pub(crate) fn expect<T: PartialEq + Display>(
        &self,
        key: &str,
        default: Option<T>,
        want: T,
        parse: impl FnOnce(&'a str) -> Option<T>,
    ) -> Result<(), String> {
        let got = self.get(key, default, parse)?;
        if got == want {
            Ok(())
        } else {
            Err(format!("{key}: {got} unsupported, only {want}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "spotter-type chen14
feat-frontend log-mel-fbank
sample-rate 16000
mel-num-bins 40
feat-pipeline
need-priors false
bnr-channel true
use-vad false
spotter-npu-use-streaming true
commands 1
hit-at-peak subhit
w-smooth 5
thresholds 0.861
subthresholds 0.435
string-representations алиса";

    /// The values of docs/max.md, "Ключевое слово", plus a key this crate ignores.
    fn full() -> String {
        format!(
            "{MINIMAL}
frame-size 25
frame-shift 10
dither 0.1
w-max 20
metric geometric
some-unmodelled-key 1
freq-filter-enabled true
freq-filter-audio-channel raw-mic
freq-filter-aggregation-func-inside mean
freq-filter-aggregation-func-outside median
freq-filter-frequencies-hz 3501:2089:814:1355:638:4510:2635:486
freq-filter-threshold -3.6
freq-filter-frame-size 252
freq-filter-frame-shift 177
freq-filter-left-context 979
freq-filter-preemph-coeff 0.72
freq-filter-inside-smooth 3
freq-filter-outside-smooth 99
freq-filter-lower-freq-ratio 0.68
freq-filter-higher-freq-ratio 1.45
freq-filter-use-dyn-freq-smooth true
version test-model"
        )
    }

    fn station_with(key: &str, line: Option<&str>) -> String {
        full()
            .lines()
            .filter(|l| l.split([' ', '=']).next() != Some(key))
            .chain(line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn station_chen14() -> Chen14Config {
        Chen14Config {
            w_smooth: 5,
            w_max: 20,
            threshold: 0.861,
            subthreshold: 0.435,
        }
    }

    #[test]
    fn parses_the_station_model() {
        let want = SpotterConfig {
            chen14: station_chen14(),
            freq_filter: Some(FreqFilterConfig {
                frequencies_hz: vec![3501.0, 2089.0, 814.0, 1355.0, 638.0, 4510.0, 2635.0, 486.0],
                threshold: -3.6,
                frame_ms: 252.0,
                shift_ms: 177.0,
                left_context_ms: 979.0,
                buffer_ms: 3000.0,
                preemph: 0.72,
                dither: 0.1,
                inside_smooth: 3,
                outside_smooth: 99,
                lower_ratio: 0.68,
                higher_ratio: 1.45,
                dyn_smooth: true,
            }),
            phrase: "алиса".to_string(),
            version: "test-model".to_string(),
        };
        assert_eq!(parse_flags(&full()), Ok(want));
    }

    #[test]
    fn missing_optional_keys_take_io_sdk_defaults() {
        let cfg = parse_flags(MINIMAL).unwrap();
        assert_eq!(cfg.chen14.w_max, 100);
        assert_eq!(cfg.freq_filter, None);
        assert_eq!(cfg.version, "");

        let text = format!(
            "{MINIMAL}\nfreq-filter-enabled true\nfreq-filter-audio-channel raw-mic\n\
             freq-filter-aggregation-func-outside median\nfreq-filter-frequencies-hz 1000\n\
             freq-filter-threshold -3"
        );
        let want = FreqFilterConfig {
            frequencies_hz: vec![1000.0],
            threshold: -3.0,
            frame_ms: 512.0,
            shift_ms: 128.0,
            left_context_ms: 0.0,
            buffer_ms: 3000.0,
            preemph: 0.97,
            dither: 0.1,
            inside_smooth: 2,
            outside_smooth: 10,
            lower_ratio: 0.9,
            higher_ratio: 1.1,
            dyn_smooth: false,
        };
        assert_eq!(parse_flags(&text).unwrap().freq_filter, Some(want));
    }

    #[test]
    fn accepts_the_defaults_written_out() {
        let text = format!(
            "{MINIMAL}\nfeat-pipeline :\nspotter-tflite-activation-function softmax\n\
             spotter-npu-stride 6\ncounteractivation-features-enabled false\n\
             fire-at-subthreshold-phrases\nuse-tts-blocker true"
        );
        assert_eq!(parse_flags(&text), parse_flags(MINIMAL));
        let text = format!("{MINIMAL}\nspotter-npu-stride -1");
        assert_eq!(parse_flags(&text), parse_flags(MINIMAL));
    }

    #[test]
    fn a_disabled_freq_filter_is_not_validated() {
        let text = station_with("freq-filter-enabled", Some("freq-filter-enabled false"));
        let text = format!("{text}\nfreq-filter-audio-channel omni");
        assert_eq!(parse_flags(&text).unwrap().freq_filter, None);
    }

    #[test]
    fn line_syntax_and_repeated_keys() {
        let text = format!(
            "# comment\r\n\r\n{}\r\nw-smooth=7\r\nthresholds 0.5\r\nthresholds   0.9  \r\n\
             bnr-channel 1\r\ndynamic-averaging=true",
            MINIMAL.replace('\n', "\r\n")
        );
        let cfg = parse_flags(&text).unwrap();
        assert_eq!(
            cfg.chen14,
            Chen14Config {
                w_smooth: 7,
                w_max: 100,
                threshold: 0.9,
                subthreshold: 0.435,
            }
        );
    }

    #[test]
    fn rejects_what_this_crate_cannot_reproduce() {
        let cases = [
            ("spotter-type", None),
            ("spotter-type", Some("spotter-type kws")),
            ("feat-frontend", None),
            ("feat-frontend", Some("feat-frontend mel-fbank")),
            ("sample-rate", None),
            ("sample-rate", Some("sample-rate 8000")),
            ("sample-rate", Some("sample-rate fast")),
            ("frame-size", Some("frame-size 20")),
            ("frame-shift", Some("frame-shift 15")),
            ("dither", Some("dither 1")),
            ("preemph-coeff", Some("preemph-coeff 0.9")),
            ("remove-dc-offset", Some("remove-dc-offset false")),
            ("mel-num-bins", None),
            ("mel-num-bins", Some("mel-num-bins 80")),
            ("mel-low-freq", Some("mel-low-freq 40")),
            ("mel-high-freq", Some("mel-high-freq 7600")),
            ("feat-pipeline", None),
            ("feat-pipeline", Some("feat-pipeline cmn")),
            ("skip-rate", Some("skip-rate 2")),
            ("need-priors", None),
            ("need-priors", Some("need-priors true")),
            ("omni-channel", Some("omni-channel false")),
            ("bnr-channel", None),
            ("bnr-channel", Some("bnr-channel false")),
            ("bnr-channel", Some("bnr-channel yes")),
            ("use-vad", None),
            (
                "spotter-npu-use-streaming",
                Some("spotter-npu-use-streaming false"),
            ),
            (
                "spotter-npu-use-sigmoid",
                Some("spotter-npu-use-sigmoid true"),
            ),
            ("verifier-enabled", Some("verifier-enabled true")),
            ("rms-blocker-enabled", Some("rms-blocker-enabled true")),
            ("commands", None),
            ("commands", Some("commands 2")),
            ("metric", Some("metric minimum")),
            ("hit-at-peak", None),
            ("hit-at-peak", Some("hit-at-peak hit")),
            ("dynamic-averaging", Some("dynamic-averaging false")),
            (
                "zeroing-confidence-buffer-after-hit",
                Some("zeroing-confidence-buffer-after-hit true"),
            ),
            (
                "interval-after-last-activation",
                Some("interval-after-last-activation 10"),
            ),
            (
                "spotter-tflite-activation-function",
                Some("spotter-tflite-activation-function sigmoid"),
            ),
            ("spotter-npu-stride", Some("spotter-npu-stride 3")),
            (
                "counteractivation-features-enabled",
                Some("counteractivation-features-enabled true"),
            ),
            (
                "fire-at-subthreshold-phrases",
                Some("fire-at-subthreshold-phrases алиса"),
            ),
            (
                "fire-at-two-subthresholds-phrases",
                Some("fire-at-two-subthresholds-phrases алиса"),
            ),
            (
                "dynamic-threshold-value",
                Some("dynamic-threshold-value 0.5"),
            ),
            (
                "dynamic-threshold-last-frame",
                Some("dynamic-threshold-last-frame 100"),
            ),
            ("w-smooth", None),
            ("w-smooth", Some("w-smooth 0")),
            ("w-smooth", Some("w-smooth 5:5")),
            ("w-max", Some("w-max 0")),
            ("thresholds", None),
            ("thresholds", Some("thresholds 0")),
            ("thresholds", Some("thresholds 0.8:0.9")),
            ("thresholds", Some("thresholds NaN")),
            ("subthresholds", None),
            ("subthresholds", Some("subthresholds inf")),
            ("string-representations", None),
            ("string-representations", Some("string-representations")),
            ("freq-filter-enabled", Some("freq-filter-enabled on")),
            ("freq-filter-audio-channel", None),
            (
                "freq-filter-audio-channel",
                Some("freq-filter-audio-channel bnr"),
            ),
            (
                "freq-filter-aggregation-func-inside",
                Some("freq-filter-aggregation-func-inside median"),
            ),
            ("freq-filter-aggregation-func-outside", None),
            (
                "freq-filter-aggregation-func-outside",
                Some("freq-filter-aggregation-func-outside mean"),
            ),
            (
                "freq-filter-reduce-features-first",
                Some("freq-filter-reduce-features-first false"),
            ),
            (
                "freq-filter-react-as-subhits",
                Some("freq-filter-react-as-subhits false"),
            ),
            ("freq-filter-frequencies-hz", None),
            (
                "freq-filter-frequencies-hz",
                Some("freq-filter-frequencies-hz 3501:8000"),
            ),
            (
                "freq-filter-frequencies-hz",
                Some("freq-filter-frequencies-hz 9.5"),
            ),
            (
                "freq-filter-frequencies-hz",
                Some("freq-filter-frequencies-hz 3501::486"),
            ),
            ("freq-filter-threshold", None),
            ("freq-filter-frame-size", Some("freq-filter-frame-size 0")),
            (
                "freq-filter-frame-shift",
                Some("freq-filter-frame-shift -177"),
            ),
            (
                "freq-filter-left-context",
                Some("freq-filter-left-context x"),
            ),
            ("freq-filter-buffer-size", Some("freq-filter-buffer-size 0")),
            (
                "freq-filter-preemph-coeff",
                Some("freq-filter-preemph-coeff 1.5"),
            ),
            (
                "freq-filter-inside-smooth",
                Some("freq-filter-inside-smooth -1"),
            ),
            (
                "freq-filter-outside-smooth",
                Some("freq-filter-outside-smooth 2.5"),
            ),
            (
                "freq-filter-lower-freq-ratio",
                Some("freq-filter-lower-freq-ratio low"),
            ),
            (
                "freq-filter-higher-freq-ratio",
                Some("freq-filter-higher-freq-ratio inf"),
            ),
            (
                "freq-filter-use-dyn-freq-smooth",
                Some("freq-filter-use-dyn-freq-smooth yes"),
            ),
        ];
        for (key, line) in cases {
            let e = parse_flags(&station_with(key, line)).expect_err(&format!("{key}: {line:?}"));
            assert!(e.starts_with(&format!("{key}: ")), "{key}: {line:?} -> {e}");
        }
    }
}
