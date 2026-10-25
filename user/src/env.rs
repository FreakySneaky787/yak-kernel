//! Environment variables of this process (kept by the kernel, inherited by
//! child processes).

use crate::sys;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// The value of `name`, if it is set.
pub fn var(name: &str) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    let n = sys::getenv(name, &mut buf);
    (n >= 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

pub fn set_var(name: &str, value: &str) -> bool {
    sys::setenv(name, Some(value)) == 0
}

pub fn remove_var(name: &str) {
    sys::setenv(name, None);
}

/// All variables as (name, value), sorted by name.
pub fn vars() -> Vec<(String, String)> {
    let mut buf = vec![0u8; 64 * 1024];
    let n = sys::environ(&mut buf);
    if n <= 0 {
        return Vec::new();
    }
    String::from_utf8_lossy(&buf[..n as usize])
        .split('\0')
        .filter_map(|e| e.split_once('='))
        .map(|(k, v)| (String::from(k), String::from(v)))
        .collect()
}
