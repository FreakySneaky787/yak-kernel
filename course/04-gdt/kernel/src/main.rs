//! Chapter 4: the kernel of chapter 3, plus our own GDT and TSS.

#![no_std] // no standard library: it needs an operating system, and we are writing one
#![no_main]
// no `main`: nobody calls it - Limine jumps to `kmain` (see linker.ld: ENTRY(kmain))
// Tests without the standard test harness: the compiler collects `#[test_case]` functions and
// calls `testing::runner` with them from a generated function named `test_main`.
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]

mod console;
mod font;
mod gdt;
#[cfg(test)]
mod qemu;
mod serial;
#[cfg(test)]
mod testing;

use core::panic::PanicInfo;
use limine::request::FramebufferRequest;
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

/// A framebuffer: memory whose bytes are the pixels on the screen. Limine sets up a graphics mode for us.
#[used]
#[unsafe(link_section = ".limine_requests")]
static FRAMEBUFFER: FramebufferRequest = FramebufferRequest::new();

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

/// Limine jumps here: 64-bit long mode, paging on, interrupts off, on a stack Limine set up.
#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    if !BASE_REVISION.is_supported() {
        serial_println!("Limine does not support our base revision");
        halt();
    }
    console::init();
    gdt::init();
    println!("Hello from KernelTry!");
    match console::CONSOLE.lock().as_ref() {
        Some(c) => serial_println!("Console: {} x {} characters", c.size().0, c.size().1),
        None => serial_println!("No 32-bit framebuffer - text goes to the serial port only"),
    }
    println!("Grüße - umlauts work too: äöü ÄÖÜ ß");
    println!(
        "GDT loaded: code segment {:#x}, TSS {:#x}, double fault stack top {:#x}",
        gdt::KERNEL_CODE_SELECTOR,
        gdt::TSS_SELECTOR,
        gdt::double_fault_stack_top()
    );
    halt()
}

/// The entry point of the test build (`cargo test`): run every `#[test_case]`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    console::init();
    gdt::init();
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

#[test_case]
fn the_console_covers_the_real_screen() {
    let fb = FRAMEBUFFER.response().unwrap().framebuffers().first().unwrap();
    let console = console::CONSOLE.lock();
    let (columns, rows) = console.as_ref().expect("console on the framebuffer").size();
    assert_eq!((columns, rows), (fb.width as usize / 8, fb.height as usize / 16));
}
