#![no_std]
#![no_main]

use core::panic::PanicInfo;
use kernel::mem::{heap, pmm, stack};
use kernel::{arch, println};

/// Entry point - Limine jumps here (see `ENTRY(kmain)` in linker.ld).
/// We are already in 64-bit long mode, paging is enabled, interrupts are off.
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    kernel::init();
    // Leave Limine's stack (no guard page) for our own guarded kernel stack
    let stack = stack::alloc_stack(stack::DEFAULT_STACK_PAGES).expect("cannot allocate the kernel stack");
    unsafe { arch::x86_64::switch_stack(stack.top.as_u64(), kernel_main) }
}

/// Runs on the guarded kernel stack.
extern "C" fn kernel_main() -> ! {
    let mem = pmm::stats();
    let heap = heap::stats();
    println!(
        "\x1b[1;96mKernelTry\x1b[0m {} - a hobby kernel in Rust",
        env!("CARGO_PKG_VERSION")
    );
    println!(
        "Memory: {} MiB usable, heap {} KiB | keyboard layout: {:?} | boot took {} ms",
        mem.usable_bytes() / (1024 * 1024),
        heap.total / 1024,
        kernel::drivers::keyboard::layout(),
        kernel::time::uptime_ms()
    );
    println!("Type 'help' for a list of commands.\n");
    kernel::shell::Shell::new().run()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic_report(info)
}
