use std::collections::{HashMap, HashSet};
use std::fmt::Display;

/// Kaldi nnet3 binary model of io.sdk's command spotter; component types it does not use are
/// rejected. Frame-causal: the conv history carries over between `forward` calls.
pub struct Nnet {
    input_dim: usize,
    output_dim: usize,
    trunk: Vec<Layer>,
    heads: Vec<Affine>,
    x: Vec<f32>,
    y: Vec<f32>,
}

enum Layer {
    Bias(Box<[f32]>),
    Scale(Box<[f32]>),
    Affine(Affine),
    Conv(Conv),
    HardSwish,
}

struct Affine {
    rows: usize,
    cols: usize,
    weights: Box<[f32]>,
    scale: f32,
    bias: Box<[f32]>,
}

struct Conv {
    channels: usize,
    taps: usize,
    // Tap-major; tap 0 is the oldest frame, no dilation (io.sdk's order).
    weights: Box<[f32]>,
    scale: f32,
    bias: Box<[f32]>,
    history: Box<[f32]>,
    next_slot: usize,
    acc: Box<[f32]>,
}

impl Nnet {
    pub fn read(data: &[u8]) -> Result<Self, String> {
        let (config, mut r) = header(data)?;
        let graph = Graph::parse(config)?;
        r.expect("<NumComponents>")?;
        let count = r.size()?;
        let mut components = HashMap::new();
        for _ in 0..count {
            let (name, component) = read_component(&mut r)?;
            if components.insert(name, component).is_some() {
                return Err(format!("nnet3: component {name} defined twice"));
            }
        }
        r.expect("</Nnet3>")?;
        if !r.rest().iter().all(u8::is_ascii_whitespace) {
            return Err(r.error("data after </Nnet3>"));
        }
        graph.build(components)
    }

    pub fn input_dim(&self) -> usize {
        self.input_dim
    }

    pub fn output_dim(&self) -> usize {
        self.output_dim
    }

    /// Appends `output_dim` posteriors per frame of `features`; batching does not change them.
    pub fn forward(&mut self, features: &[f32], out: &mut Vec<f32>) {
        assert_eq!(features.len() % self.input_dim, 0, "whole frames");
        let frames = features.len() / self.input_dim;
        let mut dim = self.input_dim;
        self.x.clear();
        self.x.extend_from_slice(features);
        for layer in &mut self.trunk {
            match layer {
                Layer::Bias(bias) => {
                    for frame in self.x.chunks_exact_mut(dim) {
                        frame.iter_mut().zip(bias.iter()).for_each(|(x, b)| *x += b);
                    }
                }
                Layer::Scale(scale) => {
                    for frame in self.x.chunks_exact_mut(dim) {
                        frame
                            .iter_mut()
                            .zip(scale.iter())
                            .for_each(|(x, s)| *x *= s);
                    }
                }
                Layer::HardSwish => self.x.iter_mut().for_each(|x| *x = hard_swish(*x)),
                Layer::Affine(affine) => {
                    affine.apply(&self.x, &mut self.y);
                    std::mem::swap(&mut self.x, &mut self.y);
                    dim = affine.rows;
                }
                Layer::Conv(conv) => {
                    conv.apply(&self.x, &mut self.y);
                    std::mem::swap(&mut self.x, &mut self.y);
                }
            }
        }
        let start = out.len();
        out.resize(start + frames * self.output_dim, 0.0);
        let mut column = 0;
        for head in &self.heads {
            head.apply(&self.x, &mut self.y);
            let rows = out[start..].chunks_exact_mut(self.output_dim);
            for (row, z) in rows.zip(self.y.chunks_exact(head.rows)) {
                let p = &mut row[column..column + head.rows];
                p.copy_from_slice(z);
                softmax(p);
            }
            column += head.rows;
        }
    }

    pub fn reset(&mut self) {
        for layer in &mut self.trunk {
            if let Layer::Conv(conv) = layer {
                conv.history.fill(0.0);
                conv.next_slot = 0;
            }
        }
    }
}

impl Affine {
    fn apply(&self, x: &[f32], y: &mut Vec<f32>) {
        let frames = x.len() / self.cols;
        y.clear();
        y.resize(frames * self.rows, 0.0);
        let rows = self.weights.chunks_exact(self.cols).zip(self.bias.iter());
        for (r, (w, &b)) in rows.enumerate() {
            for (f, x) in x.chunks_exact(self.cols).enumerate() {
                y[f * self.rows + r] = self.scale * dot(w, x) + b;
            }
        }
    }
}

impl Conv {
    fn apply(&mut self, x: &[f32], y: &mut Vec<f32>) {
        let c = self.channels;
        y.clear();
        for frame in x.chunks_exact(c) {
            self.history[self.next_slot * c..][..c].copy_from_slice(frame);
            self.next_slot = (self.next_slot + 1) % self.taps;
            self.acc.fill(0.0);
            for (tap, w) in self.weights.chunks_exact(c).enumerate() {
                let slot = (self.next_slot + tap) % self.taps;
                let h = &self.history[slot * c..][..c];
                for ((a, &w), &h) in self.acc.iter_mut().zip(w).zip(h) {
                    *a += w * h;
                }
            }
            let scale = self.scale;
            y.extend(
                self.acc
                    .iter()
                    .zip(&self.bias)
                    .map(|(&a, &b)| scale * a + b),
            );
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Separate lanes: a single f32 accumulator would block vectorisation.
    const LANES: usize = 16;
    let (a_lanes, a_tail) = a.as_chunks::<LANES>();
    let (b_lanes, b_tail) = b.as_chunks::<LANES>();
    let mut acc = [0.0f32; LANES];
    for (a, b) in a_lanes.iter().zip(b_lanes) {
        for ((acc, &a), &b) in acc.iter_mut().zip(a).zip(b) {
            *acc += a * b;
        }
    }
    let tail = a_tail
        .iter()
        .zip(b_tail)
        .fold(0.0f32, |s, (&a, &b)| s + a * b);
    acc.iter().sum::<f32>() + tail
}

fn hard_swish(x: f32) -> f32 {
    x * (x + 3.0).clamp(0.0, 6.0) / 6.0
}

fn softmax(z: &mut [f32]) {
    let max = z.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for v in z.iter_mut() {
        *v = (*v - max).exp();
        sum += *v;
    }
    z.iter_mut().for_each(|v| *v /= sum);
}

const BINARY_MARK: &[u8] = b"\0B";

fn header(data: &[u8]) -> Result<(&str, Reader<'_>), String> {
    let mut r = Reader { data, at: 0 };
    if !data.starts_with(BINARY_MARK) {
        return Err("nnet3: not a Kaldi binary file".into());
    }
    r.at = BINARY_MARK.len();
    r.expect("<Nnet3>")?;
    let rest = r.rest();
    let end = rest
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or_else(|| r.error("config lines are not terminated"))?;
    let config = std::str::from_utf8(&rest[..end]).map_err(|_| r.error("config is not UTF-8"))?;
    r.at += end + 2;
    Ok((config, r))
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn error(&self, what: impl Display) -> String {
        format!("nnet3: {what} at byte {}", self.at)
    }

    fn rest(&self) -> &'a [u8] {
        &self.data[self.at..]
    }

    fn token(&mut self) -> Result<&'a str, String> {
        while self.data.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
        let len = self
            .rest()
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| self.error("unterminated token"))?;
        let token = &self.rest()[..len];
        if token.is_empty() || !token.iter().all(u8::is_ascii_graphic) {
            return Err(self.error("bad token"));
        }
        self.at += len + 1;
        Ok(std::str::from_utf8(token).expect("ASCII"))
    }

    fn expect(&mut self, want: &str) -> Result<(), String> {
        let at = self.at;
        let got = self.token()?;
        if got == want {
            return Ok(());
        }
        self.at = at;
        Err(self.error(format_args!("expected {want}, got {got}")))
    }

    fn take(&mut self, len: Option<usize>) -> Result<&'a [u8], String> {
        let bytes = len
            .and_then(|len| self.rest().get(..len))
            .ok_or_else(|| self.error("unexpected end of data"))?;
        self.at += bytes.len();
        Ok(bytes)
    }

    fn basic<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let [size] = *self.take(Some(1))?.first_chunk().expect("one byte");
        if usize::from(size) != N {
            self.at -= 1;
            return Err(self.error(format_args!("expected a {N}-byte value")));
        }
        Ok(*self.take(Some(N))?.first_chunk().expect("N bytes"))
    }

    fn int(&mut self) -> Result<i32, String> {
        self.basic().map(i32::from_le_bytes)
    }

    fn size(&mut self) -> Result<usize, String> {
        let v = self.int()?;
        usize::try_from(v).map_err(|_| self.error(format_args!("negative size {v}")))
    }

    fn float(&mut self) -> Result<f32, String> {
        let v = self.basic().map(f32::from_le_bytes)?;
        if v.is_finite() {
            Ok(v)
        } else {
            Err(self.error("non-finite value"))
        }
    }

    fn float_vector(&mut self) -> Result<Vec<f32>, String> {
        self.expect("FV")?;
        let len = self.size()?;
        let bytes = self.take(len.checked_mul(4))?;
        let v: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        if v.iter().all(|x| x.is_finite()) {
            Ok(v)
        } else {
            Err(self.error("non-finite value in vector"))
        }
    }

    fn int16_matrix(&mut self) -> Result<Matrix, String> {
        self.expect("SIM")?;
        let rows = self.size()?;
        let cols = self.size()?;
        if rows == 0 || cols == 0 {
            return Err(self.error("empty matrix"));
        }
        let len = rows.checked_mul(cols).and_then(|n| n.checked_mul(2));
        let values = self
            .take(len)?
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| f32::from(i16::from_le_bytes(b)))
            .collect();
        Ok(Matrix { rows, cols, values })
    }
}

struct Matrix {
    rows: usize,
    cols: usize,
    values: Vec<f32>,
}

enum Value {
    Vector(Vec<f32>),
    Matrix(Matrix),
    Float(f32),
    Int(i32),
    Double,
}

enum Kind {
    Bias,
    Scale,
    Affine,
    Conv,
    HardSwish,
    Softmax,
}

enum Component {
    Bias(Vec<f32>),
    Scale(Vec<f32>),
    Affine(Affine),
    Conv(Conv),
    HardSwish,
    Softmax(usize),
}

fn read_component<'a>(r: &mut Reader<'a>) -> Result<(&'a str, Component), String> {
    r.expect("<ComponentName>")?;
    let name = r.token()?;
    let kind = r.token()?;
    let tag = match kind {
        "<FixedBiasComponent>" => Kind::Bias,
        "<FixedScaleComponent>" => Kind::Scale,
        "<Q16FixedAffineComponent>" => Kind::Affine,
        "<Q16GroupConvCorouComponent>" => Kind::Conv,
        "<hardswish>" => Kind::HardSwish,
        "<SoftmaxComponent>" => Kind::Softmax,
        _ => return Err(r.error(format_args!("{name}: unsupported component type {kind}"))),
    };
    let close = format!("</{}", &kind[1..]);
    let mut fields = Fields {
        name,
        kind,
        values: Vec::new(),
    };
    loop {
        let key = r.token()?;
        if key == close {
            break;
        }
        let value = match key {
            "<Bias>" | "<Scales>" | "<BiasParams>" | "<ValueAvg>" | "<DerivAvg>" => {
                Value::Vector(r.float_vector()?)
            }
            "<LinearParams>" => Value::Matrix(r.int16_matrix()?),
            "<S>" => Value::Float(r.float()?),
            "<ContextParam>" | "<Dim>" => Value::Int(r.int()?),
            "<Count>" => {
                r.basic::<8>()?;
                Value::Double
            }
            _ => return Err(r.error(format_args!("{name} {kind}: unsupported field {key}"))),
        };
        if fields.values.iter().any(|(k, _)| *k == key) {
            return Err(r.error(format_args!("{name}: {key} repeated")));
        }
        fields.values.push((key, value));
    }
    let component = match tag {
        Kind::Bias => Component::Bias(fields.vector("<Bias>")?),
        Kind::Scale => Component::Scale(fields.vector("<Scales>")?),
        Kind::Affine => {
            let (m, scale, bias) = fields.q16()?;
            Component::Affine(Affine {
                rows: m.rows,
                cols: m.cols,
                weights: m.values.into(),
                scale,
                bias: bias.into(),
            })
        }
        Kind::Conv => {
            let (m, scale, bias) = fields.q16()?;
            // io.sdk's per-layer lookahead: shifts outputs in time, not their values.
            if let Some(context) = fields.int("<ContextParam>")
                && context < 0
            {
                return Err(fields.error(format_args!("<ContextParam> {context}")));
            }
            let mut weights = vec![0.0; m.values.len()];
            for (ch, row) in m.values.chunks_exact(m.cols).enumerate() {
                for (tap, &w) in row.iter().enumerate() {
                    weights[tap * m.rows + ch] = w;
                }
            }
            Component::Conv(Conv {
                channels: m.rows,
                taps: m.cols,
                weights: weights.into(),
                scale,
                bias: bias.into(),
                history: vec![0.0; m.values.len()].into(),
                next_slot: 0,
                acc: vec![0.0; m.rows].into(),
            })
        }
        Kind::HardSwish => Component::HardSwish,
        Kind::Softmax => {
            fields.take("<ValueAvg>");
            fields.take("<DerivAvg>");
            fields.take("<Count>");
            let dim = fields.int("<Dim>").ok_or_else(|| fields.missing("<Dim>"))?;
            Component::Softmax(
                usize::try_from(dim)
                    .ok()
                    .filter(|&d| d > 0)
                    .ok_or_else(|| fields.error(format_args!("<Dim> {dim}")))?,
            )
        }
    };
    fields.finish()?;
    Ok((name, component))
}

struct Fields<'a> {
    name: &'a str,
    kind: &'a str,
    values: Vec<(&'a str, Value)>,
}

impl Fields<'_> {
    fn error(&self, what: impl Display) -> String {
        format!("nnet3: {} {}: {what}", self.name, self.kind)
    }

    fn missing(&self, key: &str) -> String {
        self.error(format_args!("{key} missing"))
    }

    fn take(&mut self, key: &str) -> Option<Value> {
        let i = self.values.iter().position(|(k, _)| *k == key)?;
        Some(self.values.remove(i).1)
    }

    fn vector(&mut self, key: &str) -> Result<Vec<f32>, String> {
        match self.take(key) {
            Some(Value::Vector(v)) => Ok(v),
            _ => Err(self.missing(key)),
        }
    }

    fn int(&mut self, key: &str) -> Option<i32> {
        match self.take(key)? {
            Value::Int(v) => Some(v),
            _ => None,
        }
    }

    fn q16(&mut self) -> Result<(Matrix, f32, Vec<f32>), String> {
        let Some(Value::Float(scale)) = self.take("<S>") else {
            return Err(self.missing("<S>"));
        };
        let Some(Value::Matrix(m)) = self.take("<LinearParams>") else {
            return Err(self.missing("<LinearParams>"));
        };
        let bias = self.vector("<BiasParams>")?;
        if bias.len() != m.rows {
            return Err(self.error(format_args!("{} biases for {} rows", bias.len(), m.rows)));
        }
        Ok((m, scale, bias))
    }

    fn finish(self) -> Result<(), String> {
        match self.values.first() {
            Some((key, _)) => Err(self.error(format_args!("unexpected field {key}"))),
            None => Ok(()),
        }
    }
}

struct Node<'a> {
    name: &'a str,
    component: &'a str,
    input: &'a str,
}

struct Graph<'a> {
    input: &'a str,
    input_dim: usize,
    nodes: Vec<Node<'a>>,
    outputs: Vec<&'a str>,
}

impl<'a> Graph<'a> {
    fn parse(config: &'a str) -> Result<Self, String> {
        let mut input = None;
        let mut outputs = None;
        let mut nodes = Vec::new();
        for line in config.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let bad = |what: &str| format!("nnet3: config line '{line}': {what}");
            let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
            let mut pairs = config_pairs(rest).ok_or_else(|| bad("bad syntax"))?;
            let mut get = |key: &str| {
                let i = pairs.iter().position(|(k, _)| *k == key);
                i.map(|i| pairs.remove(i).1)
                    .ok_or_else(|| bad(&format!("{key} missing")))
            };
            match kind {
                "input-node" => {
                    let name = get("name")?;
                    let dim = get("dim")?.parse().ok().filter(|&d| d > 0);
                    let dim = dim.ok_or_else(|| bad("bad dim"))?;
                    if input.replace((name, dim)).is_some() {
                        return Err(bad("second input-node"));
                    }
                }
                "component-node" => {
                    let node = Node {
                        name: get("name")?,
                        component: get("component")?,
                        input: get("input")?,
                    };
                    if !is_name(node.input) {
                        return Err(bad("unsupported input descriptor"));
                    }
                    nodes.push(node);
                }
                "output-node" => {
                    if get("name")? != "output" {
                        return Err(bad("only the output node 'output' is supported"));
                    }
                    let _ = get("objective");
                    let heads = output_descriptor(get("input")?)
                        .ok_or_else(|| bad("unsupported input descriptor"))?;
                    if outputs.replace(heads).is_some() {
                        return Err(bad("second output-node"));
                    }
                }
                _ => return Err(bad("unsupported node type")),
            }
            if let Some((key, _)) = pairs.first() {
                return Err(bad(&format!("unsupported key {key}")));
            }
        }
        let (input, input_dim) = input.ok_or("nnet3: no input-node")?;
        let outputs = outputs.ok_or("nnet3: no output-node")?;
        Ok(Self {
            input,
            input_dim,
            nodes,
            outputs,
        })
    }

    fn node(&self, name: &str) -> Result<&Node<'a>, String> {
        self.nodes
            .iter()
            .find(|n| n.name == name)
            .ok_or_else(|| format!("nnet3: no node {name}"))
    }

    fn build(self, mut components: HashMap<&str, Component>) -> Result<Nnet, String> {
        let mut seen = HashSet::new();
        for node in &self.nodes {
            if node.name == self.input || !seen.insert(node.name) {
                return Err(format!("nnet3: node {} defined twice", node.name));
            }
        }
        let mut take = |node: &Node| {
            components.remove(node.component).ok_or_else(|| {
                format!(
                    "nnet3: {}: no unused component {}",
                    node.name, node.component
                )
            })
        };

        let mut head_nodes = Vec::new();
        let mut branch = None;
        for &output in &self.outputs {
            let softmax = self.node(output)?;
            let affine = self.node(softmax.input)?;
            if branch
                .replace(affine.input)
                .is_some_and(|b| b != affine.input)
            {
                return Err("nnet3: the heads do not share one input".into());
            }
            head_nodes.push((affine, softmax));
        }
        let branch = branch.ok_or("nnet3: no outputs")?;

        let mut chain = Vec::new();
        let mut at = branch;
        while at != self.input {
            if chain.len() == self.nodes.len() {
                return Err("nnet3: the graph has a cycle".into());
            }
            let node = self.node(at)?;
            chain.push(node);
            at = node.input;
        }
        chain.reverse();
        if chain.len() + 2 * head_nodes.len() != self.nodes.len() {
            return Err("nnet3: nodes outside the chain and the heads".into());
        }

        let mut dim = self.input_dim;
        let mut trunk = Vec::new();
        for node in chain {
            let mismatch = |what: &str, got: usize| {
                format!("nnet3: {}: {what} {got}, input dim {dim}", node.name)
            };
            trunk.push(match take(node)? {
                Component::Bias(v) if v.len() == dim => Layer::Bias(v.into()),
                Component::Scale(v) if v.len() == dim => Layer::Scale(v.into()),
                Component::Affine(a) if a.cols == dim => {
                    dim = a.rows;
                    Layer::Affine(a)
                }
                Component::Conv(c) if c.channels == dim => Layer::Conv(c),
                Component::HardSwish => Layer::HardSwish,
                Component::Bias(v) | Component::Scale(v) => return Err(mismatch("size", v.len())),
                Component::Affine(a) => return Err(mismatch("columns", a.cols)),
                Component::Conv(c) => return Err(mismatch("channels", c.channels)),
                Component::Softmax(_) => {
                    return Err(format!("nnet3: {}: softmax before the heads", node.name));
                }
            });
        }

        let mut heads = Vec::new();
        for (affine, softmax) in head_nodes {
            let (Component::Affine(a), Component::Softmax(sm_dim)) =
                (take(affine)?, take(softmax)?)
            else {
                return Err(format!(
                    "nnet3: head {} is not affine + softmax",
                    softmax.name
                ));
            };
            if a.cols != dim || a.rows != sm_dim {
                return Err(format!(
                    "nnet3: head {}: dimensions do not match",
                    softmax.name
                ));
            }
            heads.push(a);
        }
        if let Some(name) = components.keys().next() {
            return Err(format!("nnet3: component {name} is not used"));
        }
        Ok(Nnet {
            input_dim: self.input_dim,
            output_dim: heads.iter().map(|h| h.rows).sum(),
            trunk,
            heads,
            x: Vec::new(),
            y: Vec::new(),
        })
    }
}

fn config_pairs(s: &str) -> Option<Vec<(&str, &str)>> {
    let mut pairs = Vec::new();
    let mut s = s.trim_start();
    while !s.is_empty() {
        let mut depth = 0i32;
        let end = s
            .char_indices()
            .find(|&(_, c)| {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                depth == 0 && c.is_whitespace()
            })
            .map_or(s.len(), |(i, _)| i);
        let (key, value) = s[..end].split_once('=')?;
        if key.is_empty() || value.is_empty() || depth != 0 {
            return None;
        }
        pairs.push((key, value));
        s = s[end..].trim_start();
    }
    Some(pairs)
}

fn output_descriptor(s: &str) -> Option<Vec<&str>> {
    if is_name(s) {
        return Some(vec![s]);
    }
    let inner = s.strip_prefix("Append(")?.strip_suffix(')')?;
    let names: Vec<&str> = inner.split(',').map(str::trim).collect();
    names.iter().all(|n| is_name(n)).then_some(names)
}

fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::f32le;
    use crate::fixtures::nnet3::{affine, component, conv, model, softmax_component};

    const TINY: &[u8] = include_bytes!("../tests/fixtures/command_tiny.nnet");
    const OUTPUTS: usize = 8;

    fn golden() -> (Vec<f32>, Vec<f32>) {
        (
            f32le(include_bytes!("../tests/fixtures/command_fbank.f32le")),
            f32le(include_bytes!("../tests/fixtures/command_post.f32le")),
        )
    }

    fn run(net: &mut Nnet, features: &[f32], batch_frames: usize) -> Vec<f32> {
        let mut out = Vec::new();
        for batch in features.chunks(batch_frames * net.input_dim()) {
            net.forward(batch, &mut out);
        }
        out
    }

    #[test]
    fn matches_the_reference_forward_pass() {
        let mut net = Nnet::read(TINY).unwrap();
        assert_eq!((net.input_dim(), net.output_dim()), (40, OUTPUTS));
        let (features, want) = golden();
        let got = run(&mut net, &features, 6);
        assert_eq!(got.len(), want.len());
        let worst = got
            .iter()
            .zip(&want)
            .map(|(g, w)| (g - w).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 2e-6, "max abs difference {worst}");
        for frame in got.as_chunks::<OUTPUTS>().0 {
            let (a, b) = frame.split_at(5);
            assert!((a.iter().sum::<f32>() - 1.0).abs() < 1e-6);
            assert!((b.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn batching_does_not_change_outputs() {
        let (features, _) = golden();
        let whole = run(&mut Nnet::read(TINY).unwrap(), &features, usize::MAX / 64);
        for batch in [1, 4, 7] {
            assert_eq!(run(&mut Nnet::read(TINY).unwrap(), &features, batch), whole);
        }
        let mut net = Nnet::read(TINY).unwrap();
        net.forward(&[], &mut Vec::new());
        assert_eq!(run(&mut net, &features, 3), whole);
    }

    #[test]
    fn reset_clears_the_conv_history() {
        let (features, _) = golden();
        let mut net = Nnet::read(TINY).unwrap();
        let first = run(&mut net, &features[..40 * 50], 6);
        assert_ne!(run(&mut net, &features[..40 * 50], 6), first);
        net.reset();
        assert_eq!(run(&mut net, &features[..40 * 50], 6), first);
    }

    #[test]
    #[should_panic(expected = "whole frames")]
    fn forward_takes_whole_frames() {
        Nnet::read(TINY)
            .unwrap()
            .forward(&[0.0; 41], &mut Vec::new());
    }

    const DELAY_CONFIG: &str = "
input-node name=input dim=2
component-node name=conv component=conv input=input
component-node name=head component=head input=conv
component-node name=sm component=sm input=head
output-node name=output input=sm objective=linear";

    /// Channel 0 keeps only the oldest tap, channel 1 only the newest; the head passes both on.
    fn delay_components() -> Vec<Vec<u8>> {
        vec![
            conv("conv", 2, &[1, 0, 0, 0, 0, 1], &[0.0; 2], 2),
            affine("head", 1.0, 2, &[1, 0, 0, 1], &[0.0; 2]),
            softmax_component("sm", 2),
        ]
    }

    fn softmax2(a: f32, b: f32) -> [f32; 2] {
        let mut z = [a, b];
        softmax(&mut z);
        z
    }

    #[test]
    fn conv_taps_run_oldest_first_without_dilation() {
        let mut net = Nnet::read(&model(DELAY_CONFIG, &delay_components())).unwrap();
        let mut out = Vec::new();
        let impulse = [4.0, 4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        net.forward(&impulse, &mut out);
        let want = [
            softmax2(0.0, 4.0),
            softmax2(0.0, 0.0),
            softmax2(4.0, 0.0),
            softmax2(0.0, 0.0),
        ];
        assert_eq!(out, want.concat());
    }

    #[test]
    fn rejects_every_truncation() {
        let (newline, rest) = TINY.split_last().unwrap();
        assert_eq!(*newline, b'\n');
        assert!(Nnet::read(rest).is_ok());
        for len in 0..rest.len() {
            assert!(Nnet::read(&TINY[..len]).is_err(), "prefix of {len} bytes");
        }
    }

    fn replaced(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
        let at = data
            .windows(from.len())
            .position(|w| w == from)
            .expect("pattern");
        [&data[..at], to, &data[at + from.len()..]].concat()
    }

    fn rejected(data: &[u8], want: &str) {
        match Nnet::read(data) {
            Ok(_) => panic!("accepted, want '{want}'"),
            Err(e) => assert!(e.contains(want), "'{e}' does not mention '{want}'"),
        }
    }

    #[test]
    fn rejects_what_it_cannot_run() {
        let good = model(DELAY_CONFIG, &delay_components());
        assert!(Nnet::read(&good).is_ok());
        let mut extra = good.clone();
        extra.extend_from_slice(b"x");
        let mut nan_scale = delay_components();
        nan_scale[0] = replaced(
            &nan_scale[0],
            &1.0f32.to_le_bytes(),
            &f32::NAN.to_le_bytes(),
        );
        let context = |v: i32| [&[b'>', b' ', 4][..], &v.to_le_bytes()].concat();
        let cases = [
            (good[1..].to_vec(), "not a Kaldi binary"),
            (extra, "data after </Nnet3>"),
            (
                replaced(
                    &good,
                    b"<SoftmaxComponent> <Dim>",
                    b"<SigmoidComponent> <Dim>",
                ),
                "unsupported component type <SigmoidComponent>",
            ),
            (
                replaced(&good, b"<Count>", b"<Other>"),
                "unsupported field <Other>",
            ),
            (
                replaced(&good, &context(2), &context(-2)),
                "<ContextParam> -2",
            ),
            (model(DELAY_CONFIG, &nan_scale), "non-finite value"),
            (
                replaced(&good, b"input=input\n", b"input=Offset(input, -1)\n"),
                "unsupported input descriptor",
            ),
            (
                replaced(&good, b"input-node", b"dim-range-node"),
                "unsupported node type",
            ),
            (
                replaced(&good, b"output-node name=output", b"output-node name=out"),
                "only the output node 'output'",
            ),
            (
                replaced(&good, b"objective=linear", b"objective=linear extra=1"),
                "unsupported key extra",
            ),
            (
                replaced(&good, b"dim=2", b"dim=3"),
                "conv: channels 2, input dim 3",
            ),
            (
                replaced(&good, b"component=head", b"component=nope"),
                "no unused component nope",
            ),
        ];
        for (data, want) in cases {
            rejected(&data, want);
        }
    }

    #[test]
    fn rejects_graphs_other_than_a_chain_with_heads() {
        let [conv, head, sm] = delay_components().try_into().unwrap();
        let identity = |name, rows, w: &[i16]| affine(name, 1.0, rows, w, &vec![0.0; rows]);
        let hardswish = component("x", "<hardswish>", |_| {});
        let cases = [
            (
                DELAY_CONFIG,
                vec![
                    conv.clone(),
                    head.clone(),
                    sm.clone(),
                    identity("spare", 2, &[1, 0, 0, 1]),
                ],
                "component spare is not used",
            ),
            (
                DELAY_CONFIG,
                vec![
                    conv.clone(),
                    identity("head", 3, &[1, 0, 0, 1, 1, 1]),
                    sm.clone(),
                ],
                "head sm: dimensions do not match",
            ),
            (
                "input-node name=input dim=2
                component-node name=sm0 component=sm0 input=input
                component-node name=head component=head input=sm0
                component-node name=sm component=sm input=head
                output-node name=output input=sm",
                vec![softmax_component("sm0", 2), head.clone(), sm.clone()],
                "softmax before the heads",
            ),
            (
                "input-node name=input dim=2
                component-node name=conv component=conv input=input
                component-node name=head component=head input=conv
                component-node name=sm component=sm input=head
                component-node name=x component=x input=input
                output-node name=output input=sm",
                vec![conv.clone(), head.clone(), sm.clone(), hardswish.clone()],
                "nodes outside the chain and the heads",
            ),
            (
                "input-node name=input dim=2
                component-node name=conv component=conv input=x
                component-node name=x component=x input=conv
                component-node name=head component=head input=conv
                component-node name=sm component=sm input=head
                output-node name=output input=sm",
                vec![conv.clone(), head.clone(), sm.clone(), hardswish],
                "cycle",
            ),
            (
                "input-node name=input dim=2
                component-node name=conv component=conv input=input
                component-node name=head component=head input=conv
                component-node name=sm component=sm input=head
                component-node name=head2 component=head2 input=input
                component-node name=sm2 component=sm2 input=head2
                output-node name=output input=Append(sm, sm2)",
                vec![
                    conv,
                    head,
                    sm,
                    identity("head2", 2, &[1, 0, 0, 1]),
                    softmax_component("sm2", 2),
                ],
                "do not share one input",
            ),
        ];
        for (config, components, want) in cases {
            rejected(&model(config, &components), want);
        }
    }

    #[test]
    fn rejects_malformed_components() {
        let good = model(DELAY_CONFIG, &delay_components());
        let [conv, head, sm] = delay_components().try_into().unwrap();
        let with_head = |head: Vec<u8>| model(DELAY_CONFIG, &[conv.clone(), head, sm.clone()]);
        let fields = |kind: &str, body: &[&[u8]]| {
            with_head(component("head", kind, |out| {
                body.iter().for_each(|b| out.extend_from_slice(b))
            }))
        };
        let s: &[u8] = &[b'<', b'S', b'>', b' ', 4, 0, 0, 0x80, 0x3f];
        let sim: &[u8] = &[
            b'<', b'L', b'i', b'n', b'e', b'a', b'r', b'P', b'a', b'r', b'a', b'm', b's', b'>',
            b' ', b'S', b'I', b'M', b' ', 4, 1, 0, 0, 0, 4, 1, 0, 0, 0, 1, 0,
        ];
        let cases = [
            (
                model(
                    DELAY_CONFIG,
                    &[conv.clone(), head.clone(), sm.clone(), sm.clone()],
                ),
                "component sm defined twice",
            ),
            (replaced(&good, b"<Count> ", b"<Co\x01nt> "), "bad token"),
            (
                replaced(&good, b"<Dim> \x04", b"<Dim> \x08"),
                "expected a 4-byte value",
            ),
            (
                replaced(
                    &good,
                    b"<ValueAvg> FV \x04\0\0\0\0",
                    b"<ValueAvg> FV \x04\xff\xff\xff\xff",
                ),
                "negative size -1",
            ),
            (
                with_head(affine("head", 1.0, 2, &[1, 0, 0, 1], &[0.0, f32::INFINITY])),
                "non-finite value in vector",
            ),
            (
                with_head(affine("head", 1.0, 2, &[], &[0.0; 2])),
                "empty matrix",
            ),
            (
                model(
                    DELAY_CONFIG,
                    &[
                        conv.clone(),
                        head.clone(),
                        replaced(&sm, b"<ValueAvg>", b"<Dim> \x04\x02\0\0\0<ValueAvg>"),
                    ],
                ),
                "<Dim> repeated",
            ),
            (fields("<Q16FixedAffineComponent>", &[sim]), "<S> missing"),
            (
                fields("<Q16FixedAffineComponent>", &[s]),
                "<LinearParams> missing",
            ),
            (
                fields("<Q16FixedAffineComponent>", &[s, sim]),
                "<BiasParams> missing",
            ),
            (fields("<FixedBiasComponent>", &[]), "<Bias> missing"),
            (fields("<SoftmaxComponent>", &[]), "<Dim> missing"),
            (
                with_head(affine("head", 1.0, 2, &[1, 0, 0, 1], &[0.0; 3])),
                "3 biases for 2 rows",
            ),
            (
                replaced(
                    &good,
                    b"</Q16FixedAffineComponent>",
                    b"<ContextParam> \x04\0\0\0\0</Q16FixedAffineComponent>",
                ),
                "unexpected field <ContextParam>",
            ),
        ];
        for (data, want) in cases {
            rejected(&data, want);
        }
    }

    #[test]
    fn rejects_inconsistent_configs() {
        let [conv, head, sm] = delay_components().try_into().unwrap();
        let bias = component("conv", "<FixedBiasComponent>", |out| {
            out.extend_from_slice(b"<Bias> FV \x04\x03\0\0\0");
            out.extend_from_slice(&[0; 12]);
        });
        let cases = [
            (
                format!("{DELAY_CONFIG}\ninput-node name=x dim=2"),
                vec![conv.clone(), head.clone(), sm.clone()],
                "second input-node",
            ),
            (
                format!("{DELAY_CONFIG}\noutput-node name=output input=sm"),
                vec![conv.clone(), head.clone(), sm.clone()],
                "second output-node",
            ),
            (
                DELAY_CONFIG.replace("dim=2", "dim=0"),
                vec![conv.clone(), head.clone(), sm.clone()],
                "bad dim",
            ),
            (
                format!("{DELAY_CONFIG}\ncomponent-node name=sm component=sm input=head"),
                vec![conv.clone(), head.clone(), sm.clone()],
                "node sm defined twice",
            ),
            (
                DELAY_CONFIG.into(),
                vec![bias, head.clone(), sm.clone()],
                "conv: size 3, input dim 2",
            ),
            (
                "input-node name=input dim=2
                component-node name=conv component=conv input=input
                component-node name=sm component=sm input=conv
                output-node name=output input=sm"
                    .into(),
                vec![conv, sm],
                "head sm is not affine + softmax",
            ),
        ];
        for (config, components, want) in cases {
            rejected(&model(&config, &components), want);
        }
    }

    #[test]
    fn config_pairs_keep_parentheses_together() {
        assert_eq!(
            config_pairs(" name=output input=Append(a, b) objective=linear"),
            Some(vec![
                ("name", "output"),
                ("input", "Append(a, b)"),
                ("objective", "linear")
            ])
        );
        assert_eq!(config_pairs("input=Append(a, b"), None);
        assert_eq!(config_pairs("name"), None);
        assert_eq!(output_descriptor("Append(a, b_1)"), Some(vec!["a", "b_1"]));
        assert_eq!(output_descriptor("Sum(a, b)"), None);
    }
}
