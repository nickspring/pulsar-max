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
