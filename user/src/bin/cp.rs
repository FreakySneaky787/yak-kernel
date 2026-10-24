//! Copies a file. Usage: cp <source> <destination>

#![no_std]
#![no_main]

use ulib::eprintln;
use ulib::fs::File;

ulib::entry!(main);

fn main(mut words: ulib::Args) -> i32 {
    let (Some(src), Some(dst), None) = (words.next(), words.next(), words.next()) else {
        eprintln!("usage: cp <source> <destination>");
        return 2;
    };
    let mut from = match File::open(src) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cp: {}: {}", src, e);
            return 1;
        }
    };
    let mut to = match File::create(dst) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cp: {}: {}", dst, e);
            return 1;
        }
    };
    let mut buf = [0u8; 4096];
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return 0,
            Ok(n) => n,
            Err(e) => {
                eprintln!("cp: {}: {}", src, e);
                return 1;
            }
        };
        if let Err(e) = to.write(&buf[..n]) {
            eprintln!("cp: {}: {}", dst, e);
            return 1;
        }
    }
}
