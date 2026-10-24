//! Prints files, or stdin when no file (or "-") is given.
//! Usage: cat [file...]

#![no_std]
#![no_main]

extern crate alloc;

use ulib::fs::File;
use ulib::{eprintln, io, sys};

ulib::entry!(main);

/// Copies `fd` to stdout; returns the last byte written.
fn copy(fd: u64, name: &str) -> Result<Option<u8>, ()> {
    let mut buf = [0u8; 4096];
    let mut last = None;
    loop {
        match sys::read(fd, &mut buf) {
            0 => return Ok(last),
            n if n > 0 => {
                let n = n as usize;
                if sys::write(sys::STDOUT, &buf[..n]) < 0 {
                    return Err(()); // e.g. the reader of a pipe is gone
                }
                last = Some(buf[n - 1]);
            }
            e => {
                eprintln!("cat: {}: {}", name, ulib::fs::Error(-e));
                return Err(());
            }
        }
    }
}

fn main(args: ulib::Args) -> i32 {
    let mut status = 0;
    let mut paths: alloc::vec::Vec<&str> = args.collect();
    if paths.is_empty() {
        paths.push("-");
    }
    for path in paths {
        let last = if path == "-" {
            if sys::isatty(sys::STDIN) {
                // console: line by line until Ctrl+D
                let mut buf = [0u8; 512];
                while let Some(line) = io::read_line(&mut buf) {
                    sys::write(sys::STDOUT, line.as_bytes());
                    sys::write(sys::STDOUT, b"\n");
                }
                Ok(None)
            } else {
                copy(sys::STDIN, "stdin")
            }
        } else {
            match File::open(path) {
                Ok(f) => copy(f.fd(), path),
                Err(e) => {
                    eprintln!("cat: {}: {}", path, e);
                    Err(())
                }
            }
        };
        match last {
            // finish a last line without newline on the console
            Ok(Some(b)) if b != b'\n' && sys::isatty(sys::STDOUT) => {
                sys::write(sys::STDOUT, b"\n");
            }
            Ok(_) => {}
            Err(()) => status = 1,
        }
    }
    status
}
