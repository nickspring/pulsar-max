//! pulsar's Sendspin sink contract over a PCM stream: requests from the runtime thread never wait
//! for `write`, which holds `io`; whichever side gets `io` first applies them.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};

use log::{error, info};

const SLICE_MS: i64 = 20;
const REOPEN_MS: i64 = 1_000;
const STATS_MS: i64 = 10_000;
// Until the stream has timestamps; see docs/max.md, "Звук".
pub const MIXER_LATENCY_US: i64 = 132_000;
const RAMP_SEC: f32 = 0.08;

pub trait Stream: Send {
    fn write(&mut self, data: &[u8], frames: usize, timeout_ms: u32) -> Result<usize, String>;
    /// (presented frame, its time in `Device::now_ns`'s clock)
    fn timestamp(&mut self) -> Option<(i64, i64)>;
    fn frames_written(&mut self) -> i64;
    fn frames_read(&mut self) -> i64;
    fn xruns(&mut self) -> i32;
}

pub trait Device: Send + Sync {
    type Stream: Stream;
    fn open(&self, rate: u32, channels: u8) -> Result<Self::Stream, String>;
    fn now_ns(&self) -> i64;
    fn idle(&self, ms: u32);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub bytes: usize,
    pub played: u32,
    pub finish_in_us: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Req {
    Configure { rate: u32, channels: u8, bits: u8 },
    Release,
}

pub struct MusicOut<D: Device> {
    device: D,
    io: Mutex<Io<D::Stream>>,
    req: Mutex<Option<Req>>,
    pending: AtomicBool,
    target_gain: AtomicU32,
}

struct Io<S> {
    active: bool,
    rate: u32,
    channels: u8,
    bits: u8,
    stream: Option<S>,
    next_open_ms: i64,
    open_failed_logged: bool,
    // Reported with the next timed report, or the library's count of buffered frames drifts.
    discarded: u32,
    gain: f32,
    scratch: Vec<u8>,
    opened_ms: i64,
    timestamped: bool,
    presentation: Presentation,
    frames: u64,
    stats_ms: i64,
    last_finish_us: i64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<D: Device> MusicOut<D> {
    pub fn new(device: D) -> Self {
        Self {
            device,
            io: Mutex::new(Io {
                active: false,
                rate: 0,
                channels: 0,
                bits: 0,
                stream: None,
                next_open_ms: 0,
                open_failed_logged: false,
                discarded: 0,
                gain: 1.0,
                scratch: Vec::new(),
                opened_ms: 0,
                timestamped: false,
                presentation: Presentation::default(),
                frames: 0,
                stats_ms: 0,
                last_finish_us: 0,
            }),
            req: Mutex::new(None),
            pending: AtomicBool::new(false),
            target_gain: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    pub fn configure(&self, rate: u32, channels: u8, bits: u8) {
        self.request(Req::Configure {
            rate,
            channels,
            bits,
        });
    }

    pub fn release(&self) {
        self.request(Req::Release);
    }

    pub fn set_gain(&self, g: f32) {
        let g = if g.is_nan() { 0.0 } else { g.clamp(0.0, 1.0) };
        self.target_gain.store(g.to_bits(), SeqCst);
        if let Ok(mut io) = self.io.try_lock()
            && (!io.active || io.stream.is_none())
        {
            io.gain = g;
        }
    }

    pub fn write(&self, data: &[u8], timeout_ms: u32) -> Written {
        let w = self.write_once(data, timeout_ms);
        // A request that found `io` taken must not wait for a next write that may never come.
        while self.pending.load(SeqCst) {
            self.apply_pending(&mut lock(&self.io));
        }
        w
    }

    fn target_gain(&self) -> f32 {
        f32::from_bits(self.target_gain.load(SeqCst))
    }

    fn now_ms(&self) -> i64 {
        self.device.now_ns() / 1_000_000
    }

    fn request(&self, r: Req) {
        {
            let mut req = lock(&self.req);
            *req = Some(r);
            self.pending.store(true, SeqCst);
        }
        let io = match self.io.try_lock() {
            Ok(io) => Some(io),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut io) = io {
            self.apply_pending(&mut io);
        }
    }

    fn apply_pending(&self, io: &mut Io<D::Stream>) {
        let r = {
            let mut req = lock(&self.req);
            self.pending.store(false, SeqCst);
            req.take()
        };
        match r {
            None => {}
            Some(Req::Configure {
                rate,
                channels,
                bits,
            }) => io.configure(rate, channels, bits, self.target_gain()),
            Some(Req::Release) => io.release(),
        }
    }

    fn write_once(&self, data: &[u8], timeout_ms: u32) -> Written {
        let mut io = lock(&self.io);
        self.apply_pending(&mut io);
        let bpf = usize::from(io.channels) * usize::from(io.bits / 8);
        if !io.active || bpf == 0 {
            return Written {
                bytes: data.len(),
                ..Written::default()
            };
        }
        let frames = data.len() / bpf;
        let whole = frames * bpf;
        let now = self.now_ms();
        if io.stream.is_none() && !io.open(&self.device, now) {
            return self.drop_paced(io, frames, bpf, timeout_ms);
        }

        let target = self.target_gain();
        let mut scratch = std::mem::take(&mut io.scratch);
        let src: &[u8] = if io.gain == target && target >= 1.0 {
            &data[..whole]
        } else {
            scale_s16(&data[..whole], bpf, io.gain, target, io.rate, &mut scratch);
            &scratch
        };
        let deadline = now + i64::from(timeout_ms);
        let mut done = 0;
        let mut failed = None;
        while done < frames {
            let Some(stream) = io.stream.as_mut() else {
                break;
            };
            let slice = (deadline - self.now_ms()).clamp(0, SLICE_MS) as u32;
            match stream.write(&src[done * bpf..], frames - done, slice) {
                Ok(n) => done += n.min(frames - done),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
            if done < frames && self.pending.load(SeqCst) {
                io.scratch = scratch;
                self.apply_pending(&mut io);
                // A format change inside a stream keeps the library's count of buffered frames.
                if io.active {
                    io.discard(frames);
                }
                return Written {
                    bytes: data.len(),
                    ..Written::default()
                };
            }
            if self.now_ms() >= deadline {
                break;
            }
        }
        io.scratch = scratch;
        io.gain = ramp_end(io.gain, target, io.rate, done);
        io.frames += done as u64;
        if let Some(e) = failed {
            io.fail(&e, self.now_ms());
        }
        if io.stream.is_none() {
            if done == 0 {
                return self.drop_paced(io, frames, bpf, timeout_ms);
            }
            io.discard(done);
            return Written {
                bytes: done * bpf,
                ..Written::default()
            };
        }
        if done == 0 && io.discarded == 0 {
            return Written::default();
        }
        let now_ns = self.device.now_ns();
        let finish_in_us = io.finish_in_us(now_ns);
        io.maybe_log_stats(now_ns / 1_000_000);
        let played = u32::try_from(done)
            .unwrap_or(u32::MAX)
            .wrapping_add(io.discarded);
        io.discarded = 0;
        Written {
            bytes: done * bpf,
            played,
            finish_in_us,
        }
    }
}

impl<D: Device> MusicOut<D> {
    // At most one call's worth, or the library's playtime runs ahead of the clock.
    fn drop_paced(
        &self,
        mut io: MutexGuard<'_, Io<D::Stream>>,
        frames: usize,
        bpf: usize,
        timeout_ms: u32,
    ) -> Written {
        let rate = io.rate;
        let limit = (u64::from(rate) * u64::from(timeout_ms) / 1000).max(1);
        let taken = frames.min(usize::try_from(limit).unwrap_or(usize::MAX));
        io.discard(taken);
        drop(io);
        self.device.idle(pace_ms(taken, timeout_ms, rate));
        Written {
            bytes: taken * bpf,
            ..Written::default()
        }
    }
}

fn pace_ms(frames: usize, timeout_ms: u32, rate: u32) -> u32 {
    let ms = frames as u64 * 1000 / u64::from(rate.max(1));
    u32::try_from(ms).unwrap_or(u32::MAX).min(timeout_ms)
}

impl<S: Stream> Io<S> {
    fn configure(&mut self, rate: u32, channels: u8, bits: u8, target_gain: f32) {
        self.close();
        self.rate = rate;
        self.channels = channels;
        self.bits = bits;
        self.active = true;
        self.open_failed_logged = false;
        self.next_open_ms = 0;
        self.gain = target_gain;
    }

    fn release(&mut self) {
        self.active = false;
        self.discarded = 0;
        if self.stream.is_some() {
            let xruns = self.stream.as_mut().map_or(0, Stream::xruns);
            info!(
                "sendspin: stream summary: {:.1} s written to the output, xruns {xruns}",
                self.frames as f64 / f64::from(self.rate.max(1))
            );
        }
        self.close();
    }

    fn open<D: Device<Stream = S>>(&mut self, device: &D, now_ms: i64) -> bool {
        if now_ms < self.next_open_ms {
            return false;
        }
        let opened = if self.bits == 16 {
            device.open(self.rate, self.channels)
        } else {
            Err(format!("unsupported bit depth {}", self.bits))
        };
        match opened {
            Ok(s) => {
                info!(
                    "sendspin: output open {} Hz/{} ch/{} bit",
                    self.rate, self.channels, self.bits
                );
                self.stream = Some(s);
                self.opened_ms = now_ms;
                self.timestamped = false;
                self.presentation = Presentation::default();
                self.frames = 0;
                self.stats_ms = now_ms;
                true
            }
            Err(e) => {
                if !self.open_failed_logged {
                    error!(
                        "sendspin: output open failed for {} Hz/{} ch/{} bit: {e}",
                        self.rate, self.channels, self.bits
                    );
                }
                self.open_failed_logged = true;
                self.next_open_ms = now_ms + REOPEN_MS;
                false
            }
        }
    }

    fn close(&mut self) {
        if self.stream.take().is_some() {
            info!("sendspin: output closed");
        }
    }

    fn fail(&mut self, e: &str, now_ms: i64) {
        error!("sendspin: output error {e}, closing it");
        self.close();
        self.next_open_ms = now_ms + REOPEN_MS;
    }

    fn discard(&mut self, frames: usize) {
        self.discarded = self
            .discarded
            .wrapping_add(u32::try_from(frames).unwrap_or(u32::MAX));
    }

    fn finish_in_us(&mut self, now_ns: i64) -> i64 {
        let Some(s) = self.stream.as_mut() else {
            return 0;
        };
        let rate = self.rate;
        let (written, read) = (s.frames_written(), s.frames_read());
        let ts = s
            .timestamp()
            .filter(|&ts| self.presentation.accept(ts, read, now_ns, rate));
        let us = finish_in_us(written, read, ts, now_ns, rate);
        if ts.is_some() && !self.timestamped {
            self.timestamped = true;
            info!(
                "sendspin: output timestamps after {} ms, the last frame plays in {:.1} ms",
                now_ns / 1_000_000 - self.opened_ms,
                us as f64 / 1000.0
            );
        }
        self.last_finish_us = us;
        us
    }

    fn maybe_log_stats(&mut self, now_ms: i64) {
        if now_ms - self.stats_ms < STATS_MS {
            return;
        }
        self.stats_ms = now_ms;
        let xruns = self.stream.as_mut().map_or(0, Stream::xruns);
        info!(
            "sendspin: output {} Hz/{} ch: {:.1} s written, last frame plays in {:.1} ms, xruns {xruns}, gain {:.2}",
            self.rate,
            self.channels,
            self.frames as f64 / f64::from(self.rate.max(1)),
            self.last_finish_us as f64 / 1000.0,
            self.gain
        );
    }
}

/// The stream's first timestamps stand still, jump, or track the mixer instead of the speaker
/// (docs/max.md, "Звук"): they count once the position moves with the clock and lags the mixer.
#[derive(Default)]
struct Presentation {
    last: Option<(i64, i64)>,
    trusted: bool,
}

impl Presentation {
    fn accept(&mut self, ts: (i64, i64), read: i64, now_ns: i64, rate: u32) -> bool {
        if self.trusted {
            return true;
        }
        let Some((pos0, at0)) = self.last else {
            self.last = Some(ts);
            return false;
        };
        let (pos, at) = ts;
        if at <= at0 {
            return false;
        }
        self.last = Some(ts);
        let rate = i64::from(rate);
        let expected = (at - at0) * rate / 1_000_000_000;
        let moved = pos - pos0;
        let steady = moved > 0 && (moved - expected).abs() * 4 <= expected;
        let presented_now = pos + (now_ns - at) * rate / 1_000_000_000;
        let behind_mixer = read - presented_now >= rate * MIXER_LATENCY_US / 2 / 1_000_000;
        self.trusted = steady && behind_mixer;
        self.trusted
    }
}

/// µs after `now_ns` until the last written frame has played.
pub fn finish_in_us(
    written: i64,
    read: i64,
    timestamp: Option<(i64, i64)>,
    now_ns: i64,
    rate: u32,
) -> i64 {
    let rate = i64::from(rate.max(1));
    let us = match timestamp {
        Some((pos, at_ns)) => (written - pos) * 1_000_000 / rate - (now_ns - at_ns) / 1_000,
        None => (written - read) * 1_000_000 / rate + MIXER_LATENCY_US,
    };
    us.max(0)
}

fn ramp_step(rate: u32) -> f32 {
    1.0 / (RAMP_SEC * rate.max(1) as f32)
}

fn ramp_end(gain: f32, target: f32, rate: u32, frames: usize) -> f32 {
    let d = ramp_step(rate) * frames as f32;
    if gain < target {
        (gain + d).min(target)
    } else {
        (gain - d).max(target)
    }
}

fn scale_s16(data: &[u8], bpf: usize, gain: f32, target: f32, rate: u32, out: &mut Vec<u8>) {
    let step = ramp_step(rate);
    out.clear();
    out.reserve(data.len());
    let mut g = gain;
    for frame in data.chunks_exact(bpf) {
        g = if g < target {
            (g + step).min(target)
        } else {
            (g - step).max(target)
        };
        for s in frame.as_chunks::<2>().0 {
            let v = f32::from(i16::from_le_bytes(*s)) * g;
            out.extend_from_slice(&(v.round() as i16).to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicI64;

    use super::*;

    const RATE: u32 = 48_000;
    const BPF: usize = 4;
    type Hook = Box<dyn FnOnce() + Send>;

    #[derive(Default)]
    struct Shared {
        now_ns: AtomicI64,
        opens: Mutex<Vec<(u32, u8)>>,
        open_fails: AtomicBool,
        closed: AtomicU32,
        samples: Mutex<Vec<i16>>,
        /// Frames the next writes take before the device is full; negative: unlimited.
        room: AtomicI64,
        write_fails: AtomicBool,
        timestamp: Mutex<Option<(i64, i64)>>,
        read: AtomicI64,
        idled: Mutex<Vec<u32>>,
        on_full: Mutex<Option<Hook>>,
        on_write: Mutex<Option<Hook>>,
    }

    struct Fake(Arc<Shared>);

    struct FakeStream {
        sh: Arc<Shared>,
        written: i64,
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.sh.closed.fetch_add(1, SeqCst);
        }
    }

    impl Stream for FakeStream {
        fn write(&mut self, data: &[u8], frames: usize, timeout_ms: u32) -> Result<usize, String> {
            if let Some(hook) = self.sh.on_write.lock().unwrap().take() {
                hook();
            }
            if self.sh.write_fails.load(SeqCst) {
                return Err("disconnected".into());
            }
            let room = self.sh.room.load(SeqCst);
            let n = if room < 0 {
                frames
            } else {
                frames.min(room as usize)
            };
            if room >= 0 {
                self.sh.room.fetch_sub(n as i64, SeqCst);
            }
            self.sh.samples.lock().unwrap().extend(
                data[..n * BPF]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b)),
            );
            self.written += n as i64;
            if n < frames {
                if let Some(hook) = self.sh.on_full.lock().unwrap().take() {
                    hook();
                }
                self.sh
                    .now_ns
                    .fetch_add(i64::from(timeout_ms) * 1_000_000, SeqCst);
            }
            Ok(n)
        }

        fn timestamp(&mut self) -> Option<(i64, i64)> {
            *self.sh.timestamp.lock().unwrap()
        }

        fn frames_written(&mut self) -> i64 {
            self.written
        }

        fn frames_read(&mut self) -> i64 {
            self.sh.read.load(SeqCst)
        }

        fn xruns(&mut self) -> i32 {
            0
        }
    }

    impl Device for Fake {
        type Stream = FakeStream;

        fn open(&self, rate: u32, channels: u8) -> Result<FakeStream, String> {
            self.0.opens.lock().unwrap().push((rate, channels));
            if self.0.open_fails.load(SeqCst) {
                return Err("busy".into());
            }
            Ok(FakeStream {
                sh: self.0.clone(),
                written: 0,
            })
        }

        fn now_ns(&self) -> i64 {
            self.0.now_ns.load(SeqCst)
        }

        fn idle(&self, ms: u32) {
            self.0.idled.lock().unwrap().push(ms);
        }
    }

    fn out() -> (Arc<MusicOut<Fake>>, Arc<Shared>) {
        let sh = Arc::new(Shared::default());
        sh.room.store(-1, SeqCst);
        sh.now_ns.store(1_000_000_000, SeqCst);
        (Arc::new(MusicOut::new(Fake(sh.clone()))), sh)
    }

    fn pcm(frames: usize, v: i16) -> Vec<u8> {
        std::iter::repeat_n(v.to_le_bytes(), frames * 2)
            .flatten()
            .collect()
    }

    fn advance_ms(sh: &Shared, ms: i64) {
        sh.now_ns.fetch_add(ms * 1_000_000, SeqCst);
    }

    #[test]
    fn nothing_is_written_outside_a_stream() {
        let (o, sh) = out();
        let w = o.write(&pcm(480, 100), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        assert!(sh.opens.lock().unwrap().is_empty());
    }

    #[test]
    fn a_stream_opens_on_its_first_write_and_reports_what_it_took() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        let w = o.write(&pcm(480, 100), 20);
        assert_eq!(*sh.opens.lock().unwrap(), [(RATE, 2)]);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                played: 480,
                finish_in_us: 10_000 + MIXER_LATENCY_US
            }
        );
        assert!(sh.samples.lock().unwrap().iter().all(|&s| s == 100));
    }

    #[test]
    fn finish_time_comes_from_the_timestamp_once_there_is_one() {
        // A probe reading on the station: 6784 frames ahead of a position presented 1.46 ms ago.
        assert_eq!(
            finish_in_us(26_880, 26_624, Some((20_096, 0)), 1_460_000, RATE),
            139_873
        );
        assert_eq!(
            finish_in_us(480, 0, None, 0, RATE),
            10_000 + MIXER_LATENCY_US
        );
        assert_eq!(finish_in_us(0, 0, Some((480, 0)), 0, RATE), 0);
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        let now = sh.now_ns.load(SeqCst);
        *sh.timestamp.lock().unwrap() = Some((0, now - 10_000_000));
        sh.read.store(4_320, SeqCst);
        assert_eq!(
            o.write(&pcm(4_800, 0), 20).finish_in_us,
            10_000 + MIXER_LATENCY_US,
            "a position that has not moved yet is not a presentation time"
        );
        *sh.timestamp.lock().unwrap() = Some((480, now));
        assert_eq!(o.write(&pcm(4_800, 0), 20).finish_in_us, 190_000);
    }

    #[test]
    fn timestamps_count_once_the_position_moves_with_the_clock_behind_the_mixer() {
        let us = |n: i64| n * 1_000;
        let mut p = Presentation::default();
        // aaudio_check on the station: (position, its time, frames read, now), µs.
        assert!(!p.accept((1024, us(15_500)), 2048, us(26_000), RATE));
        assert!(!p.accept((1024, us(15_500)), 2048, us(27_000), RATE));
        assert!(!p.accept((1024, us(66_600)), 4096, us(68_000), RATE));
        assert!(!p.accept((1024, us(103_500)), 6144, us(107_000), RATE));
        assert!(!p.accept((1664, us(146_600)), 8192, us(149_000), RATE));
        assert!(p.accept((4096, us(198_850)), 10240, us(201_000), RATE));
        assert!(
            p.accept((4096, us(198_850)), 0, us(202_000), RATE),
            "trusted for the stream"
        );

        let mut mixer = Presentation::default();
        assert!(!mixer.accept((1024, us(20_000)), 2048, us(21_000), RATE));
        assert!(
            !mixer.accept((2048, us(41_300)), 3072, us(42_000), RATE),
            "moves with the clock but only as far as the mixer read"
        );

        let mut back = Presentation::default();
        assert!(!back.accept((4096, us(10_000)), 8192, us(11_000), RATE));
        assert!(!back.accept((2048, us(30_000)), 9216, us(31_000), RATE));
    }

    #[test]
    fn release_closes_the_stream_and_later_audio_is_stale() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        o.release();
        assert_eq!(sh.closed.load(SeqCst), 1);
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        assert_eq!(sh.opens.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_restarted_stream_starts_on_a_fresh_device() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        o.configure(44_100, 2, 16);
        assert_eq!(sh.closed.load(SeqCst), 1);
        o.write(&pcm(441, 1), 20);
        assert_eq!(*sh.opens.lock().unwrap(), [(RATE, 2), (44_100, 2)]);
    }

    #[test]
    fn a_request_during_a_write_waits_for_it_and_drops_its_audio() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        sh.room.store(100, SeqCst);
        let o2 = o.clone();
        *sh.on_full.lock().unwrap() = Some(Box::new(move || o2.release()));
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        assert_eq!(sh.closed.load(SeqCst), 1);
    }

    #[test]
    fn a_request_during_a_write_that_completes_is_applied_before_it_returns() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        let o2 = o.clone();
        *sh.on_write.lock().unwrap() = Some(Box::new(move || o2.release()));
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480);
        assert_eq!(
            sh.closed.load(SeqCst),
            1,
            "the release did not wait for a next write"
        );
    }

    #[test]
    fn a_format_change_mid_write_reports_the_dropped_chunk_later() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        sh.room.store(100, SeqCst);
        let o2 = o.clone();
        *sh.on_full.lock().unwrap() = Some(Box::new(move || o2.configure(RATE, 2, 16)));
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        sh.room.store(-1, SeqCst);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480 + 480);
    }

    #[test]
    fn frames_dropped_before_a_stream_end_are_not_reported_in_the_next_stream() {
        let (o, sh) = out();
        sh.open_fails.store(true, SeqCst);
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        o.release();
        sh.open_fails.store(false, SeqCst);
        advance_ms(&sh, REOPEN_MS);
        o.configure(RATE, 2, 16);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480);
    }

    #[test]
    fn a_device_that_fails_mid_write_keeps_what_it_took() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        sh.room.store(100, SeqCst);
        let sh2 = sh.clone();
        *sh.on_full.lock().unwrap() = Some(Box::new(move || sh2.write_fails.store(true, SeqCst)));
        let w = o.write(&pcm(480, 1), 100);
        assert_eq!(
            w,
            Written {
                bytes: 400,
                ..Written::default()
            }
        );
        assert_eq!(sh.closed.load(SeqCst), 1);
        sh.write_fails.store(false, SeqCst);
        sh.room.store(-1, SeqCst);
        advance_ms(&sh, REOPEN_MS);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480 + 100);
    }

    #[test]
    fn dropped_frames_are_reported_even_when_nothing_new_fits() {
        let (o, sh) = out();
        sh.open_fails.store(true, SeqCst);
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        sh.open_fails.store(false, SeqCst);
        sh.room.store(0, SeqCst);
        advance_ms(&sh, REOPEN_MS);
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!((w.bytes, w.played), (0, 480));
        assert!(w.finish_in_us > 0);
    }

    #[test]
    fn a_full_device_returns_what_it_took_by_the_deadline() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        sh.room.store(100, SeqCst);
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!((w.bytes, w.played), (400, 100));
        sh.room.store(0, SeqCst);
        assert_eq!(o.write(&pcm(480, 1), 20), Written::default());
    }

    #[test]
    fn audio_without_a_device_is_paced_and_reported_once_it_opens() {
        let (o, sh) = out();
        sh.open_fails.store(true, SeqCst);
        o.configure(RATE, 2, 16);
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        let w = o.write(&pcm(1_920, 1), 20);
        assert_eq!(w.bytes, 960 * BPF, "no more than one timeout's worth");
        assert_eq!(*sh.idled.lock().unwrap(), [10, 20]);
        assert_eq!(sh.opens.lock().unwrap().len(), 1, "retried within a second");
        sh.open_fails.store(false, SeqCst);
        advance_ms(&sh, REOPEN_MS);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480 + 960 + 480);
    }

    #[test]
    fn a_write_error_closes_the_device_until_it_reopens() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        sh.write_fails.store(true, SeqCst);
        let w = o.write(&pcm(480, 1), 20);
        assert_eq!(
            w,
            Written {
                bytes: 1920,
                ..Written::default()
            }
        );
        assert_eq!(sh.closed.load(SeqCst), 1);
        sh.write_fails.store(false, SeqCst);
        o.write(&pcm(480, 1), 20);
        assert_eq!(sh.opens.lock().unwrap().len(), 1);
        advance_ms(&sh, REOPEN_MS);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480 * 3);
    }

    #[test]
    fn only_16_bit_streams_open() {
        let (o, sh) = out();
        o.configure(RATE, 2, 24);
        let w = o.write(&[0; 600], 20);
        assert_eq!(
            w,
            Written {
                bytes: 600,
                ..Written::default()
            }
        );
        assert!(sh.opens.lock().unwrap().is_empty());
    }

    #[test]
    fn gain_set_while_idle_applies_at_once_and_ramps_while_playing() {
        let (o, sh) = out();
        o.set_gain(0.0);
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1000), 20);
        assert!(sh.samples.lock().unwrap().iter().all(|&s| s == 0));
        sh.samples.lock().unwrap().clear();
        o.set_gain(f32::NAN);
        o.set_gain(1.0);
        o.write(&pcm(3_840, 1000), 100);
        let s = sh.samples.lock().unwrap().clone();
        assert!(s[0] <= 1, "first frame {}", s[0]);
        assert!(
            (499..=501).contains(&s[1_920 * 2]),
            "half way {}",
            s[1_920 * 2]
        );
        assert!(s.windows(2).all(|w| w[0] <= w[1]), "monotonic ramp");
        assert_eq!(*s.last().unwrap(), 1000);
    }

    #[test]
    fn ramp_covers_full_scale_in_80_ms() {
        assert_eq!(ramp_end(0.0, 1.0, RATE, 3_840), 1.0);
        assert!(ramp_end(0.0, 1.0, RATE, 1_920) < 0.51);
        assert_eq!(ramp_end(1.0, 0.15, RATE, 48_000), 0.15);
        assert_eq!(ramp_end(0.5, 0.5, 0, 10), 0.5);
    }

    #[test]
    fn pacing_follows_the_audio_up_to_the_timeout() {
        assert_eq!(pace_ms(480, 20, RATE), 10);
        assert_eq!(pace_ms(48_000, 20, RATE), 20);
        assert_eq!(pace_ms(480, 20, 0), 20);
    }

    #[test]
    fn stats_and_summary_do_not_disturb_the_stream() {
        let (o, sh) = out();
        o.configure(RATE, 2, 16);
        o.write(&pcm(480, 1), 20);
        advance_ms(&sh, STATS_MS);
        assert_eq!(o.write(&pcm(480, 1), 20).played, 480);
        o.release();
        assert_eq!(sh.closed.load(SeqCst), 1);
    }
}
