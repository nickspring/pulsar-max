use std::ffi::CStr;
use std::io;

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
