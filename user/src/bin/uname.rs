//! Prints the system name. Usage: uname
//! (The example program of docs/HACKING.md: it uses syscall 52, `uname`.)

#![no_std]
#![no_main]

use ulib::{eprintln, println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    let mut buf = [0u8; 64];
    let n = sys::uname(&mut buf);
    if n < 0 {
        eprintln!("uname: {}", ulib::fs::Error(-n));
        return 1;
    }
    println!("{}", core::str::from_utf8(&buf[..n as usize]).unwrap_or("?"));
    0
}
