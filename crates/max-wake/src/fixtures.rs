//! Test inputs rebuilt bit-exactly from the Python generators in probes/spotter/.

pub fn f32le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect()
}

/// `lcg()` of golden_signal.py.
pub fn lcg(seed: u32, n: usize) -> Vec<i32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 16) as i32 - 32768
        })
        .collect()
}

/// `signal()` of golden_command.py.
pub fn command_signal() -> Vec<i16> {
    const SAMPLES: usize = 32_000;
    const SEGMENT: usize = 3_200;
    const PERIODS: [usize; 10] = [40, 0, 22, 64, 0, 30, 90, 0, 26, 50];
    lcg(7, SAMPLES)
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let seg = i / SEGMENT;
            let period = PERIODS[seg];
            let tone = match period {
                0 => 0,
                p if (i / (p / 2)) % 2 == 1 => 3000,
                _ => -3000,
            };
            ((a >> (4 + seg % 3)) + tone) as i16
        })
        .collect()
}

/// Writes Kaldi nnet3 binary models as golden_command.py does.
pub mod nnet3 {
    fn tok(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(s.as_bytes());
        out.push(b' ');
    }

    fn int(out: &mut Vec<u8>, v: i32) {
        out.push(4);
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn vector(out: &mut Vec<u8>, key: &str, v: &[f32]) {
        tok(out, key);
        tok(out, "FV");
        int(out, v.len() as i32);
        v.iter()
            .for_each(|x| out.extend_from_slice(&x.to_le_bytes()));
    }

    pub fn component(name: &str, kind: &str, body: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut out = Vec::new();
        tok(&mut out, "<ComponentName>");
        tok(&mut out, name);
        tok(&mut out, kind);
        body(&mut out);
        tok(&mut out, &format!("</{}", &kind[1..]));
        out
    }

    fn q16(
        kind: &str,
        name: &str,
        scale: f32,
        rows: usize,
        w: &[i16],
        bias: &[f32],
        context: Option<i32>,
    ) -> Vec<u8> {
        component(name, kind, |out| {
            tok(out, "<S>");
            out.push(4);
            out.extend_from_slice(&scale.to_le_bytes());
            tok(out, "<LinearParams>");
            tok(out, "SIM");
            int(out, rows as i32);
            int(out, (w.len() / rows) as i32);
            w.iter()
                .for_each(|x| out.extend_from_slice(&x.to_le_bytes()));
            vector(out, "<BiasParams>", bias);
            if let Some(c) = context {
                tok(out, "<ContextParam>");
                int(out, c);
            }
        })
    }

    pub fn affine(name: &str, scale: f32, rows: usize, w: &[i16], bias: &[f32]) -> Vec<u8> {
        q16(
            "<Q16FixedAffineComponent>",
            name,
            scale,
            rows,
            w,
            bias,
            None,
        )
    }

    pub fn conv(name: &str, rows: usize, w: &[i16], bias: &[f32], context: i32) -> Vec<u8> {
        q16(
            "<Q16GroupConvCorouComponent>",
            name,
            1.0,
            rows,
            w,
            bias,
            Some(context),
        )
    }

    pub fn softmax_component(name: &str, dim: i32) -> Vec<u8> {
        component(name, "<SoftmaxComponent>", |out| {
            tok(out, "<Dim>");
            int(out, dim);
            vector(out, "<ValueAvg>", &[]);
            vector(out, "<DerivAvg>", &[]);
            tok(out, "<Count>");
            out.push(8);
            out.extend_from_slice(&0.0f64.to_le_bytes());
        })
    }

    pub fn model(config: &str, components: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"\0B".to_vec();
        tok(&mut out, "<Nnet3>");
        out.push(b'\n');
        out.extend_from_slice(config.trim().as_bytes());
        out.extend_from_slice(b"\n\n");
        tok(&mut out, "<NumComponents>");
        int(&mut out, components.len() as i32);
        components.iter().for_each(|c| out.extend_from_slice(c));
        tok(&mut out, "</Nnet3>");
        out.push(b'\n');
        out
    }
}
