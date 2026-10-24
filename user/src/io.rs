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

/// Reads one line into `buf` and returns it without the newline; `None` at
/// the end of the input (end of a file/pipe, or Ctrl+D at the start of a line
/// on the console). On the console the typed characters are echoed (to
/// stderr, so the echo stays on the screen even if stdout is redirected) and
/// backspace works; from a file or pipe the bytes are taken as they are.
pub fn read_line(buf: &mut [u8]) -> Option<&str> {
    let tty = sys::isatty(sys::STDIN);
    let echo = |bytes: &[u8]| {
        if tty {
            sys::write(sys::STDERR, bytes);
        }
    };
    let mut len = 0;
    loop {
        let mut byte = [0u8; 1];
        if sys::read(sys::STDIN, &mut byte) <= 0 {
            if len == 0 {
                return None;
            }
            break;
        }
        match byte[0] {
            b'\n' => {
                echo(b"\n");
                break;
            }
            0x04 if tty && len == 0 => return None, // Ctrl+D
            0x08 | 0x7F if tty => {
                if len > 0 {
                    // drop the whole UTF-8 sequence of the last character
                    len -= 1;
                    while len > 0 && buf[len] & 0xC0 == 0x80 {
                        len -= 1;
                    }
                    echo(b"\x08 \x08");
                }
            }
            b if (b >= 0x20 || b == b'\t' || !tty) && len < buf.len() => {
                buf[len] = b;
                len += 1;
                // echo once the character is complete
                if b < 0x80 {
                    echo(&byte);
                } else if let Ok(s) = core::str::from_utf8(&buf[..len]) {
                    let start = s.char_indices().last().map_or(0, |(i, _)| i);
                    echo(&buf[start..len]);
                }
            }
            _ => {}
        }
    }
    // a cut-off multi-byte character at the end is dropped
    Some(match core::str::from_utf8(&buf[..len]) {
        Ok(s) => s,
        Err(e) => unsafe { core::str::from_utf8_unchecked(&buf[..e.valid_up_to()]) },
    })
}

/// Reads `fd` until the end of the file/pipe.
pub fn read_all(fd: u64) -> alloc::vec::Vec<u8> {
    let mut data = alloc::vec::Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(fd, &mut buf) {
            n if n > 0 => data.extend_from_slice(&buf[..n as usize]),
            _ => return data,
        }
    }
}

/// All of stdin: from a pipe/file until its end, from the console line by
/// line until Ctrl+D.
pub fn read_stdin() -> alloc::vec::Vec<u8> {
    if !sys::isatty(sys::STDIN) {
        return read_all(sys::STDIN);
    }
    let mut data = alloc::vec::Vec::new();
    let mut buf = [0u8; 512];
    while let Some(line) = read_line(&mut buf) {
        data.extend_from_slice(line.as_bytes());
        data.push(b'\n');
    }
    data
}

/// The contents of `path`, or of stdin for `None` / `"-"`.
pub fn read_input(path: Option<&str>) -> Result<alloc::vec::Vec<u8>, crate::fs::Error> {
    match path {
        None | Some("-") => Ok(read_stdin()),
        Some(p) => {
            let file = crate::fs::File::open(p)?;
            Ok(read_all(file.fd()))
        }
    }
}
