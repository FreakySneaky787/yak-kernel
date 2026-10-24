//! The current date and time from the CMOS clock.

#![no_std]
#![no_main]

use ulib::fs::Timestamp;
use ulib::{println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    println!("{}", Timestamp(sys::sysinfo().date));
    0
}
