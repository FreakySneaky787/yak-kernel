//! Creates empty files (existing files are left alone). Usage: touch <file>...

#![no_std]
#![no_main]

use ulib::{eprintln, sys};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut status = 0;
    for path in args {
        // WRITE + CREATE without TRUNCATE: creates a missing file, keeps an existing one
        let fd = sys::open(path, sys::O_WRITE | sys::O_CREATE);
        if fd < 0 {
            eprintln!("touch: {}: {}", path, ulib::fs::Error(-fd));
            status = 1;
        } else {
            sys::close(fd as u64);
        }
    }
    status
}
