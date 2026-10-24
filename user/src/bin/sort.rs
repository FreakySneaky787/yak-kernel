//! Prints the lines of files (or of the typed input, ended by an empty
//! line) in sorted order. Usage: sort [-r] [file...]

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ulib::fs::File;
use ulib::{eprintln, io, println};

ulib::entry!(main);

fn read_all(path: &str) -> Result<Vec<u8>, ulib::fs::Error> {
    let mut file = File::open(path)?;
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match file.read(&mut buf)? {
            0 => return Ok(data),
            n => data.extend_from_slice(&buf[..n]),
        }
    }
}

fn main(args: ulib::Args) -> i32 {
    let mut reverse = false;
    let mut lines: Vec<String> = Vec::new();
    let mut files = 0;
    for arg in args {
        if arg == "-r" {
            reverse = true;
            continue;
        }
        files += 1;
        match read_all(arg) {
            Ok(data) => lines.extend(String::from_utf8_lossy(&data).lines().map(String::from)),
            Err(e) => {
                eprintln!("sort: {}: {}", arg, e);
                return 1;
            }
        }
    }
    if files == 0 {
        let mut buf = [0u8; 256];
        loop {
            let line = io::read_line(&mut buf);
            if line.is_empty() {
                break;
            }
            lines.push(String::from(line));
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
