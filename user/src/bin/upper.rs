//! Reads one line from the keyboard / serial port and prints it in upper case.

#![no_std]
#![no_main]

use ulib::{io, print, println};

ulib::entry!(main);

fn main(_args: &str) -> i32 {
    print!("Type a line: ");
    let mut buf = [0u8; 256];
    let line = io::read_line(&mut buf);
    for c in line.chars().flat_map(char::to_uppercase) {
        print!("{}", c);
    }
    println!();
    0
}
