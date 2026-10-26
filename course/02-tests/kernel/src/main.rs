//! Chapter 2: the kernel of chapter 1, plus unit tests that run inside it (`cargo test`).

#![no_std] // no standard library: it needs an operating system, and we are writing one
#![no_main]
// no `main`: nobody calls it - Limine jumps to `kmain` (see linker.ld: ENTRY(kmain))
// Tests without the standard test harness: the compiler collects `#[test_case]` functions and
// calls `testing::runner` with them from a generated function named `test_main`.
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

#[cfg(test)]
mod qemu;
mod serial;
#[cfg(test)]
mod testing;

use core::panic::PanicInfo;
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};

// Requests: static structures Limine looks for in the kernel file before it starts it.
// They must stay in the binary even though no code reads them (`#[used]`) and lie between
// the start and end markers in their own section (see linker.ld).
#[used]
#[unsafe(link_section = ".limine_requests_start")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

/// Which version of the boot protocol we speak. Limine sets a field in it to 0 if it supports it.
#[used]
#[unsafe(link_section = ".limine_requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

/// Limine jumps here: 64-bit long mode, paging on, interrupts off, on a stack Limine set up.
#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    serial_println!("Hello from KernelTry!");
    if !BASE_REVISION.is_supported() {
        serial_println!("Limine does not support our base revision");
        halt();
    }
    serial_println!("Limine base revision {} is supported.", BaseRevision::MAX_SUPPORTED);
    halt()
}

/// The entry point of the test build (`cargo test`): run every `#[test_case]`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    test_main();
    halt() // not reached: the runner ends QEMU
}

/// Stops the CPU for good: interrupts off, then `hlt` (wakes only for interrupts we no longer take).
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("cli", "hlt") };
    }
}

/// Called by Rust when something panics. There is nobody to return to: report and stop.
#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("KERNEL PANIC: {}", info);
    halt()
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    testing::panic_handler(info)
}

#[test_case]
fn limine_supports_our_base_revision() {
    assert!(BASE_REVISION.is_supported());
}
