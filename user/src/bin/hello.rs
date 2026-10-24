//! The first user program: greets, shows its arguments, sleeps a bit.

#![no_std]
#![no_main]

use ulib::{println, sys};

ulib::entry!(main);

fn main(args: &str) -> i32 {
    println!("Hello from user space! I am process {}.", sys::getpid());
    for (i, word) in args.split_whitespace().enumerate() {
        println!("  arg {}: {}", i + 1, word);
    }
    let start = sys::uptime_ms();
    sys::sleep_ms(100);
    println!("Slept for {} ms in ring 3. Bye!", sys::uptime_ms() - start);
    0
}
