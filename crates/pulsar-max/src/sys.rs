use std::ffi::CStr;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};

pub fn system_property(name: &CStr) -> String {
    let mut buf = [0u8; libc::PROP_VALUE_MAX as usize];
    // SAFETY: NUL-terminated name; bionic writes at most PROP_VALUE_MAX bytes into buf.
    let n = unsafe { libc::__system_property_get(name.as_ptr(), buf.as_mut_ptr().cast()) };
    let n = usize::try_from(n).unwrap_or(0).min(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

pub fn kill(pid: i32) -> io::Result<()> {
    // SAFETY: integer arguments only.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn exit_now(status: i32) -> ! {
    // SAFETY: _exit(2) takes an integer and does not return.
    unsafe { libc::_exit(status) }
}

// <linux/input.h>: EVIOCGRAB = _IOW('E', 0x90, int); EVIOCGSW(len) = _IOC(_IOC_READ, 'E', 0x1b, len).
const EVIOCGRAB: u32 = 0x4004_4590;
const SW_BYTES: usize = 8;
const EVIOCGSW: u32 = 0x8000_451b | ((SW_BYTES as u32) << 16);

pub fn evdev_grab(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: EVIOCGRAB takes an int by value; the fd is borrowed and open.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), EVIOCGRAB as libc::c_int, 1 as libc::c_int) };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn evdev_switch_on(fd: BorrowedFd<'_>, code: u16) -> io::Result<bool> {
    let mut bits = [0u8; SW_BYTES];
    // SAFETY: EVIOCGSW writes at most SW_BYTES bytes, the length encoded in the request.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), EVIOCGSW as libc::c_int, bits.as_mut_ptr()) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    let byte = bits.get(usize::from(code / 8)).copied().unwrap_or(0);
    Ok(byte & (1 << (code % 8)) != 0)
}

pub fn wait_readable(fd: BorrowedFd<'_>, timeout_ms: i32) -> io::Result<bool> {
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd for the duration of the call.
    let r = unsafe { libc::poll(&mut p, 1, timeout_ms) };
    match r {
        0 => Ok(false),
        n if n > 0 => Ok(true),
        _ => {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                Ok(false)
            } else {
                Err(e)
            }
        }
    }
}

const FBIOGET_VSCREENINFO: libc::c_int = 0x4600;
const FBIOPAN_DISPLAY: libc::c_int = 0x4606;
/// `struct fb_var_screeninfo`: 40 `__u32`, `xoffset` and `yoffset` at 4 and 5.
type FbVar = [u32; 40];

/// The LED framebuffer as io.sdk drives it. With gowin_led_device every read() and write() first
/// pushes the current memory to the panel, and write() then stores without pushing.
pub struct FbMap {
    file: std::fs::File,
    ptr: *mut u8,
    len: usize,
    var: FbVar,
}

// SAFETY: the mapping is owned by the struct and lives until Drop; access goes through &self/&mut self.
unsafe impl Send for FbMap {}

impl FbMap {
    pub fn open(path: &std::path::Path, len: usize) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let fd = file.as_raw_fd();
        let mut var: FbVar = [0; 40];
        // SAFETY: FBIOGET_VSCREENINFO writes one fb_var_screeninfo, 160 bytes, into var.
        if unsafe { libc::ioctl(fd, FBIOGET_VSCREENINFO, var.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        var[4] = 0;
        var[5] = 0;
        // SAFETY: a fresh shared mapping of an open fd; the result is checked before use.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file,
            ptr: ptr.cast(),
            len,
            var,
        })
    }

    /// No SPI traffic: io.sdk's memmove is visible here before its pan.
    pub fn snapshot(&self, out: &mut [u8]) {
        let n = out.len().min(self.len);
        // SAFETY: n <= len bytes inside the mapping; out does not alias it.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr, out.as_mut_ptr(), n) };
    }

    pub fn differs(&self, expected: &[u8]) -> bool {
        let n = expected.len().min(self.len);
        let words = n / 8;
        let base = self.ptr.cast::<u64>();
        for i in 0..words {
            let want =
                u64::from_ne_bytes(expected[i * 8..i * 8 + 8].try_into().unwrap_or_default());
            // SAFETY: the mapping is page aligned and i * 8 + 8 <= n <= len; volatile because
            // io.sdk writes it from another process.
            if unsafe { base.add(i).read_volatile() } != want {
                return true;
            }
        }
        (words * 8..n).any(|i| {
            // SAFETY: i < n <= len.
            let b = unsafe { self.ptr.add(i).read_volatile() };
            b != expected[i]
        })
    }

    /// Ours goes back into memory before anything else, so io.sdk's pending pan may push it.
    pub fn take_over(&mut self, foreign: &mut [u8], ours: &[u8]) {
        self.snapshot(foreign);
        self.store(ours);
    }

    pub fn store(&mut self, frame: &[u8]) {
        let n = frame.len().min(self.len);
        // SAFETY: n <= len bytes inside the mapping; frame does not alias it.
        unsafe { std::ptr::copy_nonoverlapping(frame.as_ptr(), self.ptr, n) };
    }

    pub fn show(&mut self, frame: &[u8]) -> io::Result<()> {
        self.store(frame);
        self.pan()
    }

    /// Holds the kernel's fb_info lock for the whole SPI push, about 2 ms.
    pub fn pan(&self) -> io::Result<()> {
        // SAFETY: FBIOPAN_DISPLAY reads one fb_var_screeninfo from var.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), FBIOPAN_DISPLAY, self.var.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for FbMap {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in open.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

/// Lowest real-time priority: above every normal thread, below Android's audio threads.
pub fn realtime_lowest() -> io::Result<()> {
    let p = libc::sched_param { sched_priority: 1 };
    // SAFETY: pid 0 is the calling thread; p lives for the call.
    if unsafe { libc::sched_setscheduler(0, libc::SCHED_FIFO, &p) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
