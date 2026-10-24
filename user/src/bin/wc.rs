//! Counts lines, words and bytes of files or stdin.
//! Usage: wc [file...]

#![no_std]
#![no_main]

extern crate alloc;

use ulib::{eprintln, io, println};

ulib::entry!(main);

fn count(data: &[u8]) -> (usize, usize, usize) {
    let text = alloc::string::String::from_utf8_lossy(data);
    (
        data.iter().filter(|&&b| b == b'\n').count(),
        text.split_whitespace().count(),
        data.len(),
    )
}

fn main(args: ulib::Args) -> i32 {
    let mut paths: alloc::vec::Vec<&str> = args.collect();
    let named = !paths.is_empty();
    if !named {
        paths.push("-");
    }
    let mut status = 0;
    let mut total = (0, 0, 0);
    for path in &paths {
        match io::read_input(Some(path)) {
            Ok(data) => {
                let (l, w, b) = count(&data);
                total = (total.0 + l, total.1 + w, total.2 + b);
                if named {
                    println!("{:>7} {:>7} {:>7} {}", l, w, b, path);
                } else {
                    println!("{:>7} {:>7} {:>7}", l, w, b);
                }
            }
            Err(e) => {
                eprintln!("wc: {}: {}", path, e);
                status = 1;
            }
        }
    }
    if paths.len() > 1 {
        println!("{:>7} {:>7} {:>7} total", total.0, total.1, total.2);
    }
    status
}
