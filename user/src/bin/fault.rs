//! Misbehaves on purpose, to show that the kernel only kills the process.
//! Usage: fault null | kernel | cli | stack | panic | oom | spin | sleep | exit <code>

#![no_std]
#![no_main]

extern crate alloc;

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

fn main(mut words: ulib::Args) -> i32 {
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
        Some("oom") => {
            // Fill the heap (64 MiB) down to the last byte, then ask for more: the allocation
            // fails and Rust panics. The panic handler must report it without the heap (bug #19).
            println!("filling the heap ...");
            let mut size = 1usize << 20;
            while size >= 16 {
                let layout = core::alloc::Layout::from_size_align(size, 8).unwrap();
                // black_box: LLVM may otherwise drop the unused allocation and assume success
                if black_box(unsafe { alloc::alloc::alloc(layout) }).is_null() {
                    size /= 2; // the rest no longer fits: try smaller blocks
                }
            }
            black_box(alloc::vec![0u8; 1 << 20]);
        }
        Some("spin") => {
            println!("spinning forever (Ctrl+C stops me) ...");
            loop {
                core::hint::spin_loop();
            }
        }
        Some("sleep") => {
            println!("sleeping forever (Ctrl+C stops me) ...");
            loop {
                ulib::sys::sleep_ms(1000);
            }
        }
        Some("exit") => return words.next().and_then(|c| c.parse().ok()).unwrap_or(1),
        _ => eprintln!("usage: fault null | kernel | cli | stack | panic | spin | sleep | exit <code>"),
    }
    1
}
