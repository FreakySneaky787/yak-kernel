//! Formatted output and line input on top of `sys`.

use crate::sys;
use core::fmt;

pub struct Stdout;
pub struct Stderr;

impl fmt::Write for Stdout {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys::write(sys::STDOUT, s.as_bytes());
        Ok(())
    }
}

impl fmt::Write for Stderr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        sys::write(sys::STDERR, s.as_bytes());
        Ok(())
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { { use core::fmt::Write; let _ = write!($crate::io::Stdout, $($arg)*); } };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { { use core::fmt::Write; let _ = writeln!($crate::io::Stdout, $($arg)*); } };
}

#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => { { use core::fmt::Write; let _ = writeln!($crate::io::Stderr, $($arg)*); } };
}

/// Reads one line into `buf` with echo and backspace handling (the kernel
/// delivers raw key bytes). Returns the line without the newline.
pub fn read_line(buf: &mut [u8]) -> &str {
    let mut len = 0;
    loop {
        let mut byte = [0u8; 1];
        if sys::read(sys::STDIN, &mut byte) <= 0 {
            break;
        }
        match byte[0] {
            b'\n' => {
                sys::write(sys::STDOUT, b"\n");
                break;
            }
            0x08 => {
                if len > 0 {
                    // drop the whole UTF-8 sequence of the last character
                    len -= 1;
                    while len > 0 && buf[len] & 0xC0 == 0x80 {
                        len -= 1;
                    }
                    sys::write(sys::STDOUT, b"\x08 \x08");
                }
            }
            b if b >= 0x20 && len < buf.len() => {
                buf[len] = b;
                len += 1;
                // echo once the character is complete
                if b < 0x80 {
                    sys::write(sys::STDOUT, &byte);
                } else if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                    let start = s.char_indices().last().map_or(0, |(i, _)| i);
                    sys::write(sys::STDOUT, &buf[start..len]);
                }
            }
            _ => {}
        }
    }
    // a cut-off multi-byte character at the end is dropped
    match core::str::from_utf8(&buf[..len]) {
        Ok(s) => s,
        Err(e) => unsafe { core::str::from_utf8_unchecked(&buf[..e.valid_up_to()]) },
    }
}
