//! Chapter 8: the kernel of chapter 7, plus physical memory - Limine's memory map and a frame allocator.

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
mod keyboard;
mod mem;
mod pic;
mod pit;
mod pmm;
mod port;
#[cfg(test)]
mod qemu;
mod ringbuf;
mod scancode;
mod serial;
#[cfg(test)]
mod testing;
mod time;

use core::panic::PanicInfo;
use limine::request::{ExecutableCmdlineRequest, FramebufferRequest, HhdmRequest, MemmapRequest};
use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};
#[cfg(not(test))]
use scancode::Key;

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

/// Where Limine mapped all of physical memory (the higher-half direct map, see mem.rs).
#[used]
#[unsafe(link_section = ".limine_requests")]
static HHDM: HhdmRequest = HhdmRequest::new();

/// Which ranges of physical memory are RAM we may use, and which belong to firmware, Limine, the kernel, devices.
#[used]
#[unsafe(link_section = ".limine_requests")]
static MEMMAP: MemmapRequest = MemmapRequest::new();

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
    mem::init();
    print_memory_map();
    let info = pmm::info();
    println!(
        "Frames: {} MiB usable, {} frames free; the bitmap takes {} KiB at {:#x}",
        info.usable_frames as u64 * mem::PAGE_SIZE / (1024 * 1024),
        pmm::free_frames(),
        info.bitmap.length / 1024,
        info.bitmap.base
    );
    let frame = pmm::alloc_frame_zeroed().expect("a free frame");
    let pointer = frame.as_mut_ptr::<u64>();
    unsafe { pointer.write(0x00C0_FFEE) };
    let back = unsafe { pointer.read() };
    println!(
        "Frame {:#x}, seen at {:#x} through the HHDM: wrote 0xc0ffee, read {:#x}",
        frame.0, pointer as u64, back
    );
    pmm::free_frame(frame);
    match requested_fault() {
        Some("page") => interrupts::provoke_page_fault(),
        Some("stack") => interrupts::provoke_double_fault(),
        Some(other) => println!("Unknown fault={} (try page or stack)", other),
        None => {}
    }
    interrupts::start_keyboard();
    println!(
        "Keyboard: IRQ {} on vector {} - type something (US layout)",
        interrupts::KEYBOARD_IRQ,
        pic::vector(interrupts::KEYBOARD_IRQ)
    );
    loop {
        match keyboard::read_key() {
            Key::Char(c) => print!("{}", c),
            Key::Enter => println!(),
            Key::Backspace => print!("\x08 \x08"),
        }
    }
}

/// Every entry of Limine's memory map: where, how big, what.
#[cfg(not(test))]
fn print_memory_map() {
    use limine::memmap;
    let entries = MEMMAP.response().expect("Limine gave no memory map").entries();
    println!("Memory map ({} entries):", entries.len());
    for entry in entries {
        let kind = match entry.type_ {
            memmap::MEMMAP_USABLE => "usable",
            memmap::MEMMAP_RESERVED => "reserved",
            memmap::MEMMAP_ACPI_RECLAIMABLE => "ACPI tables",
            memmap::MEMMAP_ACPI_NVS => "ACPI non-volatile",
            memmap::MEMMAP_BAD_MEMORY => "bad memory",
            memmap::MEMMAP_BOOTLOADER_RECLAIMABLE => "Limine (page tables, our stack, the answers)",
            memmap::MEMMAP_EXECUTABLE_AND_MODULES => "our kernel",
            memmap::MEMMAP_FRAMEBUFFER => "framebuffer",
            memmap::MEMMAP_MAPPED_RESERVED => "reserved, but in the HHDM",
            _ => "other",
        };
        println!(
            "  {:#012x} - {:#012x} {:>8} KiB  {}",
            entry.base,
            entry.base + entry.length,
            entry.length / 1024,
            kind
        );
    }
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
    interrupts::start_keyboard();
    mem::init();
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
