//! Memory usage of the system (physical memory, kernel heap).

#![no_std]
#![no_main]

use ulib::{println, sys};

ulib::entry!(main);

fn main(_args: ulib::Args) -> i32 {
    const MIB: u64 = 1024 * 1024;
    let i = sys::sysinfo();
    println!(
        "Physical: {} MiB usable, {} MiB used, {} MiB free ({} frames of 4 KiB free)",
        i.mem_usable / MIB,
        (i.mem_usable - i.mem_free) / MIB,
        i.mem_free / MIB,
        i.mem_free / 4096
    );
    println!(
        "Heap:     {} KiB mapped, {} bytes used (kernel heap)",
        i.heap_total / 1024,
        i.heap_used
    );
    0
}
