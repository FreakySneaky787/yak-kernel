//! Misbehaves on purpose, to show that the kernel only kills the process.
//! Usage: fault null | kernel | cli | stack | panic | exit <code>

#![no_std]
#![no_main]

use core::hint::black_box;
use ulib::{eprintln, println};

ulib::entry!(main);

fn recurse(depth: u64) -> u64 {
    if depth == u64::MAX {
        return 0; // never reached, keeps the compiler happy
    }
    let frame = black_box([depth; 64]); // 512 bytes per call
    recurse(depth + 1) + frame[0]
}

fn main(args: &str) -> i32 {
    let mut words = args.split_whitespace();
    match words.next() {
        Some("null") => {
            println!("writing to address 0 ...");
            unsafe { (black_box(0usize) as *mut u64).write_volatile(1) };
        }
        Some("kernel") => {
            println!("reading kernel memory ...");
            let value = unsafe { (0xFFFF_FFFF_8000_0000usize as *const u64).read_volatile() };
            println!("read {:#x} - this must never happen", value);
        }
        Some("cli") => {
            println!("disabling interrupts ...");
            unsafe { core::arch::asm!("cli") };
        }
        Some("stack") => {
            println!("recursing forever ...");
            black_box(recurse(0));
        }
        Some("panic") => panic!("fault was asked to panic"),
        Some("exit") => return words.next().and_then(|c| c.parse().ok()).unwrap_or(1),
        _ => eprintln!("usage: fault null | kernel | cli | stack | panic | exit <code>"),
    }
    1
}
