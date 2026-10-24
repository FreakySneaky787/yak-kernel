//! Lists a directory (default: the current one) or shows a single file.
//! Usage: ls [path]

#![no_std]
#![no_main]

use ulib::fs::{self, Dir, Timestamp};
use ulib::{eprintln, println};

ulib::entry!(main);

fn show(name: &str, st: &ulib::sys::Stat) {
    let time = Timestamp(st.modified);
    if st.kind == ulib::sys::KIND_DIR {
        println!("  {:>10}  {}  \x1b[94m{}/\x1b[0m", "<DIR>", time, name);
    } else {
        println!("  {:>10}  {}  {}", st.size, time, name);
    }
}

fn main(mut args: ulib::Args) -> i32 {
    let path = args.next().unwrap_or(".");
    let st = match fs::stat(path) {
        Ok(st) => st,
        Err(e) => {
            eprintln!("ls: {}: {}", path, e);
            return 1;
        }
    };
    if st.kind != ulib::sys::KIND_DIR {
        show(path.rsplit('/').next().unwrap_or(path), &st);
        return 0;
    }
    let dir = match Dir::open(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ls: {}: {}", path, e);
            return 1;
        }
    };
    for entry in dir {
        match entry {
            Ok(e) => show(e.name(), e.stat()),
            Err(e) => {
                eprintln!("ls: {}: {}", path, e);
                return 1;
            }
        }
    }
    0
}
