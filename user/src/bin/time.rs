//! Runs a program and prints how long it took. Usage: time <program> [args...]

#![no_std]
#![no_main]

extern crate alloc;

use ulib::process::{self, ExitStatus};
use ulib::{eprintln, sys};

ulib::entry!(main);

fn main(mut args: ulib::Args) -> i32 {
    let Some(program) = args.next() else {
        eprintln!("usage: time <program> [args...]");
        return 2;
    };
    let path = if program.contains('/') {
        alloc::string::String::from(program)
    } else {
        alloc::format!("/bin/{}", program)
    };
    let start = sys::clock_us();
    let status = process::run(&path, args);
    let us = sys::clock_us() - start;
    // stderr: the measurement must not end up in a pipe with the program's output
    eprintln!("real {}.{:06} s", us / 1_000_000, us % 1_000_000);
    match status {
        Ok(ExitStatus::Exited(code)) => code as i32,
        Ok(ExitStatus::Killed(reason, _)) => {
            eprintln!("time: {} was killed: {}", program, reason);
            1
        }
        Err(e) => {
            eprintln!("time: {}: {}", path, e);
            127
        }
    }
}
