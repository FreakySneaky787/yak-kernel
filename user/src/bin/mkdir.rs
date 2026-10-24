//! Creates directories. Usage: mkdir <dir>...

#![no_std]
#![no_main]

use ulib::{eprintln, sys};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut status = 0;
    let mut any = false;
    for dir in args {
        any = true;
        let r = sys::mkdir(dir);
        if r < 0 {
            eprintln!("mkdir: {}: {}", dir, ulib::fs::Error(-r));
            status = 1;
        }
    }
    if !any {
        eprintln!("usage: mkdir <dir>...");
        return 2;
    }
    status
}
