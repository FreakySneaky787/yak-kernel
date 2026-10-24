//! Time since boot.

#![no_std]
#![no_main]

use ulib::{println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    let i = sys::sysinfo();
    let ms = i.uptime_ms;
    println!(
        "up {:02}:{:02}:{:02}.{:03} ({} ticks at {} Hz, {} CPU(s))",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000,
        i.ticks,
        i.timer_hz,
        i.cpus
    );
    0
}
