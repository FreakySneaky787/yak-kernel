//! Prints files. Usage: cat <file>...

#![no_std]
#![no_main]

use ulib::fs::File;
use ulib::{eprintln, sys};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut status = 0;
    if args.clone().next().is_none() {
        eprintln!("usage: cat <file>...");
        return 2;
    }
    for path in args {
        let mut file = match File::open(path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("cat: {}: {}", path, e);
                status = 1;
                continue;
            }
        };
        let mut buf = [0u8; 4096];
        let mut last = b'\n';
        loop {
            match file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    sys::write(sys::STDOUT, &buf[..n]);
                    last = buf[n - 1];
                }
                Err(e) => {
                    eprintln!("cat: {}: {}", path, e);
                    status = 1;
                    break;
                }
            }
        }
        if last != b'\n' {
            sys::write(sys::STDOUT, b"\n");
        }
    }
    status
}
