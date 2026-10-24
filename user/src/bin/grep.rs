//! Prints the lines that contain a text. Usage: grep [-v] [-i] <text> [file...]
//! -v: lines that do NOT contain it, -i: ignore upper/lower case (ASCII).
//! Exit code 0 if a line was printed, 1 if not, 2 on errors.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use ulib::{eprintln, io, println};

ulib::entry!(main);

fn main(args: ulib::Args) -> i32 {
    let (mut invert, mut ignore_case) = (false, false);
    let mut rest: Vec<&str> = Vec::new();
    for arg in args {
        match arg {
            "-v" if rest.is_empty() => invert = true,
            "-i" if rest.is_empty() => ignore_case = true,
            _ => rest.push(arg),
        }
    }
    let Some((&pattern, files)) = rest.split_first() else {
        eprintln!("usage: grep [-v] [-i] <text> [file...]");
        return 2;
    };
    let pattern = if ignore_case {
        pattern.to_ascii_lowercase()
    } else {
        String::from(pattern)
    };
    let files: Vec<&str> = if files.is_empty() {
        alloc::vec!["-"]
    } else {
        files.to_vec()
    };
    let mut found = false;
    for path in &files {
        let data = match io::read_input(Some(path)) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("grep: {}: {}", path, e);
                return 2;
            }
        };
        for line in String::from_utf8_lossy(&data).lines() {
            let hay = if ignore_case {
                line.to_ascii_lowercase()
            } else {
                String::from(line)
            };
            if hay.contains(pattern.as_str()) != invert {
                found = true;
                if files.len() > 1 {
                    println!("{}:{}", path, line);
                } else {
                    println!("{}", line);
                }
            }
        }
    }
    if found { 0 } else { 1 }
}
