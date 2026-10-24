//! Prints the lines of files (or of stdin) in sorted order.
//! Usage: sort [-r] [file...]   (on the console, end the input with Ctrl+D)

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ulib::{eprintln, io, println};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let mut reverse = false;
    let mut files = Vec::new();
    for arg in args {
        match arg {
            "-r" => reverse = true,
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        files.push("-");
    }
    let mut lines: Vec<String> = Vec::new();
    for path in files {
        match io::read_input(Some(path)) {
            Ok(data) => lines.extend(String::from_utf8_lossy(&data).lines().map(String::from)),
            Err(e) => {
                eprintln!("sort: {}: {}", path, e);
                return 1;
            }
        }
    }
    lines.sort();
    if reverse {
        lines.reverse();
    }
    for line in &lines {
        println!("{}", line);
    }
    0
}
