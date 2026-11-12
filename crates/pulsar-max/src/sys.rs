use std::ffi::CStr;

pub fn system_property(name: &CStr) -> String {
    let mut buf = [0u8; libc::PROP_VALUE_MAX as usize];
    // SAFETY: `name` is NUL-terminated; bionic writes at most PROP_VALUE_MAX bytes into `buf`,
    // NUL included, and returns the length without it.
    let n = unsafe { libc::__system_property_get(name.as_ptr(), buf.as_mut_ptr().cast()) };
    let n = usize::try_from(n).unwrap_or(0).min(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}
