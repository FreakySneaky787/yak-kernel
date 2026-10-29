//! Chapter 6: the kernel of chapter 5, plus hardware interrupts - the PIC, the PIT timer and time.

#![no_std] // no standard library: it needs an operating system, and we are writing one
#![no_main]
// no `main`: nobody calls it - Limine jumps to `kmain` (see linker.ld: ENTRY(kmain))
// Tests without the standard test harness: the compiler collects `#[test_case]` functions and
// calls `testing::runner` with them from a generated function named `test_main`.
#![feature(custom_test_frameworks)]
#![test_runner(crate::testing::runner)]
#![reexport_test_harness_main = "test_main"]
// `extern "x86-interrupt" fn`: functions the CPU calls, not Rust code (they save every register and end with `iretq`)
#![feature(abi_x86_interrupt)]

mod console;
mod font;
mod gdt;
mod idt;
mod interrupts;
mod pic;
mod pit;
mod port;
#[cfg(test)]
mod qemu;
mod serial;
#[cfg(test)]
mod testing;
mod time;

use core::panic::PanicInfo;
use limine::request::{ExecutableCmdlineRequest, FramebufferRequest};
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

/// The command line from `limine.conf` (`cmdline: …`): `fault=page` or `fault=stack` crash the kernel on purpose.
#[used]
#[unsafe(link_section = ".limine_requests")]
static CMDLINE: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

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
    interrupts::init();
    println!("Hello from YAK!");
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
    println!(
        "IDT loaded: exceptions 0, 3, 6, 8 (on IST{}), 13, 14",
        gdt::DOUBLE_FAULT_IST
    );
    unsafe { core::arch::asm!("int3") };
    println!("Back from the breakpoint handler - the kernel carries on.");
    interrupts::start_timer();
    println!(
        "Timer: IRQ {} on vector {}, {} Hz",
        interrupts::TIMER_IRQ,
        pic::vector(0),
        time::TICKS_PER_SECOND
    );
    let before = time::uptime_ms();
    time::sleep_ms(1000);
    let slept = time::uptime_ms() - before;
    let enough = if slept >= 1000 { "yes" } else { "NO" };
    println!(
        "sleep_ms(1000) took {} ms (at least 1000: {}), the CPU waited in hlt",
        slept, enough
    );
    match requested_fault() {
        Some("page") => interrupts::provoke_page_fault(),
        Some("stack") => interrupts::provoke_double_fault(),
        Some(other) => println!("Unknown fault={} (try page or stack)", other),
        None => {}
    }
    halt()
}

/// `fault=…` from the kernel command line, if there is one.
#[cfg(not(test))]
fn requested_fault() -> Option<&'static str> {
    let cmdline = CMDLINE.response()?.cmdline();
    cmdline.split(' ').find_map(|word| word.strip_prefix("fault="))
}

/// The entry point of the test build (`cargo test`): run every `#[test_case]`.
#[cfg(test)]
#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    serial::init();
    console::init();
    gdt::init();
    interrupts::init();
    interrupts::start_timer();
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
