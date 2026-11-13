//! See docs/max.md, "Звук".

use std::ffi::{CStr, c_char, c_void};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

#[repr(C)]
struct AAudioStreamBuilder {
    _opaque: [u8; 0],
}

#[repr(C)]
struct AAudioStream {
    _opaque: [u8; 0],
}

const AAUDIO_FORMAT_PCM_I16: i32 = 1;
const AAUDIO_USAGE_MEDIA: i32 = 1;
const AAUDIO_CONTENT_TYPE_MUSIC: i32 = 2;
const AAUDIO_PERFORMANCE_MODE_NONE: i32 = 10;
const POLL: Duration = Duration::from_millis(4);

#[link(name = "aaudio")]
unsafe extern "C" {
    fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> i32;
    fn AAudioStreamBuilder_setFormat(builder: *mut AAudioStreamBuilder, format: i32);
    fn AAudioStreamBuilder_setChannelCount(builder: *mut AAudioStreamBuilder, count: i32);
    fn AAudioStreamBuilder_setSampleRate(builder: *mut AAudioStreamBuilder, rate: i32);
    fn AAudioStreamBuilder_setUsage(builder: *mut AAudioStreamBuilder, usage: i32);
    fn AAudioStreamBuilder_setContentType(builder: *mut AAudioStreamBuilder, content: i32);
    fn AAudioStreamBuilder_setPerformanceMode(builder: *mut AAudioStreamBuilder, mode: i32);
    fn AAudioStreamBuilder_openStream(
        builder: *mut AAudioStreamBuilder,
        stream: *mut *mut AAudioStream,
    ) -> i32;
    fn AAudioStreamBuilder_delete(builder: *mut AAudioStreamBuilder) -> i32;
    fn AAudioStream_requestStart(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_close(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_write(
        stream: *mut AAudioStream,
        buffer: *const c_void,
        frames: i32,
        timeout_ns: i64,
    ) -> i32;
    fn AAudioStream_getTimestamp(
        stream: *mut AAudioStream,
        clock: libc::clockid_t,
        position: *mut i64,
        time_ns: *mut i64,
    ) -> i32;
    fn AAudioStream_getFramesWritten(stream: *mut AAudioStream) -> i64;
    fn AAudioStream_getFramesRead(stream: *mut AAudioStream) -> i64;
    fn AAudioStream_getXRunCount(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_getSampleRate(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_getChannelCount(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_getFramesPerBurst(stream: *mut AAudioStream) -> i32;
    fn AAudioStream_getBufferSizeInFrames(stream: *mut AAudioStream) -> i32;
    fn AAudio_convertResultToText(result: i32) -> *const c_char;
}

fn error_text(r: i32) -> String {
    // SAFETY: AAudio returns null or a static NUL-terminated string for any value.
    let text = unsafe {
        let p = AAudio_convertResultToText(r);
        (!p.is_null()).then(|| CStr::from_ptr(p))
    };
    text.map_or_else(
        || format!("AAudio error {r}"),
        |t| t.to_string_lossy().into_owned(),
    )
}

fn check(r: i32, what: &str) -> Result<(), String> {
    if r < 0 {
        Err(format!("{what}: {}", error_text(r)))
    } else {
        Ok(())
    }
}

pub struct Stream {
    ptr: NonNull<AAudioStream>,
    channels: usize,
}

// SAFETY: an AAudio stream may be used from any thread; `&mut self` serializes our calls.
unsafe impl Send for Stream {}

impl Stream {
    pub fn open_output(rate: u32, channels: u8) -> Result<Self, String> {
        // SAFETY: the builder is used only after its creation succeeded and deleted once.
        let opened = unsafe {
            let mut b = std::ptr::null_mut();
            check(AAudio_createStreamBuilder(&mut b), "builder")?;
            AAudioStreamBuilder_setFormat(b, AAUDIO_FORMAT_PCM_I16);
            AAudioStreamBuilder_setChannelCount(b, i32::from(channels));
            AAudioStreamBuilder_setSampleRate(b, i32::try_from(rate).unwrap_or(i32::MAX));
            AAudioStreamBuilder_setUsage(b, AAUDIO_USAGE_MEDIA);
            AAudioStreamBuilder_setContentType(b, AAUDIO_CONTENT_TYPE_MUSIC);
            // LOW_LATENCY is not granted to this process anyway (docs/max.md, "Звук").
            AAudioStreamBuilder_setPerformanceMode(b, AAUDIO_PERFORMANCE_MODE_NONE);
            let mut s = std::ptr::null_mut();
            let r = AAudioStreamBuilder_openStream(b, &mut s);
            AAudioStreamBuilder_delete(b);
            check(r, "open").map(|()| s)
        }?;
        let ptr = NonNull::new(opened).ok_or("open: no stream")?;
        let mut stream = Self {
            ptr,
            channels: usize::from(channels),
        };
        let got = (
            stream.get(AAudioStream_getSampleRate),
            stream.get(AAudioStream_getChannelCount),
        );
        if got != (i32::try_from(rate).unwrap_or(0), i32::from(channels)) {
            return Err(format!("open: got {} Hz/{} ch", got.0, got.1));
        }
        check(stream.get(AAudioStream_requestStart), "start")?;
        Ok(stream)
    }

    fn get<T>(&mut self, f: unsafe extern "C" fn(*mut AAudioStream) -> T) -> T {
        // SAFETY: every `f` passed here only needs an open stream.
        unsafe { f(self.ptr.as_ptr()) }
    }

    pub fn burst_and_buffer(&mut self) -> (i32, i32) {
        (
            self.get(AAudioStream_getFramesPerBurst),
            self.get(AAudioStream_getBufferSizeInFrames),
        )
    }

    // Android 9 runs the stream on a legacy AudioTrack, whose blocking write ignores the timeout.
    pub fn write(&mut self, data: &[u8], frames: usize, timeout_ms: u32) -> Result<usize, String> {
        let frames = frames.min(data.len() / (2 * self.channels));
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
        loop {
            // SAFETY: `data` holds at least `frames` frames.
            let n = unsafe {
                AAudioStream_write(
                    self.ptr.as_ptr(),
                    data.as_ptr().cast(),
                    i32::try_from(frames).unwrap_or(i32::MAX),
                    0,
                )
            };
            check(n, "write")?;
            let now = Instant::now();
            if n > 0 || now >= deadline {
                return Ok(usize::try_from(n).unwrap_or(0));
            }
            std::thread::sleep(POLL.min(deadline - now));
        }
    }

    pub fn timestamp(&mut self) -> Option<(i64, i64)> {
        let (mut pos, mut ns) = (0, 0);
        // SAFETY: out-pointers to locals.
        let r = unsafe {
            AAudioStream_getTimestamp(self.ptr.as_ptr(), libc::CLOCK_MONOTONIC, &mut pos, &mut ns)
        };
        (r >= 0).then_some((pos, ns))
    }

    pub fn frames_written(&mut self) -> i64 {
        self.get(AAudioStream_getFramesWritten)
    }

    pub fn frames_read(&mut self) -> i64 {
        self.get(AAudioStream_getFramesRead)
    }

    pub fn xruns(&mut self) -> i32 {
        self.get(AAudioStream_getXRunCount)
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.get(AAudioStream_close);
    }
}

pub fn monotonic_ns() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: out-pointer to a local.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}
